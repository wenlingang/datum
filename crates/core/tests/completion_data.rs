//! 补全用到的库内数据:外键(物理 + 推断)与低基数列的取值。
//! SQLite 默认就跑;PG / MySQL 需要本机数据库,默认忽略:
//!   DATUM_COMPL_PG=端口      PG(用户 postgres、空密码,会建库 datum_compl_test)
//!   DATUM_COMPL_MYSQL=端口   MySQL(用户 root、空密码,会建库 datum_compl_test)
//!   cargo test -p datum-core --test completion_data -- --include-ignored --test-threads=1

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-compl-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

async fn connect(st: &AppState, kind: DbKind, port: u16, user: &str, db: &str, file: Option<String>) -> String {
    let id = format!("compl-{}", uuid::Uuid::new_v4());
    let c = Connection {
        id: id.clone(),
        name: "c".into(),
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
    service::query(st, &kind, id, sql, None).await.unwrap_or_else(|e| panic!("{sql}: {}", e.display()));
}

/// 建表并灌 200 个用户(状态只有三种取值,其中一个带单引号)。
async fn seed(st: &AppState, kind: DbKind, id: &str) {
    let ts = if kind == DbKind::Pg { "TIMESTAMP" } else { "DATETIME" };
    exec(st, kind, id, &format!("CREATE TABLE users (id INTEGER PRIMARY KEY, status VARCHAR(20), created_at {ts})")).await;
    exec(st, kind, id, "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users(id), state VARCHAR(20))").await;
    if kind == DbKind::Mysql {
        // MySQL 忽略列上内联的 REFERENCES:单独加约束
        exec(st, kind, id, "ALTER TABLE orders ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_id) REFERENCES users(id)").await;
    }
    exec(st, kind, id, "CREATE TABLE notes (id INTEGER PRIMARY KEY, order_id INTEGER, body TEXT)").await;
    let rows: Vec<String> = (1..=200)
        .map(|i| {
            let s = match i % 3 {
                0 => "active",
                1 => "banned",
                _ => "it''s",
            };
            format!("({i}, '{s}', '2026-01-01 00:00:00')")
        })
        .collect();
    exec(st, kind, id, &format!("INSERT INTO users VALUES {}", rows.join(", "))).await;
}

async fn check(st: &AppState, kind: DbKind, id: &str) {
    let fks = service::foreign_keys(st, &kind, id).await.unwrap();
    let phys = fks.iter().find(|f| f.table == "orders").unwrap();
    assert_eq!((phys.column.as_str(), phys.ref_table.as_str(), phys.inferred), ("user_id", "users", false));
    let inf = fks.iter().find(|f| f.table == "notes").unwrap();
    assert_eq!((inf.column.as_str(), inf.ref_table.as_str(), inf.inferred), ("order_id", "orders", true));
    assert_eq!(fks.iter().filter(|f| f.table == "orders").count(), 1, "物理外键覆盖的列不再推断");

    let mut v = service::distinct_values(st, &kind, id, "users", "status", "varchar").await.unwrap().unwrap();
    v.sort();
    assert_eq!(v, vec!["active", "banned", "it's"]);
    // 200 个不同的 id:不是枚举
    assert!(service::distinct_values(st, &kind, id, "users", "id", "integer").await.unwrap().is_none());
    // 时间类型直接跳过
    assert!(service::distinct_values(st, &kind, id, "users", "created_at", "timestamp").await.unwrap().is_none());
    // 空表:没有取值
    assert_eq!(service::distinct_values(st, &kind, id, "orders", "state", "varchar").await.unwrap(), Some(vec![]));

    // 改了表结构后重新取外键:新加的外键能拿到(界面在建表 / 改表后刷新)
    exec(st, kind, id, "CREATE TABLE refunds (id INTEGER PRIMARY KEY, ord INTEGER, FOREIGN KEY (ord) REFERENCES orders(id))").await;
    let fks = service::foreign_keys(st, &kind, id).await.unwrap();
    assert!(fks.iter().any(|f| f.table == "refunds" && f.column == "ord" && !f.inferred), "{fks:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_completion_data() {
    let st = state();
    let file = st.config_dir.join("c.db");
    rusqlite::Connection::open(&file).unwrap();
    let id = connect(&st, DbKind::Sqlite, 0, "", "", Some(file.display().to_string())).await;
    seed(&st, DbKind::Sqlite, &id).await;
    check(&st, DbKind::Sqlite, &id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_completion_data() {
    let Some(port) = std::env::var("DATUM_COMPL_PG").ok().and_then(|p| p.parse().ok()) else { return };
    let st = state();
    let boot = connect(&st, DbKind::Pg, port, "postgres", "postgres", None).await;
    exec(&st, DbKind::Pg, &boot, "DROP DATABASE IF EXISTS datum_compl_test").await;
    exec(&st, DbKind::Pg, &boot, "CREATE DATABASE datum_compl_test").await;
    let id = connect(&st, DbKind::Pg, port, "postgres", "datum_compl_test", None).await;
    seed(&st, DbKind::Pg, &id).await;
    check(&st, DbKind::Pg, &id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_completion_data() {
    let Some(port) = std::env::var("DATUM_COMPL_MYSQL").ok().and_then(|p| p.parse().ok()) else { return };
    let st = state();
    let boot = connect(&st, DbKind::Mysql, port, "root", "", None).await;
    exec(&st, DbKind::Mysql, &boot, "DROP DATABASE IF EXISTS datum_compl_test").await;
    exec(&st, DbKind::Mysql, &boot, "CREATE DATABASE datum_compl_test").await;
    let id = connect(&st, DbKind::Mysql, port, "root", "datum_compl_test", None).await;
    seed(&st, DbKind::Mysql, &id).await;
    check(&st, DbKind::Mysql, &id).await;
    exec(&st, DbKind::Mysql, &id, "DROP DATABASE datum_compl_test").await;
}
