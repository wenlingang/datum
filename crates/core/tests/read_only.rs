//! 只读执行(service::query_read_only):数据库层面拒绝写入,执行后会话恢复读写。
//! SQLite 用临时文件常规运行;PG / MySQL 需要本机数据库,默认忽略:
//!   DATUM_TEST_PG_DB=<库名>  DATUM_TEST_MYSQL_DB=<库名>  cargo test --test read_only -- --ignored

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env};
use datum_core::service;

fn conn(kind: DbKind, port: u16, user: &str, database: &str, file: Option<String>) -> Connection {
    Connection {
        id: format!("ro-{kind:?}"),
        name: "ro".into(),
        kind,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        user: user.into(),
        database: database.into(),
        plaintext_password: None,
        file_path: file,
    }
}

async fn check(state: &AppState, c: Connection) {
    let (kind, id) = (c.kind.clone(), c.id.clone());
    commands::save_connection(state, c, Some(String::new())).unwrap();
    service::connect(state, &kind, &id).await.unwrap();
    let q = |sql: &'static str| service::query(state, &kind, &id, sql, None);
    q("DROP TABLE IF EXISTS datum_ro_t").await.unwrap();
    q("CREATE TABLE datum_ro_t (id INT)").await.unwrap();
    q("INSERT INTO datum_ro_t VALUES (1)").await.unwrap();

    let ro = |sql: &'static str| service::query_read_only(state, &kind, &id, sql, None);
    // 只读查询照常
    assert_eq!(ro("SELECT id FROM datum_ro_t").await.unwrap().rows.len(), 1);
    // 写入被数据库拒绝,包括夹在多条语句里、或先 COMMIT 再写的
    assert!(ro("INSERT INTO datum_ro_t VALUES (2)").await.is_err());
    assert!(ro("DELETE FROM datum_ro_t").await.is_err());
    // 多语句:各驱动行为不同(SQLite 首句是查询时只执行首句),不要求报错,只要求没写成功
    let _ = ro("SELECT 1; DELETE FROM datum_ro_t").await;
    // 先 COMMIT 结束当前事务再写:会话级只读对新事务同样生效
    let _ = ro("COMMIT; DELETE FROM datum_ro_t").await;
    // 并发的多次只读执行:各自的「恢复读写」不能让别的那次以读写模式执行
    let (a, b, c, d) = tokio::join!(
        ro("DELETE FROM datum_ro_t"),
        ro("DELETE FROM datum_ro_t"),
        ro("DELETE FROM datum_ro_t"),
        ro("DELETE FROM datum_ro_t")
    );
    assert!(a.is_err() && b.is_err() && c.is_err() && d.is_err());
    // 数据没被改
    let r = q("SELECT count(*) FROM datum_ro_t").await.unwrap();
    assert_eq!(r.rows[0][0].as_deref(), Some("1"));
    // 会话已恢复读写
    q("INSERT INTO datum_ro_t VALUES (3)").await.unwrap();
    q("DROP TABLE datum_ro_t").await.unwrap();
}

fn state() -> (std::path::PathBuf, AppState) {
    let dir = std::env::temp_dir().join(format!(
        "datum-ro-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    (dir.clone(), AppState::new(dir))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_read_only_rejects_writes() {
    let (dir, st) = state();
    let file = dir.join("t.db").to_string_lossy().to_string();
    check(&st, conn(DbKind::Sqlite, 0, "", "", Some(file))).await;
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_read_only_rejects_writes() {
    let db = std::env::var("DATUM_TEST_PG_DB").expect("DATUM_TEST_PG_DB");
    let (dir, st) = state();
    check(&st, conn(DbKind::Pg, 5432, "postgres", &db, None)).await;
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_read_only_rejects_writes() {
    let db = std::env::var("DATUM_TEST_MYSQL_DB").expect("DATUM_TEST_MYSQL_DB");
    let (dir, st) = state();
    check(&st, conn(DbKind::Mysql, 3306, "root", &db, None)).await;
    std::fs::remove_dir_all(dir).ok();
}
