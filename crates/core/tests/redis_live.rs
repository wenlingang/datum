//! Redis 实时(Pub/Sub、MONITOR)的真实执行。需要一个可随意写入的 Redis,默认忽略:
//!   DATUM_TEST_REDIS=端口   例如 docker run --rm -p 16379:6379 redis:7-alpine
//!   cargo test -p datum-core --test redis_live -- --ignored --nocapture
//! 连接用库 12(Pub/Sub 与库无关,MONITOR 看得到所有库的命令);频道名带随机后缀,并行跑互不干扰。

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::rds::live::{EndReason, Limits, LivePoll, MonitorFilter, Sub};
use datum_core::service;
use std::time::Duration;

async fn setup(id: &str) -> Option<AppState> {
    let port: u16 = std::env::var("DATUM_TEST_REDIS").ok()?.parse().ok()?;
    let dir = std::env::temp_dir().join(format!("datum-rds-live-{}", uuid::Uuid::new_v4()));
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
        database: "12".into(),
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

fn chan(name: &str) -> Sub {
    Sub { pattern: false, name: name.into(), active: false }
}

fn pat(name: &str) -> Sub {
    Sub { pattern: true, name: name.into(), active: false }
}

async fn poll(st: &AppState, sid: &str) -> LivePoll {
    service::redis_live_poll(st, sid, 0, 1000).await.unwrap()
}

/// 轮询直到满足条件(最多 5 秒)。
async fn wait_for(st: &AppState, sid: &str, ok: impl Fn(&LivePoll) -> bool) -> LivePoll {
    for _ in 0..100 {
        let p = poll(st, sid).await;
        if ok(&p) {
            return p;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("等不到预期状态:{:?}", poll(st, sid).await);
}

async fn client_list(st: &AppState, id: &str) -> String {
    service::redis_exec(st, id, "CLIENT LIST").await.unwrap()
}

/// 专用连接已从服务器上消失(最多等 5 秒)。
async fn wait_closed(st: &AppState, id: &str, name: &str) {
    for _ in 0..100 {
        if !client_list(st, id).await.contains(&format!("name={name} ")) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("专用连接 {name} 没有关闭");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn subscribe_and_psubscribe_receive_published_messages() {
    let Some(st) = setup("live-sub").await else { return };
    let a = uniq("datum:live:a");
    let p = uniq("datum:live:p");
    let sid = service::redis_pubsub_start(&st, "live-sub", vec![chan(&a), pat(&format!("{p}:*"))]).await.unwrap();
    wait_for(&st, &sid, |x| x.subs.len() == 2 && x.subs.iter().all(|s| s.active)).await;

    assert_eq!(service::redis_publish(&st, "live-sub", &a, "hello").await.unwrap(), 1);
    service::redis_publish(&st, "live-sub", &format!("{p}:x"), r#"{"n":1}"#).await.unwrap();
    let got = wait_for(&st, &sid, |x| x.items.len() == 2).await;
    assert_eq!(got.items[0].channel, a);
    assert_eq!(got.items[0].text, "hello");
    assert_eq!(got.items[0].pattern, None);
    assert_eq!(got.items[1].channel, format!("{p}:x"));
    assert_eq!(got.items[1].pattern.as_deref(), Some(format!("{p}:*").as_str()));
    assert_eq!(got.items[1].format, "json");
    // 增量轮询:只拿新的
    let since = got.items[1].seq;
    assert!(service::redis_live_poll(&st, &sid, since, 100).await.unwrap().items.is_empty());

    // 运行中加订阅 / 退订
    let b = uniq("datum:live:b");
    service::redis_live_subscribe(&st, &sid, &[chan(&b)]).await.unwrap();
    wait_for(&st, &sid, |x| x.subs.iter().any(|s| s.name == b && s.active)).await;
    service::redis_live_unsubscribe(&st, &sid, &[chan(&a)]).await.unwrap();
    // 退订生效后 a 没有订阅者了
    for _ in 0..100 {
        if service::redis_publish(&st, "live-sub", &a, "after").await.unwrap() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    service::redis_publish(&st, "live-sub", &b, "to-b").await.unwrap();
    let got = wait_for(&st, &sid, |x| x.items.iter().any(|i| i.text == "to-b")).await;
    assert!(!got.items.iter().any(|i| i.text == "after"));

    // 详情 / 清空
    let d = service::redis_live_detail(&st, &sid, got.items[0].seq).await.unwrap().unwrap();
    assert_eq!(d.text, "hello");
    service::redis_live_clear(&st, &sid).await.unwrap();
    let cleared = poll(&st, &sid).await;
    assert_eq!(cleared.count, 0);
    assert!(service::redis_live_detail(&st, &sid, got.items[0].seq).await.unwrap().is_none());
    service::redis_live_close(&st, &sid).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn stop_unsubscribes_and_closes_dedicated_connection() {
    let Some(st) = setup("live-stop").await else { return };
    let a = uniq("datum:live:stop");
    let sid = service::redis_pubsub_start(&st, "live-stop", vec![chan(&a)]).await.unwrap();
    wait_for(&st, &sid, |x| x.subs.iter().all(|s| s.active)).await;
    let name = st.redis_pool.live(&sid).await.unwrap().client_name.clone();
    assert!(client_list(&st, "live-stop").await.contains(&format!("name={name} ")));
    // 共享连接不受订阅影响:照常执行命令
    assert_eq!(service::redis_exec(&st, "live-stop", "PING").await.unwrap(), "PONG");

    service::redis_live_stop(&st, &sid).await;
    wait_closed(&st, "live-stop", &name).await;
    assert_eq!(service::redis_publish(&st, "live-stop", &a, "x").await.unwrap(), 0);
    // 停止后缓冲还在(看详情),关闭后才移除
    let p = poll(&st, &sid).await;
    assert!(!p.running);
    assert_eq!(p.ended, Some(EndReason::Stopped));
    service::redis_live_close(&st, &sid).await;
    assert!(service::redis_live_poll(&st, &sid, 0, 10).await.is_err());

    // 断开连接时停掉名下所有会话
    let sid = service::redis_pubsub_start(&st, "live-stop", vec![chan(&a)]).await.unwrap();
    let mon = service::redis_monitor_start(&st, "live-stop", 60, MonitorFilter::default()).await.unwrap();
    let names = [
        st.redis_pool.live(&sid).await.unwrap().client_name.clone(),
        st.redis_pool.live(&mon).await.unwrap().client_name.clone(),
    ];
    service::disconnect(&st, &DbKind::Redis, "live-stop").await;
    assert!(st.redis_pool.live(&sid).await.is_err());
    assert!(st.redis_pool.live(&mon).await.is_err());
    // 共享连接还在(Redis 断开不移除它),用它确认专用连接都关了
    for n in &names {
        wait_closed(&st, "live-stop", n).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn monitor_sees_filtered_commands_and_stops_on_time() {
    let Some(st) = setup("live-mon").await else { return };
    let k = uniq("datum:mon");
    let filter = MonitorFilter { command: "set".into(), key: k.clone() };
    let sid = service::redis_monitor_start(&st, "live-mon", 2, filter).await.unwrap();
    let name = st.redis_pool.live(&sid).await.unwrap().client_name.clone();
    service::redis_exec(&st, "live-mon", &format!("SET {k}:1 v1")).await.unwrap();
    service::redis_exec(&st, "live-mon", &format!("GET {k}:1")).await.unwrap();
    service::redis_exec(&st, "live-mon", "SET datum:mon:other v").await.unwrap();
    let got = wait_for(&st, &sid, |x| !x.items.is_empty() && x.filtered >= 2).await;
    assert_eq!(got.items.len(), 1);
    let it = &got.items[0];
    assert_eq!(it.command, "SET");
    assert!(it.text.contains(&format!("\"{k}:1\"")), "{}", it.text);
    assert_eq!(it.db, Some(12));
    assert!(it.client.contains(':'));
    assert!(got.deadline.is_some());

    // 换过滤条件:之后的 GET 也收
    service::redis_live_filter(&st, &sid, MonitorFilter { command: "get".into(), key: String::new() }).await.unwrap();
    assert_eq!(poll(&st, &sid).await.count, 0);
    service::redis_exec(&st, "live-mon", &format!("GET {k}:1")).await.unwrap();
    wait_for(&st, &sid, |x| x.items.iter().any(|i| i.command == "GET")).await;

    // 到时自动停止并关掉连接
    let done = wait_for(&st, &sid, |x| !x.running).await;
    assert_eq!(done.ended, Some(EndReason::Timeout));
    wait_closed(&st, "live-mon", &name).await;
    service::redis_exec(&st, "live-mon", &format!("DEL {k}:1 datum:mon:other")).await.unwrap();
    service::redis_live_close(&st, &sid).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn buffer_keeps_newest_and_truncates_big_messages() {
    let Some(st) = setup("live-buf").await else { return };
    let a = uniq("datum:live:buf");
    let sid = st.redis_pool.start_pubsub("live-buf", vec![chan(&a)], Limits { max_messages: 20, max_bytes: usize::MAX }).await.unwrap();
    wait_for(&st, &sid, |x| x.subs.iter().all(|s| s.active)).await;
    for i in 0..50 {
        service::redis_publish(&st, "live-buf", &a, &format!("m{i}")).await.unwrap();
    }
    let got = wait_for(&st, &sid, |x| x.received == 50).await;
    assert_eq!(got.count, 20);
    assert_eq!(got.dropped, 30);
    assert_eq!(got.first_seq, 31);
    assert_eq!(got.items.first().unwrap().text, "m30");
    assert_eq!(got.items.last().unwrap().text, "m49");

    // 2 MB 的消息:列表只带前 4 KB,详情保存前 1 MB,之后的消息照常解析(流没有错位)
    service::redis_live_clear(&st, &sid).await.unwrap();
    let big = "x".repeat(2 << 20);
    service::redis_publish(&st, "live-buf", &a, &big).await.unwrap();
    service::redis_publish(&st, "live-buf", &a, "after-big").await.unwrap();
    let got = wait_for(&st, &sid, |x| x.items.len() == 2).await;
    let it = &got.items[0];
    assert_eq!(it.size, 2 << 20);
    assert!(it.truncated);
    assert_eq!(it.text.len(), datum_core::rds::live::PREVIEW_BYTES);
    assert_eq!(got.items[1].text, "after-big");
    let d = service::redis_live_detail(&st, &sid, it.seq).await.unwrap().unwrap();
    assert_eq!(d.stored, datum_core::rds::live::MAX_STORED_BYTES);
    assert_eq!(d.size, 2 << 20);
    service::redis_live_close(&st, &sid).await;
}
