//! 表结构对比在真实库上:生成的脚本在目标库执行后,再对比只剩被注释掉的删除项。
//! SQLite 默认就跑;PG / MySQL 需要本机数据库,默认忽略:
//!   DATUM_DIAG_PG=端口  DATUM_DIAG_MYSQL=端口(同 tests/diag.rs 的容器)
//!   cargo test -p datum-core --test schema_diff -- --include-ignored --test-threads=1

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::{schema_diff, service};

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-sdiff-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

async fn connect(st: &AppState, kind: DbKind, port: u16, user: &str, db: &str, file: Option<String>) -> String {
    let id = format!("sd-{kind:?}-{}", uuid::Uuid::new_v4());
    let c = Connection {
        id: id.clone(),
        name: "sd".into(),
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

/// 执行脚本里没被注释的语句(按分号切,跳过注释行)。
async fn apply(st: &AppState, kind: DbKind, id: &str, script: &str) {
    let body: String = script.lines().filter(|l| !l.trim_start().starts_with("--")).collect::<Vec<_>>().join("\n");
    for stmt in body.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        exec(st, kind, id, stmt).await;
    }
}

async fn roundtrip(st: &AppState, kind: DbKind, src: &str, dst: &str) {
    // 源库:orders 多一列、一个索引、状态列更长且 NOT NULL;多一张 refunds 表
    exec(st, kind, src, "CREATE TABLE orders (id INT PRIMARY KEY, status VARCHAR(20) NOT NULL DEFAULT 'new', note TEXT)").await;
    exec(st, kind, src, "CREATE INDEX orders_status_idx ON orders (status)").await;
    exec(st, kind, src, "CREATE TABLE refunds (id INT PRIMARY KEY, order_id INT REFERENCES orders (id) ON DELETE CASCADE)").await;
    // 两边都有的 items:源库多一个外键;源库多一个视图、目标库多一个视图
    exec(st, kind, src, "CREATE TABLE items (id INT PRIMARY KEY, order_id INT, FOREIGN KEY (order_id) REFERENCES orders (id))").await;
    exec(st, kind, src, "CREATE VIEW v_orders AS SELECT id, status FROM orders").await;
    // 目标库:旧结构,多一张 legacy 表、多一列 old_flag
    exec(st, kind, dst, "CREATE TABLE orders (id INT PRIMARY KEY, status VARCHAR(10), old_flag INT)").await;
    exec(st, kind, dst, "CREATE TABLE legacy (id INT PRIMARY KEY)").await;
    exec(st, kind, dst, "CREATE TABLE items (id INT PRIMARY KEY, order_id INT)").await;
    exec(st, kind, dst, "CREATE VIEW v_legacy AS SELECT id FROM legacy").await;
    // 注释(SQLite 没有)
    match kind {
        DbKind::Pg => {
            exec(st, kind, src, "COMMENT ON TABLE orders IS '订单'").await;
            exec(st, kind, src, "COMMENT ON COLUMN orders.status IS '状态'").await;
        }
        DbKind::Mysql => {
            exec(st, kind, src, "ALTER TABLE orders COMMENT = '订单'").await;
            exec(st, kind, src, "ALTER TABLE orders MODIFY COLUMN status VARCHAR(20) NOT NULL DEFAULT 'new' COMMENT '状态'").await;
        }
        _ => {}
    }

    let r = schema_diff::compare(st, &kind, src, dst).await.unwrap();
    let names: Vec<(&str, &str, &str)> = r.tables.iter().map(|t| (t.kind, t.name.as_str(), t.status)).collect();
    assert!(names.contains(&("table", "refunds", "added")), "{names:?}");
    assert!(names.contains(&("table", "legacy", "removed")));
    assert!(names.contains(&("table", "orders", "changed")));
    assert!(names.contains(&("table", "items", "changed")), "{names:?}");
    assert!(names.contains(&("view", "v_orders", "added")), "{names:?}");
    assert!(names.contains(&("view", "v_legacy", "removed")), "{names:?}");
    assert!(!names.iter().any(|(k, n, _)| *k == "table" && n.starts_with("v_")), "视图不当表比较:{names:?}");
    if kind != DbKind::Sqlite {
        let orders = r.tables.iter().find(|t| t.name == "orders").unwrap();
        assert!(orders.changes.iter().any(|c| c.contains("订单")), "{:?}", orders.changes);
        assert!(orders.changes.iter().any(|c| c.contains("状态")), "{:?}", orders.changes);
    }
    eprintln!("{kind:?} script:\n{}", r.sql);
    apply(st, kind, dst, &r.sql).await;

    // 再对比:只剩目标库独有的(删除语句是注释,没执行)
    let again = schema_diff::compare(st, &kind, src, dst).await.unwrap();
    let left: Vec<(&str, &str)> = again.tables.iter().map(|t| (t.name.as_str(), t.status)).collect();
    assert!(left.contains(&("legacy", "removed")));
    // 视图已建;目标库独有的视图只给了注释掉的 DROP
    assert!(!left.contains(&("v_orders", "added")), "{left:?}");
    assert!(left.contains(&("v_legacy", "removed")));
    // 外键已补上(SQLite 不能给已有的表加外键,仍会列出)
    assert_eq!(left.contains(&("items", "changed")), kind == DbKind::Sqlite, "{left:?}");
    let orders = again.tables.iter().find(|t| t.name == "orders");
    if let Some(o) = orders {
        // SQLite 改不了列定义;其余只应剩下目标库独有的列
        if kind != DbKind::Sqlite {
            assert_eq!(o.changes.len(), 1, "{:?}", o.changes);
            assert!(o.changes[0].contains("old_flag"));
        }
    }
    assert!(!left.iter().any(|(n, _)| *n == "refunds"), "新表已建好");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_roundtrip() {
    let st = state();
    let a = st.config_dir.join("a.db");
    let b = st.config_dir.join("b.db");
    rusqlite::Connection::open(&a).unwrap();
    rusqlite::Connection::open(&b).unwrap();
    let src = connect(&st, DbKind::Sqlite, 0, "", "", Some(a.display().to_string())).await;
    let dst = connect(&st, DbKind::Sqlite, 0, "", "", Some(b.display().to_string())).await;
    roundtrip(&st, DbKind::Sqlite, &src, &dst).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_roundtrip() {
    let Some(port) = std::env::var("DATUM_DIAG_PG").ok().and_then(|p| p.parse().ok()) else { return };
    let st = state();
    let boot = connect(&st, DbKind::Pg, port, "postgres", "postgres", None).await;
    for db in ["sd_src", "sd_dst"] {
        exec(&st, DbKind::Pg, &boot, &format!("DROP DATABASE IF EXISTS {db}")).await;
        exec(&st, DbKind::Pg, &boot, &format!("CREATE DATABASE {db}")).await;
    }
    let src = connect(&st, DbKind::Pg, port, "postgres", "sd_src", None).await;
    let dst = connect(&st, DbKind::Pg, port, "postgres", "sd_dst", None).await;
    roundtrip(&st, DbKind::Pg, &src, &dst).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_roundtrip() {
    let Some(port) = std::env::var("DATUM_DIAG_MYSQL").ok().and_then(|p| p.parse().ok()) else { return };
    let st = state();
    let boot = connect(&st, DbKind::Mysql, port, "root", "", None).await;
    for db in ["sd_src", "sd_dst"] {
        exec(&st, DbKind::Mysql, &boot, &format!("DROP DATABASE IF EXISTS {db}")).await;
        exec(&st, DbKind::Mysql, &boot, &format!("CREATE DATABASE {db}")).await;
    }
    let src = connect(&st, DbKind::Mysql, port, "root", "sd_src", None).await;
    let dst = connect(&st, DbKind::Mysql, port, "root", "sd_dst", None).await;
    roundtrip(&st, DbKind::Mysql, &src, &dst).await;
}
