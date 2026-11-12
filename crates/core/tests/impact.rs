//! 写操作影响范围预估:改写成只读查询后真实执行。
//! SQLite 默认就跑;PG / MySQL 需要本机数据库,默认忽略:
//!   DATUM_IMPACT_PG=端口      PG(用户 postgres、库 postgres、空密码)
//!   DATUM_IMPACT_MYSQL=端口   MySQL(用户 root、空密码,会建库 datum_impact_test)
//!   cargo test -p datum-core --test impact -- --include-ignored --test-threads=1

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-impact-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

fn conn(kind: DbKind, port: u16, user: &str, db: &str, file: Option<String>) -> Connection {
    Connection {
        id: format!("impact-{kind:?}-{}", uuid::Uuid::new_v4()),
        name: "impact".into(),
        kind,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        user: user.into(),
        database: db.into(),
        plaintext_password: None,
        file_path: file,
        ssl_mode: SslMode::Disable,
        uri: None,
        auth_source: None,
        ssh: None,
    }
}

async fn connect(st: &AppState, c: Connection) -> (DbKind, String) {
    let (kind, id) = (c.kind, c.id.clone());
    commands::save_connection(st, c, Some(String::new())).unwrap();
    service::connect(st, &kind, &id).await.unwrap();
    (kind, id)
}

async fn exec(st: &AppState, kind: DbKind, id: &str, sql: &str) {
    service::query(st, &kind, id, sql, None).await.unwrap();
}

async fn count_rows(st: &AppState, kind: DbKind, id: &str, table: &str) -> String {
    let r = service::query(st, &kind, id, &format!("SELECT COUNT(*) FROM {table}"), None).await.unwrap();
    r.rows[0][0].clone().unwrap()
}

/// 三种库共用的断言:数对行数、带回预览、数据一行没动。
async fn check_counts_and_leaves_data_alone(st: &AppState, kind: DbKind, id: &str) {
    exec(st, kind, id, "DROP TABLE IF EXISTS impact_items").await;
    exec(st, kind, id, "CREATE TABLE impact_items (id INT PRIMARY KEY, status VARCHAR(20))").await;
    let values: Vec<String> = (1..=30)
        .map(|i| format!("({i}, '{}')", if i % 3 == 0 { "cancelled" } else { "paid" }))
        .collect();
    exec(st, kind, id, &format!("INSERT INTO impact_items VALUES {}", values.join(", "))).await;

    let i = service::write_impact(st, &kind, id, "DELETE FROM impact_items WHERE status = 'cancelled'")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(i.count, 10);
    assert!(!i.whole_table);
    assert_eq!(i.rows.len(), 10);
    assert!(i.columns.iter().any(|c| c == "status"));

    let i = service::write_impact(st, &kind, id, "UPDATE impact_items SET status = 'x'").await.unwrap().unwrap();
    assert_eq!(i.count, 30);
    assert_eq!(i.rows.len(), 10, "预览最多 10 行");

    let i = service::write_impact(st, &kind, id, "UPDATE impact_items SET status = 'x' WHERE id > 1000")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(i.count, 0);
    assert!(i.rows.is_empty());

    let i = service::write_impact(st, &kind, id, "DROP TABLE impact_items").await.unwrap().unwrap();
    assert_eq!(i.count, 30);
    assert!(i.whole_table);

    // 改写不了的形态:不猜
    assert!(service::write_impact(st, &kind, id, "INSERT INTO impact_items VALUES (99, 'x')")
        .await
        .unwrap()
        .is_none());

    // 预估全程只读:数据没动
    assert_eq!(count_rows(st, kind, id, "impact_items").await, "30");
    exec(st, kind, id, "DROP TABLE impact_items").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_impact() {
    let st = state();
    let file = st.config_dir.join("impact.db");
    rusqlite::Connection::open(&file).unwrap();
    let (kind, id) = connect(&st, conn(DbKind::Sqlite, 0, "", "", Some(file.display().to_string()))).await;
    check_counts_and_leaves_data_alone(&st, kind, &id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_impact_and_timeout() {
    let Some(port) = std::env::var("DATUM_IMPACT_PG").ok().and_then(|p| p.parse().ok()) else {
        return;
    };
    let st = state();
    let (kind, id) = connect(&st, conn(DbKind::Pg, port, "postgres", "postgres", None)).await;
    check_counts_and_leaves_data_alone(&st, kind, &id).await;
    // 慢条件:服务端 5 秒中止,报超时;连接随后照常可用
    exec(&st, kind, &id, "CREATE TABLE IF NOT EXISTS impact_slow (id INT)").await;
    exec(&st, kind, &id, "INSERT INTO impact_slow VALUES (1)").await;
    let started = std::time::Instant::now();
    let e = service::write_impact(&st, &kind, &id, "DELETE FROM impact_slow WHERE pg_sleep(10) IS NOT NULL")
        .await
        .unwrap_err();
    assert!(started.elapsed().as_secs() < 9, "没有按时中止");
    assert!(e.display().contains("stopped"), "{}", e.display());
    assert_eq!(count_rows(&st, kind, &id, "impact_slow").await, "1");
    exec(&st, kind, &id, "DROP TABLE impact_slow").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_impact_and_timeout() {
    let Some(port) = std::env::var("DATUM_IMPACT_MYSQL").ok().and_then(|p| p.parse().ok()) else {
        return;
    };
    let st = state();
    let (kind, boot) = connect(&st, conn(DbKind::Mysql, port, "root", "", None)).await;
    exec(&st, kind, &boot, "CREATE DATABASE IF NOT EXISTS datum_impact_test").await;
    let (kind, id) = connect(&st, conn(DbKind::Mysql, port, "root", "datum_impact_test", None)).await;
    check_counts_and_leaves_data_alone(&st, kind, &id).await;
    exec(&st, kind, &id, "CREATE TABLE IF NOT EXISTS impact_slow (id INT)").await;
    exec(&st, kind, &id, "INSERT INTO impact_slow VALUES (1)").await;
    let started = std::time::Instant::now();
    let e = service::write_impact(&st, &kind, &id, "DELETE FROM impact_slow WHERE SLEEP(10) = 0")
        .await
        .unwrap_err();
    assert!(started.elapsed().as_secs() < 9, "没有按时中止");
    assert!(e.display().contains("stopped"), "{}", e.display());
    assert_eq!(count_rows(&st, kind, &id, "impact_slow").await, "1");
    exec(&st, kind, &id, "DROP DATABASE datum_impact_test").await;
}
