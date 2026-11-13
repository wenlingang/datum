//! 执行计划在真实库上的解析。SQLite 默认就跑;PG / MySQL 需要本机数据库,默认忽略:
//!   DATUM_DIAG_PG=端口  DATUM_DIAG_MYSQL=端口(同 tests/diag.rs 的容器)
//!   cargo test -p datum-core --test explain -- --include-ignored --test-threads=1

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::explain;
use datum_core::service;

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-explain-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

async fn connect(st: &AppState, kind: DbKind, port: u16, user: &str, db: &str, file: Option<String>) -> String {
    let id = format!("ex-{kind:?}-{}", uuid::Uuid::new_v4());
    let c = Connection {
        id: id.clone(),
        name: "ex".into(),
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
    };
    commands::save_connection(st, c, Some(String::new())).unwrap();
    service::connect(st, &kind, &id).await.unwrap();
    id
}

async fn exec(st: &AppState, kind: DbKind, id: &str, sql: &str) {
    service::query(st, &kind, id, sql, None).await.unwrap();
}

fn labels(n: &explain::PlanNode, out: &mut Vec<String>) {
    out.push(n.label.clone());
    for c in &n.children {
        labels(c, out);
    }
}

async fn seed(st: &AppState, kind: DbKind, id: &str) {
    exec(st, kind, id, "DROP TABLE IF EXISTS ex_orders").await;
    exec(st, kind, id, "DROP TABLE IF EXISTS ex_customers").await;
    exec(st, kind, id, "CREATE TABLE ex_customers (id INT PRIMARY KEY, name VARCHAR(20))").await;
    exec(st, kind, id, "CREATE TABLE ex_orders (id INT PRIMARY KEY, customer_id INT, status VARCHAR(20))").await;
    exec(st, kind, id, "INSERT INTO ex_customers VALUES (1, 'a'), (2, 'b'), (3, 'c')").await;
    exec(st, kind, id, "INSERT INTO ex_orders VALUES (1, 1, 'paid'), (2, 1, 'paid'), (3, 2, 'cancelled')").await;
}

const Q: &str = "SELECT c.name, COUNT(*) FROM ex_customers c JOIN ex_orders o ON o.customer_id = c.id WHERE o.status = 'paid' GROUP BY c.name";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_plan() {
    let st = state();
    let file = st.config_dir.join("ex.db");
    rusqlite::Connection::open(&file).unwrap();
    let id = connect(&st, DbKind::Sqlite, 0, "", "", Some(file.display().to_string())).await;
    seed(&st, DbKind::Sqlite, &id).await;
    let plan = explain::explain(&st, &DbKind::Sqlite, &id, Q, false).await.unwrap();
    let tree = plan.tree.expect("tree");
    let mut l = vec![];
    labels(&tree, &mut l);
    assert!(l.iter().any(|x| x == "SCAN" || x == "SEARCH"), "{l:?}");
    assert!(explain::explain(&st, &DbKind::Sqlite, &id, Q, true).await.is_err(), "SQLite 没有 ANALYZE");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_plan_and_analyze() {
    let Some(port) = std::env::var("DATUM_DIAG_PG").ok().and_then(|p| p.parse().ok()) else { return };
    let st = state();
    let id = connect(&st, DbKind::Pg, port, "postgres", "postgres", None).await;
    seed(&st, DbKind::Pg, &id).await;
    let plan = explain::explain(&st, &DbKind::Pg, &id, Q, false).await.unwrap();
    let tree = plan.tree.expect("tree");
    assert!(tree.cost.unwrap() > 0.0);
    let mut l = vec![];
    labels(&tree, &mut l);
    assert!(l.iter().any(|x| x.contains("Scan")), "{l:?}");
    assert!(!plan.analyzed);
    // 原始输出是文本版计划
    assert!(plan.raw.rows[0][0].as_deref().unwrap().contains("cost="));
    let a = explain::explain(&st, &DbKind::Pg, &id, Q, true).await.unwrap();
    assert!(a.analyzed && a.total_ms.is_some() && a.tree.unwrap().actual_ms.is_some());
    // 写语句不允许 ANALYZE(会真正执行)
    assert!(explain::explain(&st, &DbKind::Pg, &id, "DELETE FROM ex_orders", true).await.is_err());
    let left = service::query(&st, &DbKind::Pg, &id, "SELECT COUNT(*) FROM ex_orders", None).await.unwrap();
    assert_eq!(left.rows[0][0].as_deref(), Some("3"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_plan_and_analyze() {
    let Some(port) = std::env::var("DATUM_DIAG_MYSQL").ok().and_then(|p| p.parse().ok()) else { return };
    let st = state();
    let boot = connect(&st, DbKind::Mysql, port, "root", "", None).await;
    exec(&st, DbKind::Mysql, &boot, "CREATE DATABASE IF NOT EXISTS datum_explain_test").await;
    let id = connect(&st, DbKind::Mysql, port, "root", "datum_explain_test", None).await;
    seed(&st, DbKind::Mysql, &id).await;
    let plan = explain::explain(&st, &DbKind::Mysql, &id, Q, false).await.unwrap();
    let tree = plan.tree.expect("tree");
    let mut l = vec![];
    labels(&tree, &mut l);
    assert!(l.len() >= 2, "{l:?}");
    let a = explain::explain(&st, &DbKind::Mysql, &id, Q, true).await.unwrap();
    assert!(a.analyzed && a.tree.unwrap().actual_ms.is_some());
    exec(&st, DbKind::Mysql, &boot, "DROP DATABASE datum_explain_test").await;
}
