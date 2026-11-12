//! 撤销:执行前抓快照生成反向语句,执行后在一个事务里撤销,数据回到原样。
//! SQLite 默认就跑;PG / MySQL 需要本机数据库,默认忽略:
//!   DATUM_UNDO_PG=端口      PG(用户 postgres、库 postgres、空密码)
//!   DATUM_UNDO_MYSQL=端口   MySQL(用户 root、空密码,会建库 datum_undo_test)
//!   cargo test -p datum-core --test undo -- --include-ignored --test-threads=1

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-undo-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

fn conn(kind: DbKind, port: u16, user: &str, db: &str, file: Option<String>) -> Connection {
    Connection {
        id: format!("undo-{kind:?}-{}", uuid::Uuid::new_v4()),
        name: "undo".into(),
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

/// 整表内容(按 id 排序),用来比对撤销前后。
async fn dump(st: &AppState, kind: DbKind, id: &str) -> Vec<Vec<Option<String>>> {
    service::query(st, &kind, id, "SELECT id, status, note FROM undo_items ORDER BY id", None)
        .await
        .unwrap()
        .rows
}

async fn check_round_trips(st: &AppState, kind: DbKind, id: &str) {
    exec(st, kind, id, "DROP TABLE IF EXISTS undo_items").await;
    exec(st, kind, id, "CREATE TABLE undo_items (id INT PRIMARY KEY, status VARCHAR(20), note VARCHAR(50))").await;
    exec(st, kind, id, "INSERT INTO undo_items VALUES (1,'paid',NULL),(2,'paid','it''s'),(3,'cancelled','x'),(4,'cancelled',NULL)").await;
    let before = dump(st, kind, id).await;

    // 一段里两条写语句:先改再删;撤销倒序执行
    let sql = "UPDATE undo_items SET status = 'archived', note = 'n' WHERE status = 'paid'; DELETE FROM undo_items WHERE id >= 3";
    let prep = service::undo_prepare(st, &kind, id, sql).await.unwrap();
    assert!(prep.reason.is_none(), "{:?}", prep.reason);
    assert_eq!(prep.rows, 4);
    assert_eq!(prep.statements.len(), 4);
    assert!(prep.statements[0].starts_with("INSERT"), "删除的先撤销:{:?}", prep.statements);
    for stmt in datum_core::sql::stmt::statements(sql) {
        exec(st, kind, id, &stmt).await;
    }
    assert_ne!(dump(st, kind, id).await, before);
    service::import_sql(st, &kind, id, prep.statements).await.unwrap();
    assert_eq!(dump(st, kind, id).await, before, "撤销后应回到原样");

    // 不能撤销的情况给出原因
    exec(st, kind, id, "CREATE TABLE IF NOT EXISTS undo_nopk (a INT, b INT)").await;
    let r = service::undo_prepare(st, &kind, id, "UPDATE undo_nopk SET b = 1").await.unwrap();
    assert!(r.statements.is_empty() && r.reason.is_some());
    let r = service::undo_prepare(st, &kind, id, "UPDATE undo_items SET id = id + 10").await.unwrap();
    assert!(r.reason.unwrap().contains("primary key"));
    let r = service::undo_prepare(st, &kind, id, "INSERT INTO undo_items VALUES (9,'x',NULL)").await.unwrap();
    assert!(r.reason.is_some());
    let r = service::undo_prepare(st, &kind, id, "TRUNCATE TABLE undo_items").await.unwrap();
    assert!(r.reason.is_some());
    // 快照全程只读:数据没动
    assert_eq!(dump(st, kind, id).await, before);
    exec(st, kind, id, "DROP TABLE undo_nopk").await;
    exec(st, kind, id, "DROP TABLE undo_items").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_undo() {
    let st = state();
    let file = st.config_dir.join("undo.db");
    rusqlite::Connection::open(&file).unwrap();
    let (kind, id) = connect(&st, conn(DbKind::Sqlite, 0, "", "", Some(file.display().to_string()))).await;
    check_round_trips(&st, kind, &id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_undo() {
    let Some(port) = std::env::var("DATUM_UNDO_PG").ok().and_then(|p| p.parse().ok()) else {
        return;
    };
    let st = state();
    let (kind, id) = connect(&st, conn(DbKind::Pg, port, "postgres", "postgres", None)).await;
    check_round_trips(&st, kind, &id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_undo() {
    let Some(port) = std::env::var("DATUM_UNDO_MYSQL").ok().and_then(|p| p.parse().ok()) else {
        return;
    };
    let st = state();
    let (kind, boot) = connect(&st, conn(DbKind::Mysql, port, "root", "", None)).await;
    exec(&st, kind, &boot, "CREATE DATABASE IF NOT EXISTS datum_undo_test").await;
    let (kind, id) = connect(&st, conn(DbKind::Mysql, port, "root", "datum_undo_test", None)).await;
    check_round_trips(&st, kind, &id).await;
    exec(&st, kind, &id, "DROP DATABASE datum_undo_test").await;
}
