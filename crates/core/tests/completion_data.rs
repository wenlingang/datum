//! 补全用到的库内数据:外键(物理 + 推断)与低基数列的取值。在临时 SQLite 上跑。

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;

async fn sqlite() -> (AppState, String) {
    let dir = std::env::temp_dir().join(format!("datum-compl-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let st = AppState::new(dir.clone());
    let file = dir.join("c.db");
    rusqlite::Connection::open(&file).unwrap();
    let c = Connection {
        id: "compl".into(),
        name: "c".into(),
        kind: DbKind::Sqlite,
        env: Env::Local,
        host: String::new(),
        port: 0,
        user: String::new(),
        database: String::new(),
        plaintext_password: None,
        file_path: Some(file.display().to_string()),
        ssl_mode: SslMode::Disable,
        uri: None,
        auth_source: None,
        ssh: None,
    };
    commands::save_connection(&st, c, Some(String::new())).unwrap();
    service::connect(&st, &DbKind::Sqlite, "compl").await.unwrap();
    for sql in [
        "CREATE TABLE users (id INTEGER PRIMARY KEY, status TEXT, created_at DATETIME)",
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users(id), state TEXT)",
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, order_id INTEGER, body TEXT)",
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200) \
         INSERT INTO users SELECT i, CASE i % 3 WHEN 0 THEN 'active' WHEN 1 THEN 'banned' ELSE 'it''s' END, '2026-01-01' FROM n",
    ] {
        service::query(&st, &DbKind::Sqlite, "compl", sql, None).await.unwrap();
    }
    (st, "compl".into())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_keys_physical_and_inferred() {
    let (st, id) = sqlite().await;
    let fks = service::foreign_keys(&st, &DbKind::Sqlite, &id).await.unwrap();
    let phys = fks.iter().find(|f| f.table == "orders").unwrap();
    assert_eq!((phys.column.as_str(), phys.ref_table.as_str(), phys.inferred), ("user_id", "users", false));
    let inf = fks.iter().find(|f| f.table == "notes").unwrap();
    assert_eq!((inf.column.as_str(), inf.ref_table.as_str(), inf.inferred), ("order_id", "orders", true));
    assert_eq!(fks.iter().filter(|f| f.table == "orders").count(), 1, "物理外键覆盖的列不再推断");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distinct_values_only_for_low_cardinality() {
    let (st, id) = sqlite().await;
    let k = DbKind::Sqlite;
    let v = service::distinct_values(&st, &k, &id, "users", "status", "TEXT").await.unwrap().unwrap();
    assert_eq!(v, vec!["active", "banned", "it's"]);
    // 200 个不同的 id:不是枚举
    assert!(service::distinct_values(&st, &k, &id, "users", "id", "INTEGER").await.unwrap().is_none());
    // 时间类型直接跳过
    assert!(service::distinct_values(&st, &k, &id, "users", "created_at", "DATETIME").await.unwrap().is_none());
    // 空表:没有取值
    assert_eq!(service::distinct_values(&st, &k, &id, "orders", "state", "TEXT").await.unwrap(), Some(vec![]));
}
