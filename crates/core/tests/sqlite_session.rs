//! SQLite 的长查询:只读限时会中止、可以停止,且不占 tokio 工作线程、不挡别的库。
//! 用临时文件,默认就跑。

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;
use std::sync::Arc;
use std::time::{Duration, Instant};

const LITE: DbKind = DbKind::Sqlite;
/// 跑不完的递归 CTE:静态检查判为只读(WITH 开头、没有写关键字)
const ENDLESS: &str = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c";

fn state() -> (std::path::PathBuf, Arc<AppState>) {
    let dir = std::env::temp_dir().join(format!("datum-litesess-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    (dir.clone(), Arc::new(AppState::new(dir)))
}

fn connect(st: &AppState, dir: &std::path::Path, name: &str) -> String {
    let file = dir.join(format!("{name}.db"));
    rusqlite::Connection::open(&file).unwrap().execute_batch("CREATE TABLE t (id INTEGER)").unwrap();
    let id = format!("lite-{name}");
    let c = Connection {
        id: id.clone(),
        name: name.into(),
        kind: LITE,
        env: Env::Local,
        host: String::new(),
        port: 0,
        user: String::new(),
        database: String::new(),
        plaintext_password: None,
        file_path: Some(file.to_string_lossy().into_owned()),
        ssl_mode: SslMode::Disable,
        ssh: None,
    };
    commands::save_connection(st, c, None).unwrap();
    id
}

async fn select_one(st: &AppState, id: &str) {
    let r = service::query(st, &LITE, id, "SELECT 1", None).await.unwrap();
    assert_eq!(r.rows[0][0].as_deref(), Some("1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_timeout_aborts_endless_query() {
    let (dir, st) = state();
    let id = connect(&st, &dir, "a");
    service::connect(&st, &LITE, &id).await.unwrap();
    let t0 = Instant::now();
    let err = service::query_read_only_timeout(&st, &LITE, &id, ENDLESS, 10, 500).await.unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(3), "没有按时中止 {:?}", t0.elapsed());
    assert!(err.display().contains('1'), "应报超时(1 秒内):{}", err.display());
    // 连接照常可用,且已恢复可写
    select_one(&st, &id).await;
    service::query(&st, &LITE, &id, "INSERT INTO t VALUES (1)", None).await.unwrap();
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_interrupts_running_query() {
    let (dir, st) = state();
    let id = connect(&st, &dir, "a");
    service::connect(&st, &LITE, &id).await.unwrap();
    // 空闲时没有可停的
    assert!(!service::cancel_query(&st, &LITE, &id).await.unwrap());
    for read_only in [false, true] {
        let running = {
            let (st, id) = (st.clone(), id.clone());
            tokio::spawn(async move {
                if read_only {
                    service::query_read_only(&st, &LITE, &id, ENDLESS, None).await
                } else {
                    service::query(&st, &LITE, &id, ENDLESS, None).await
                }
            })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(service::cancel_query(&st, &LITE, &id).await.unwrap());
        let r = tokio::time::timeout(Duration::from_secs(3), running).await.expect("停止后语句没有结束").unwrap();
        assert!(r.is_err());
        select_one(&st, &id).await;
    }
    // 只读执行被停后已恢复可写
    service::query(&st, &LITE, &id, "INSERT INTO t VALUES (1)", None).await.unwrap();
    std::fs::remove_dir_all(dir).ok();
}

/// 单线程运行时:长查询若同步跑在工作线程上,别的任务全都动不了。
#[tokio::test(flavor = "current_thread")]
async fn long_query_does_not_block_runtime_or_other_databases() {
    let (dir, st) = state();
    let a = connect(&st, &dir, "a");
    let b = connect(&st, &dir, "b");
    service::connect(&st, &LITE, &a).await.unwrap();
    service::connect(&st, &LITE, &b).await.unwrap();
    let running = {
        let (st, a) = (st.clone(), a.clone());
        tokio::spawn(async move { service::query(&st, &LITE, &a, ENDLESS, None).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::time::timeout(Duration::from_secs(1), select_one(&st, &b)).await.expect("别的库被长查询挡住了");
    assert!(service::cancel_query(&st, &LITE, &a).await.unwrap());
    tokio::time::timeout(Duration::from_secs(3), running).await.expect("停止后语句没有结束").unwrap().unwrap_err();
    std::fs::remove_dir_all(dir).ok();
}
