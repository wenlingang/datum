//! 停止正在执行的查询(编辑器的「停止」):服务器端真的中止语句,连接之后照常可用。
//! 需要本机数据库,默认忽略:
//!   DATUM_TEST_PG=端口      PG(用户 postgres、库 postgres、空密码)
//!   DATUM_TEST_MYSQL=端口   MySQL(用户 root、空密码、库 datum_test)
//!   cargo test -p datum-core --test cancel -- --include-ignored

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn state() -> Arc<AppState> {
    let dir = std::env::temp_dir().join(format!("datum-cancel-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    Arc::new(AppState::new(dir))
}

async fn connect(st: &AppState, kind: DbKind, env: &str, user: &str, db: &str) -> String {
    let port: u16 = std::env::var(env).unwrap_or_else(|_| panic!("{env}=端口")).parse().unwrap();
    let id = format!("cancel-{}", uuid::Uuid::new_v4());
    let c = Connection {
        id: id.clone(),
        name: "cancel".into(),
        kind,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        user: user.into(),
        database: db.into(),
        plaintext_password: None,
        file_path: None,
        ssl_mode: SslMode::Disable,
        ssh: None,
    };
    commands::save_connection(st, c, Some(String::new())).unwrap();
    service::connect(st, &kind, &id).await.unwrap();
    id
}

/// 起一条慢语句,稍后停止:应在 2 秒内返回(不等满 10 秒),之后连接照常可用。
async fn stops_slow_query(st: Arc<AppState>, kind: DbKind, id: String, slow: &'static str, read_only: bool) {
    let started = Instant::now();
    let running = {
        let (st, id) = (st.clone(), id.clone());
        tokio::spawn(async move {
            if read_only {
                service::query_read_only(&st, &kind, &id, slow, None).await
            } else {
                service::query(&st, &kind, &id, slow, None).await
            }
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(service::cancel_query(&st, &kind, &id).await.unwrap(), "应发出了取消");
    let _ = tokio::time::timeout(Duration::from_secs(3), running).await.expect("停止后语句没有结束");
    assert!(started.elapsed() < Duration::from_secs(4), "语句没有被中止 {:?}", started.elapsed());
    let r = service::query(&st, &kind, &id, "SELECT 1", None).await.unwrap();
    assert_eq!(r.rows[0][0].as_deref(), Some("1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn pg_stop() {
    let st = state();
    let id = connect(&st, DbKind::Pg, "DATUM_TEST_PG", "postgres", "postgres").await;
    stops_slow_query(st.clone(), DbKind::Pg, id.clone(), "SELECT pg_sleep(10)", false).await;
    // 免确认执行(只读会话)同样能停,停完会话恢复读写
    stops_slow_query(st.clone(), DbKind::Pg, id.clone(), "SELECT pg_sleep(10)", true).await;
    let r = service::query(&st, &DbKind::Pg, &id, "SHOW transaction_read_only", None).await.unwrap();
    assert_eq!(r.rows[0][0].as_deref(), Some("off"));
    // 空闲时停止:请求被服务器忽略,不影响下一条语句
    service::cancel_query(&st, &DbKind::Pg, &id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    service::query(&st, &DbKind::Pg, &id, "SELECT pg_sleep(0.3)", None).await.unwrap();
    // 停止时元数据(辅助会话)不受影响
    service::list_objects(&st, &DbKind::Pg, &id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn mysql_stop() {
    let st = state();
    let id = connect(&st, DbKind::Mysql, "DATUM_TEST_MYSQL", "root", "datum_test").await;
    // SLEEP 被 KILL QUERY 打断时返回 1 而不是报错;只看是否提前结束
    stops_slow_query(st.clone(), DbKind::Mysql, id.clone(), "SELECT SLEEP(10)", false).await;
    stops_slow_query(st.clone(), DbKind::Mysql, id.clone(), "SELECT SLEEP(10)", true).await;
    // 空闲时没有可停的语句
    assert!(!service::cancel_query(&st, &DbKind::Mysql, &id).await.unwrap());
    // 只读执行被停后,连接归还池前已恢复读写
    for _ in 0..5 {
        service::query(&st, &DbKind::Mysql, &id, "CREATE TABLE IF NOT EXISTS cancel_t (id INT)", None).await.unwrap();
    }
    service::query(&st, &DbKind::Mysql, &id, "DROP TABLE cancel_t", None).await.unwrap();
}
