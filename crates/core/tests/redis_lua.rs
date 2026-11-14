//! Redis Lua 脚本(EVAL、SCRIPT KILL)的真实执行。需要一个可随意写入的 Redis,默认忽略:
//!   DATUM_TEST_REDIS=端口   例如 docker run --rm -p 16379:6379 redis:7-alpine
//!   cargo test -p datum-core --test redis_lua -- --ignored --nocapture
//! 连接用库 11;key 带随机后缀。死循环脚本测试无论成败都会在结尾 SCRIPT KILL,不把测试实例卡住。

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::rds::lua::{ErrorHint, EvalRequest, KillStatus, LuaValue};
use datum_core::service;
use std::sync::Arc;
use std::time::Duration;

const DB: &str = "11";

/// 死循环脚本运行期间整个实例都回 BUSY:本文件的测试串行跑,别的测试不和它撞在一起。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn port() -> Option<u16> {
    std::env::var("DATUM_TEST_REDIS").ok()?.parse().ok()
}

async fn setup(id: &str) -> Option<AppState> {
    let port = port()?;
    let dir = std::env::temp_dir().join(format!("datum-rds-lua-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let st = AppState::new(dir);
    let c = Connection {
        id: id.into(),
        name: "rds".into(),
        kind: DbKind::Redis,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        user: String::new(),
        database: DB.into(),
        plaintext_password: None,
        file_path: None,
        ssl_mode: SslMode::Disable,
        uri: None,
        auth_source: None,
        ssh: None,
    };
    commands::save_connection(&st, c, Some(String::new())).unwrap();
    service::connect(&st, &DbKind::Redis, id).await.unwrap();
    Some(st)
}

fn uniq(prefix: &str) -> String {
    format!("{prefix}:{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// 直连库 11 的客户端(核对脚本写进去的数据)。
async fn raw() -> redis::aio::MultiplexedConnection {
    let client = redis::Client::open(format!("redis://127.0.0.1:{}/{DB}", port().unwrap())).unwrap();
    client.get_multiplexed_async_connection().await.unwrap()
}

fn req(script: &str, keys: &[&str], args: &[&str]) -> EvalRequest {
    EvalRequest {
        script: script.into(),
        keys: keys.iter().map(|s| s.to_string()).collect(),
        args: args.iter().map(|s| s.to_string()).collect(),
    }
}

fn s(v: &LuaValue) -> &str {
    match v {
        LuaValue::Str { text, .. } | LuaValue::Int { text } | LuaValue::Status { text } | LuaValue::Error { text } => text,
        other => panic!("不是标量:{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn eval_reads_and_writes_in_the_configured_db() {
    let _serial = SERIAL.lock().await;
    let Some(st) = setup("lua-rw").await else { return };
    let k = uniq("datum:lua");
    // KEYS / ARGV 逐个作为参数传:带空格、空串都原样到达
    let out = service::redis_eval(
        &st,
        "lua-rw",
        "run-1",
        req(
            "redis.call('SET', KEYS[1], ARGV[1])\nreturn {redis.call('GET', KEYS[1]), #ARGV[2], 42, false, {ok='fine'}, redis.pcall('INCR', KEYS[1]), {1, {2, {3}}}}",
            &[&k],
            &["hello world", ""],
        ),
    )
    .await
    .unwrap();
    assert_eq!(out.db, 11);
    assert_eq!(out.hint, None);
    assert!(!out.truncated);
    let LuaValue::Array { items, len } = &out.value else { panic!("{:?}", out.value) };
    assert_eq!(*len, 7);
    assert_eq!(s(&items[0]), "hello world");
    assert_eq!(s(&items[1]), "0");
    assert_eq!(s(&items[2]), "42");
    assert_eq!(items[3], LuaValue::Nil);
    assert_eq!(items[4], LuaValue::Status { text: "fine".into() });
    // pcall 的错误作为数组里的错误值返回
    assert!(matches!(&items[5], LuaValue::Error { text } if text.contains("not an integer")), "{:?}", items[5]);
    let LuaValue::Array { items: nested, .. } = &items[6] else { panic!() };
    assert_eq!(nested.len(), 2);

    // 写进的是库 11(连接设置里的库)
    let mut con = raw().await;
    let v: String = redis::cmd("GET").arg(&k).query_async(&mut con).await.unwrap();
    assert_eq!(v, "hello world");
    let _: () = redis::cmd("DEL").arg(&k).query_async(&mut con).await.unwrap();

    // 执行完专用连接就关了
    let list = service::redis_exec(&st, "lua-rw", "CLIENT LIST").await.unwrap();
    for _ in 0..50 {
        if !service::redis_exec(&st, "lua-rw", "CLIENT LIST").await.unwrap().contains("name=datum-eval-") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!service::redis_exec(&st, "lua-rw", "CLIENT LIST").await.unwrap().contains("name=datum-eval-"), "{list}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn big_results_are_capped() {
    let _serial = SERIAL.lock().await;
    let Some(st) = setup("lua-big").await else { return };
    // 两千个元素,每个 2 KB:超过每层元素上限与整体字节预算
    let out = service::redis_eval(
        &st,
        "lua-big",
        "run-big",
        req("local t = {} for i = 1, 2000 do t[i] = string.rep('x', 2048) end return t", &[], &[]),
    )
    .await
    .unwrap();
    assert!(out.truncated);
    let LuaValue::Array { items, len } = &out.value else { panic!() };
    assert_eq!(*len, 2000);
    assert_eq!(items.len(), datum_core::rds::lua::MAX_ITEMS);
    // 同一个连接后续照常可用(结果读完、读走丢掉的部分没有留在网络上)
    assert_eq!(service::redis_exec(&st, "lua-big", "PING").await.unwrap(), "PONG");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn script_errors_are_classified() {
    let _serial = SERIAL.lock().await;
    let Some(st) = setup("lua-err").await else { return };
    let out = service::redis_eval(&st, "lua-err", "r1", req("return (", &[], &[])).await.unwrap();
    assert_eq!(out.hint, Some(ErrorHint::Compile), "{:?}", out.value);
    assert!(matches!(out.value, LuaValue::Error { .. }));
    let out = service::redis_eval(&st, "lua-err", "r2", req("return redis.call('NOPE_NOT_A_COMMAND')", &[], &[])).await.unwrap();
    assert!(matches!(out.value, LuaValue::Error { .. }), "{:?}", out.value);
    let out = service::redis_eval(&st, "lua-err", "r3", req("return redis.error_reply('my failure')", &[], &[])).await.unwrap();
    assert_eq!(out.value, LuaValue::Error { text: "my failure".into() });
    let out = service::redis_eval(&st, "lua-err", "r4", req("return nil_value.x", &[], &[])).await.unwrap();
    assert_eq!(out.hint, Some(ErrorHint::Runtime), "{:?}", out.value);
    // 空脚本在本地就拒绝
    assert!(service::redis_eval(&st, "lua-err", "r5", req("  ", &[], &[])).await.is_err());
    // 没有脚本在跑时 SCRIPT KILL 回 NOTBUSY
    let k = service::redis_script_kill(&st, "lua-err").await.unwrap();
    assert_eq!(k.status, KillStatus::NotBusy);
}

/// 结束测试实例上可能还在跑的脚本(只读死循环才 KILL 得掉,测试只用只读的)。
async fn kill_leftover() {
    // 用库 0 建连:不发 SELECT(脚本运行时除 AUTH / SCRIPT KILL 等少数命令外都回 BUSY)
    let client = redis::Client::open(format!("redis://127.0.0.1:{}/0", port().unwrap())).unwrap();
    for _ in 0..40 {
        let Ok(mut con) = client.get_multiplexed_async_connection().await else {
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        };
        let r: redis::RedisResult<String> = redis::cmd("SCRIPT").arg("KILL").query_async(&mut con).await;
        match r {
            Ok(_) => return,
            Err(e) if e.to_string().contains("NOTBUSY") => return,
            Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn long_script_can_be_killed_from_another_connection() {
    let _serial = SERIAL.lock().await;
    let Some(st) = setup("lua-kill").await else { return };
    let st = Arc::new(st);
    let body = {
        let st = st.clone();
        tokio::spawn(async move {
            // 只读死循环:没写过数据,SCRIPT KILL 能终止
            let run = {
                let st = st.clone();
                tokio::spawn(async move { service::redis_eval(&st, "lua-kill", "loop-1", req("while true do end", &[], &[])).await })
            };
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(!run.is_finished(), "死循环不该自己结束");
            // 另开的专用连接发 SCRIPT KILL;服务器在 busy 阈值(默认 5 秒)后才处理它
            let k = service::redis_script_kill(&st, "lua-kill").await.unwrap();
            assert_eq!(k.status, KillStatus::Killed, "{k:?}");
            let out = tokio::time::timeout(Duration::from_secs(10), run).await.expect("KILL 后 EVAL 应该很快返回").unwrap().unwrap();
            assert_eq!(out.hint, Some(ErrorHint::Killed), "{:?}", out.value);
            assert!(out.elapsed_ms >= 300);

            // 放弃等待:关掉专用连接,界面拿到「已放弃」;服务器上的脚本仍在跑,再 KILL 掉
            let run = {
                let st = st.clone();
                tokio::spawn(async move { service::redis_eval(&st, "lua-kill", "loop-2", req("while true do end", &[], &[])).await })
            };
            tokio::time::sleep(Duration::from_millis(300)).await;
            service::redis_eval_abandon(&st, "loop-2").await;
            let r = tokio::time::timeout(Duration::from_secs(2), run).await.expect("放弃后应立即返回").unwrap();
            assert!(r.is_err());
            let k = service::redis_script_kill(&st, "lua-kill").await.unwrap();
            assert_eq!(k.status, KillStatus::Killed, "{k:?}");
        })
    };
    let res = body.await;
    // 无论上面成败,都确认测试实例没有被卡住
    kill_leftover().await;
    let mut con = raw().await;
    let pong: String = redis::cmd("PING").query_async(&mut con).await.unwrap();
    assert_eq!(pong, "PONG");
    if let Err(e) = res {
        std::panic::resume_unwind(e.into_panic());
    }
}
