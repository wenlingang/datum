//! 关系库诊断的真实执行。SQLite 默认就跑;PG / MySQL 需要本机数据库,默认忽略:
//!   DATUM_DIAG_PG=端口      PG(用户 postgres、库 postgres、免密;开 pg_stat_statements 才测慢 SQL 排行)
//!     docker run --rm -p 55491:5432 -e POSTGRES_HOST_AUTH_METHOD=trust postgres:16-alpine \
//!       -c shared_preload_libraries=pg_stat_statements
//!   DATUM_DIAG_MYSQL=端口   MySQL(用户 root、空密码,会建库 datum_diag_test)
//!     docker run --rm -p 33391:3306 -e MYSQL_ALLOW_EMPTY_PASSWORD=yes mysql:8.0
//!   cargo test -p datum-core --test diag -- --include-ignored --test-threads=1

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::diag::{self, Action, DiagResult};
use datum_core::service;
use std::time::Duration;

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-diag-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

fn conn(kind: DbKind, port: u16, user: &str, db: &str, file: Option<String>) -> Connection {
    Connection {
        id: format!("diag-{kind:?}-{}", uuid::Uuid::new_v4()),
        name: "diag".into(),
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

fn col(r: &DiagResult, name: &str) -> usize {
    r.columns.iter().position(|c| c == name).unwrap_or_else(|| panic!("缺列 {name}: {:?}", r.columns))
}

/// 每个分区都能查(或者给出不可用的原因),不报错。
async fn run_all(st: &AppState, kind: DbKind, id: &str) -> Vec<(String, DiagResult)> {
    let mut out = vec![];
    for s in diag::sections(&kind) {
        let r = service::diag_run(st, &kind, id, s.key)
            .await
            .unwrap_or_else(|e| panic!("{kind:?} {} 失败: {}", s.key, e.display()));
        if r.unavailable.is_none() {
            for c in &s.target_columns {
                col(&r, c);
            }
        }
        out.push((s.key.to_string(), r));
    }
    out
}

fn section<'a>(all: &'a [(String, DiagResult)], key: &str) -> &'a DiagResult {
    &all.iter().find(|(k, _)| k == key).unwrap().1
}

/// 轮询某个分区直到满足条件(最多约 5 秒)。
async fn wait_for(st: &AppState, kind: DbKind, id: &str, key: &str, ok: impl Fn(&DiagResult) -> bool) -> DiagResult {
    for _ in 0..50 {
        let r = service::diag_run(st, &kind, id, key).await.unwrap();
        if ok(&r) {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{key} 等不到期望的结果");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_diag() {
    let st = state();
    let file = st.config_dir.join("diag.db");
    rusqlite::Connection::open(&file).unwrap();
    let (kind, id) = connect(&st, conn(DbKind::Sqlite, 0, "", "", Some(file.display().to_string()))).await;
    exec(&st, kind, &id, "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT)").await;
    exec(&st, kind, &id, "CREATE INDEX items_name ON items(name)").await;
    exec(&st, kind, &id, "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000) \
                          INSERT INTO items SELECT i, printf('name-%d', i) FROM n").await;
    exec(&st, kind, &id, "DELETE FROM items WHERE id > 100").await;

    let all = run_all(&st, kind, &id).await;
    let o = section(&all, "overview");
    assert_eq!(o.rows.len(), 1);
    assert_eq!(o.rows[0][col(o, "tables")].as_deref(), Some("1"));
    let free: u64 = o.rows[0][col(o, "free_pages")].as_deref().unwrap().parse().unwrap();
    assert!(free > 0, "删掉大半行后应有空闲页");

    let t = section(&all, "tables");
    if t.unavailable.is_none() {
        assert!(t.rows.iter().any(|r| r[col(t, "name")].as_deref() == Some("items_name")));
    }
    let i = section(&all, "integrity");
    assert_eq!(i.rows, vec![vec![Some("ok".to_string())]]);

    service::diag_act(&st, &kind, &id, Action::Analyze, &[]).await.unwrap();
    service::diag_act(&st, &kind, &id, Action::Vacuum, &[]).await.unwrap();
    let o = service::diag_run(&st, &kind, &id, "overview").await.unwrap();
    assert_eq!(o.rows[0][col(&o, "free_pages")].as_deref(), Some("0"), "VACUUM 后空闲页清零");
    // 诊断查询跑在只读会话里,之后连接照常可写
    exec(&st, kind, &id, "INSERT INTO items VALUES (9999, 'x')").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_diag() {
    let Some(port) = std::env::var("DATUM_DIAG_PG").ok().and_then(|p| p.parse::<u16>().ok()) else {
        return;
    };
    let st = state();
    let (kind, id) = connect(&st, conn(DbKind::Pg, port, "postgres", "postgres", None)).await;
    exec(&st, kind, &id, "DROP EXTENSION IF EXISTS pg_stat_statements").await;
    exec(&st, kind, &id, "DROP TABLE IF EXISTS diag_items").await;
    exec(&st, kind, &id, "CREATE TABLE diag_items (id INT PRIMARY KEY, v TEXT)").await;
    exec(&st, kind, &id, "CREATE INDEX diag_items_v ON diag_items(v)").await;
    exec(&st, kind, &id, "INSERT INTO diag_items SELECT g, 'v' || g FROM generate_series(1, 1000) g").await;
    exec(&st, kind, &id, "DELETE FROM diag_items WHERE id > 500").await;

    let all = run_all(&st, kind, &id).await;
    let slow = section(&all, "slow");
    assert_eq!(slow.unavailable.as_ref().unwrap().reason, "pg_stat_statements_missing");
    let o = section(&all, "overview");
    assert!(o.rows[0][col(o, "version")].as_deref().unwrap().starts_with("16"));
    let u = section(&all, "unused_indexes");
    assert!(u.rows.iter().any(|r| r[col(u, "index")].as_deref() == Some("diag_items_v")));
    assert!(!u.rows.iter().any(|r| r[col(u, "index")].as_deref() == Some("diag_items_pkey")), "主键不算");

    // 慢 SQL 排行:建了扩展(镜像已预加载)就有数据
    exec(&st, kind, &id, "CREATE EXTENSION pg_stat_statements").await;
    exec(&st, kind, &id, "SELECT count(*) FROM diag_items").await;
    let slow = service::diag_run(&st, &kind, &id, "slow").await.unwrap();
    assert!(slow.unavailable.is_none());
    assert!(!slow.rows.is_empty());

    // 锁等待:一个会话持表锁,另一个等;诊断里能看到阻塞链,终止阻塞者后等待者完成
    let url = format!("host=127.0.0.1 port={port} user=postgres dbname=postgres");
    let (blocker, c1) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(c1);
    let (waiter, c2) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(c2);
    blocker.batch_execute("BEGIN; LOCK TABLE diag_items IN ACCESS EXCLUSIVE MODE").await.unwrap();
    let blocker_pid: i32 = blocker.query_one("SELECT pg_backend_pid()", &[]).await.unwrap().get(0);
    let waiting = tokio::spawn(async move { waiter.simple_query("SELECT count(*) FROM diag_items").await.is_ok() });

    let locks = wait_for(&st, kind, &id, "locks", |r| !r.rows.is_empty()).await;
    let pid = locks.rows[0][col(&locks, "blocking_pid")].clone().unwrap();
    assert_eq!(pid, blocker_pid.to_string());
    let sessions = service::diag_run(&st, &kind, &id, "sessions").await.unwrap();
    assert!(sessions.rows.iter().any(|r| r[col(&sessions, "pid")].as_deref() == Some(pid.as_str())));

    service::diag_act(&st, &kind, &id, Action::Terminate, std::slice::from_ref(&pid)).await.unwrap();
    let ok = tokio::time::timeout(Duration::from_secs(5), waiting).await.expect("等待者没有放行").unwrap();
    assert!(ok);
    assert!(blocker.simple_query("SELECT 1").await.is_err(), "阻塞者已被终止");
    // 再终止同一个会话:已经不在了,报错而不是静默成功
    let e = service::diag_act(&st, &kind, &id, Action::Terminate, &[pid]).await;
    assert!(e.is_err());

    service::diag_act(&st, &kind, &id, Action::VacuumTable, &["public".into(), "diag_items".into()])
        .await
        .unwrap();
    let bloat = service::diag_run(&st, &kind, &id, "bloat").await.unwrap();
    let row = bloat.rows.iter().find(|r| r[col(&bloat, "table")].as_deref() == Some("diag_items")).unwrap();
    assert!(row[col(&bloat, "last_vacuum")].is_some());
    exec(&st, kind, &id, "DROP TABLE diag_items").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_diag() {
    let Some(port) = std::env::var("DATUM_DIAG_MYSQL").ok().and_then(|p| p.parse::<u16>().ok()) else {
        return;
    };
    let st = state();
    let (kind, boot) = connect(&st, conn(DbKind::Mysql, port, "root", "", None)).await;
    exec(&st, kind, &boot, "CREATE DATABASE IF NOT EXISTS datum_diag_test").await;
    let (kind, id) = connect(&st, conn(DbKind::Mysql, port, "root", "datum_diag_test", None)).await;
    exec(&st, kind, &id, "DROP TABLE IF EXISTS diag_items").await;
    exec(&st, kind, &id, "CREATE TABLE diag_items (id INT PRIMARY KEY, v VARCHAR(20), KEY diag_v (v))").await;
    exec(&st, kind, &id, "INSERT INTO diag_items VALUES (1, 'a'), (2, 'b')").await;

    let all = run_all(&st, kind, &id).await;
    let o = section(&all, "overview");
    assert!(o.rows[0][col(o, "version")].as_deref().unwrap().starts_with("8."));
    assert!(o.rows[0][col(o, "connections")].as_deref().unwrap().contains(" / "));
    let t = section(&all, "tables");
    assert!(t.rows.iter().any(|r| r[col(t, "table")].as_deref() == Some("diag_items")));
    assert!(section(&all, "slow").unavailable.is_none());

    // 锁等待:一个事务锁住行,另一个更新同一行被挡住
    let url = format!("mysql://root@127.0.0.1:{port}/datum_diag_test");
    use mysql_async::prelude::Queryable;
    let mut blocker = mysql_async::Conn::from_url(&url).await.unwrap();
    let mut waiter = mysql_async::Conn::from_url(&url).await.unwrap();
    blocker.query_drop("START TRANSACTION").await.unwrap();
    blocker.query_drop("SELECT * FROM diag_items WHERE id = 1 FOR UPDATE").await.unwrap();
    let blocker_id = blocker.id();
    let waiting = tokio::spawn(async move { waiter.query_drop("UPDATE diag_items SET v = 'z' WHERE id = 1").await.is_ok() });

    let locks = wait_for(&st, kind, &id, "locks", |r| !r.rows.is_empty()).await;
    assert_eq!(locks.rows[0][col(&locks, "blocking_id")].clone().unwrap(), blocker_id.to_string());
    let tx = service::diag_run(&st, &kind, &id, "long_tx").await.unwrap();
    assert!(tx.rows.iter().any(|r| r[col(&tx, "id")].as_deref() == Some(blocker_id.to_string().as_str())));

    service::diag_act(&st, &kind, &id, Action::Kill, &[blocker_id.to_string()]).await.unwrap();
    let ok = tokio::time::timeout(Duration::from_secs(5), waiting).await.expect("等待者没有放行").unwrap();
    assert!(ok);
    exec(&st, kind, &boot, "DROP DATABASE datum_diag_test").await;
}
