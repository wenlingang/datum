//! 造测试数据:在真实数据库上插入,核对约束都满足(非空、唯一、外键、枚举 / CHECK、长度与精度、
//! 自增 / 标识 / 生成列跳过、created ≤ updated),取消回滚,同一种子结果一致。
//! SQLite 默认就跑;PG / MySQL 设了环境变量才跑(没设时直接跳过):
//!   DATUM_TEST_PG=端口(postgres、空密码)  DATUM_TEST_MYSQL=端口(root、空密码)
//!   cargo test -p datum-core --test testdata

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;
use datum_core::testdata::api;
use datum_core::testdata::job::Progress;
use datum_core::testdata::{Generator, Locale, Plan};

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-testdata-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

fn conn(kind: DbKind, port: u16, user: &str, db: &str, file: Option<String>) -> Connection {
    Connection {
        id: format!("td-{kind:?}-{}", uuid::Uuid::new_v4()),
        name: "td".into(),
        kind,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        user: user.into(),
        database: db.into(),
        plaintext_password: None,
        file_path: file,
        ssl_mode: SslMode::Disable,
        ssh: None,
    }
}

async fn connect(st: &AppState, c: Connection) -> String {
    let (id, kind) = (c.id.clone(), c.kind);
    commands::save_connection(st, c, Some(String::new())).unwrap();
    service::connect(st, &kind, &id).await.unwrap();
    id
}

async fn exec(st: &AppState, kind: DbKind, id: &str, sql: &str) {
    service::query(st, &kind, id, sql, None).await.unwrap_or_else(|e| panic!("{sql}: {}", e.display()));
}

async fn one(st: &AppState, kind: DbKind, id: &str, sql: &str) -> String {
    let r = service::query(st, &kind, id, sql, None).await.unwrap_or_else(|e| panic!("{sql}: {}", e.display()));
    r.rows.first().and_then(|r| r.first().cloned().flatten()).unwrap_or_default()
}

async fn wait(st: &AppState, id: &str) -> Progress {
    loop {
        let p = api::progress(st, id).unwrap();
        if p.done {
            return p;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// 自动方案 + 给定行数 / 种子,插入并等完成。
async fn generate(st: &AppState, kind: DbKind, id: &str, table: &str, rows: u64, seed: u64) -> (Plan, Progress) {
    let mut plan = api::analyze(st, &kind, id, table, Locale::En).await.unwrap().plan;
    plan.rows = rows;
    plan.seed = seed;
    api::start(st, &kind, id, &plan).await.unwrap_or_else(|e| panic!("{table}: {}", e.display()));
    let p = wait(st, id).await;
    assert_eq!(p.error, None, "{table}");
    (plan, p)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_inserts_valid_rows_and_cancel_rolls_back() {
    let st = state();
    let file = std::env::temp_dir().join(format!("datum-td-{}.db", uuid::Uuid::new_v4()));
    let id = connect(&st, conn(DbKind::Sqlite, 0, "", "", Some(file.to_string_lossy().into_owned()))).await;
    let k = DbKind::Sqlite;
    exec(&st, k, &id, "CREATE TABLE users (id INTEGER PRIMARY KEY, email VARCHAR(60) NOT NULL UNIQUE, name TEXT, \
        created_at TEXT, updated_at TEXT)").await;
    exec(&st, k, &id, "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id), \
        status TEXT NOT NULL CHECK (status IN ('new','paid','shipped')), total NUMERIC(8,2) CHECK (total >= 0), \
        note VARCHAR(20), qty INTEGER CHECK (qty BETWEEN 1 AND 5), seq INTEGER UNIQUE)").await;

    // 父表空:外键列不可空,不能生成
    let a = api::analyze(&st, &k, &id, "orders", Locale::En).await.unwrap();
    assert_eq!(a.parent_empty.len(), 1);
    assert_eq!(a.parent_empty[0].parent, "users");
    assert!(api::start(&st, &k, &id, &a.plan).await.is_err());
    let id_plan = a.plan.columns.iter().find(|c| c.name == "id").unwrap();
    assert_eq!(id_plan.generator, Generator::Skip, "rowid 别名跳过");

    // 预览与实际插入的前几行一致
    let mut plan = api::analyze(&st, &k, &id, "users", Locale::En).await.unwrap().plan;
    plan.rows = 500;
    plan.seed = 42;
    let preview = api::preview(&st, &k, &id, &plan).await.unwrap();
    assert_eq!(preview.rows.len(), 10);
    assert!(!preview.columns.contains(&"id".to_string()));
    api::start(&st, &k, &id, &plan).await.unwrap();
    let p = wait(&st, &id).await;
    assert_eq!((p.error.clone(), p.rows, p.committed, p.batched), (None, 500, 500, false));
    let ei = preview.columns.iter().position(|c| c == "email").unwrap();
    assert_eq!(one(&st, k, &id, "SELECT email FROM users ORDER BY id LIMIT 1").await, preview.rows[0][ei].clone().unwrap());
    assert_eq!(one(&st, k, &id, "SELECT count(DISTINCT email) FROM users").await, "500");
    assert_eq!(one(&st, k, &id, "SELECT count(*) FROM users WHERE updated_at < created_at").await, "0");
    assert_eq!(one(&st, k, &id, "SELECT count(*) FROM users WHERE length(email) > 60 OR email NOT LIKE '%@%'").await, "0");

    // 子表:外键都指向已有的用户,CHECK 清单与范围、长度都满足,唯一整数列是递增序列
    generate(&st, k, &id, "orders", 1000, 7).await;
    assert_eq!(one(&st, k, &id, "SELECT count(*) FROM orders o JOIN users u ON u.id = o.user_id").await, "1000");
    assert_eq!(one(&st, k, &id, "SELECT count(*) FROM orders WHERE status NOT IN ('new','paid','shipped')").await, "0");
    assert_eq!(one(&st, k, &id, "SELECT count(*) FROM orders WHERE total < 0 OR total > 999999.99 OR length(note) > 20 OR qty NOT BETWEEN 1 AND 5").await, "0");
    assert_eq!(one(&st, k, &id, "SELECT count(DISTINCT seq) FROM orders").await, "1000");
    // 再来一批:序列接在现有最大值之后
    generate(&st, k, &id, "orders", 10, 8).await;
    assert_eq!(one(&st, k, &id, "SELECT count(DISTINCT seq) FROM orders").await, "1010");

    // 取消:单事务,一行不留
    let mut plan = api::analyze(&st, &k, &id, "users", Locale::Zh).await.unwrap().plan;
    plan.rows = 50_000;
    api::start(&st, &k, &id, &plan).await.unwrap();
    api::cancel(&st, &id);
    let p = wait(&st, &id).await;
    assert!(p.cancelled, "{p:?}");
    assert_eq!(p.committed, 0);
    assert_eq!(one(&st, k, &id, "SELECT count(*) FROM users").await, "500");

    // 超过单事务上限:分批提交
    exec(&st, k, &id, "CREATE TABLE logs (id INTEGER PRIMARY KEY, message TEXT, level TEXT, created_at TEXT)").await;
    let (_, p) = generate(&st, k, &id, "logs", 60_000, 1).await;
    assert!(p.batched);
    assert_eq!(p.committed, 60_000);
    assert_eq!(one(&st, k, &id, "SELECT count(*) FROM logs").await, "60000");

    // INSERT 文本:小批量可复制
    let mut plan = api::analyze(&st, &k, &id, "users", Locale::Zh).await.unwrap().plan;
    plan.rows = 5;
    let sql = api::insert_script(&st, &k, &id, &plan).await.unwrap();
    assert!(sql.starts_with("INSERT INTO \"users\""), "{sql}");
    plan.rows = 1001;
    assert!(api::insert_script(&st, &k, &id, &plan).await.is_err());
    let _ = std::fs::remove_file(&file);
}

const PG_SETUP: &[&str] = &[
    "DROP TABLE IF EXISTS td_child",
    "DROP TABLE IF EXISTS td_parent",
    "DROP TYPE IF EXISTS td_mood",
    "CREATE TYPE td_mood AS ENUM ('sad', 'ok', 'happy')",
    "CREATE TABLE td_parent (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, code varchar(8) NOT NULL UNIQUE, \
     org text NOT NULL, dept int NOT NULL, UNIQUE (org, dept))",
    "CREATE TABLE td_child (id bigserial PRIMARY KEY, parent_id int NOT NULL REFERENCES td_parent(id), \
     org text, dept int, FOREIGN KEY (org, dept) REFERENCES td_parent(org, dept), mood td_mood NOT NULL, \
     price numeric(6,2) NOT NULL CHECK (price > 0), qty smallint CHECK (qty BETWEEN 1 AND 10), name varchar(12) NOT NULL, \
     status varchar(10) CHECK (status IN ('a', 'b')), payload jsonb, ip inet, uid uuid, raw bytea, active boolean, \
     created_at timestamptz, updated_at timestamptz, total numeric GENERATED ALWAYS AS (price * qty) STORED)",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_enums_precision_limits_identity_and_composite_fk() {
    let Some(port) = std::env::var("DATUM_TEST_PG").ok().and_then(|p| p.parse::<u16>().ok()) else {
        eprintln!("跳过:未设 DATUM_TEST_PG");
        return;
    };
    let st = state();
    let k = DbKind::Pg;
    let id = connect(&st, conn(k, port, "postgres", "postgres", None)).await;
    for s in PG_SETUP {
        exec(&st, k, &id, s).await;
    }
    let a = api::analyze(&st, &k, &id, "td_child", Locale::En).await.unwrap();
    let gen = |n: &str| a.plan.columns.iter().find(|c| c.name == n).unwrap().generator.clone();
    assert_eq!(gen("id"), Generator::Skip);
    assert_eq!(gen("total"), Generator::Skip);
    assert_eq!(gen("mood"), Generator::PickList { values: vec!["sad".into(), "ok".into(), "happy".into()] });
    assert_eq!(gen("org"), Generator::ForeignKey);
    assert_eq!(a.parent_empty.len(), 2);

    generate(&st, k, &id, "td_parent", 200, 3).await;
    assert_eq!(one(&st, k, &id, "SELECT count(DISTINCT code) FROM td_parent WHERE length(code) <= 8").await, "200");
    let (_, p) = generate(&st, k, &id, "td_child", 1000, 4).await;
    assert_eq!(p.committed, 1000);
    assert_eq!(one(&st, k, &id, "SELECT count(*) FROM td_child c JOIN td_parent p ON p.id = c.parent_id").await, "1000");
    assert_eq!(
        one(&st, k, &id, "SELECT count(*) FROM td_child WHERE price <= 0 OR price > 9999.99 OR qty NOT BETWEEN 1 AND 10 \
            OR length(name) > 12 OR updated_at < created_at OR (status IS NOT NULL AND status NOT IN ('a','b'))").await,
        "0"
    );
    assert_eq!(one(&st, k, &id, "SELECT count(*) FROM td_child WHERE org IS NOT NULL AND (org, dept) NOT IN (SELECT org, dept FROM td_parent)").await, "0");
    assert_eq!(one(&st, k, &id, "SELECT count(*) FROM td_child WHERE jsonb_typeof(payload) <> 'object'").await, "0");
    // 标识列由数据库编号
    assert_eq!(one(&st, k, &id, "SELECT max(id) FROM td_parent").await, "200");

    // 取消:单事务全部回滚
    let mut plan = a.plan.clone();
    plan.rows = 50_000;
    api::start(&st, &k, &id, &plan).await.unwrap();
    api::cancel(&st, &id);
    let p = wait(&st, &id).await;
    assert!(p.cancelled);
    assert_eq!(one(&st, k, &id, "SELECT count(*) FROM td_child").await, "1000");
    for s in ["DROP TABLE td_child", "DROP TABLE td_parent", "DROP TYPE td_mood"] {
        exec(&st, k, &id, s).await;
    }
}

const MY_SETUP: &[&str] = &[
    "CREATE DATABASE IF NOT EXISTS datum_testdata",
    "USE datum_testdata",
    "DROP TABLE IF EXISTS td_child",
    "DROP TABLE IF EXISTS td_parent",
    "CREATE TABLE td_parent (id INT AUTO_INCREMENT PRIMARY KEY, email VARCHAR(40) NOT NULL UNIQUE, name VARCHAR(30) NOT NULL)",
    "CREATE TABLE td_child (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, parent_id INT NOT NULL, \
     state ENUM('new','paid','void') NOT NULL, price DECIMAL(5,2) NOT NULL, code VARCHAR(6) NOT NULL, \
     is_active TINYINT(1) NOT NULL, rating TINYINT CHECK (rating BETWEEN 1 AND 5), doc JSON, raw VARBINARY(16), \
     created_at DATETIME NOT NULL, updated_at DATETIME, total DECIMAL(9,2) AS (price * 2) VIRTUAL, \
     FOREIGN KEY (parent_id) REFERENCES td_parent(id))",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mysql_enums_precision_limits_and_auto_increment() {
    let Some(port) = std::env::var("DATUM_TEST_MYSQL").ok().and_then(|p| p.parse::<u16>().ok()) else {
        eprintln!("跳过:未设 DATUM_TEST_MYSQL");
        return;
    };
    let st = state();
    let k = DbKind::Mysql;
    let boot = connect(&st, conn(k, port, "root", "", None)).await;
    exec(&st, k, &boot, "CREATE DATABASE IF NOT EXISTS datum_testdata").await;
    let id = connect(&st, conn(k, port, "root", "datum_testdata", None)).await;
    for s in &MY_SETUP[2..] {
        exec(&st, k, &id, s).await;
    }
    let a = api::analyze(&st, &k, &id, "td_child", Locale::En).await.unwrap();
    let gen = |n: &str| a.plan.columns.iter().find(|c| c.name == n).unwrap().generator.clone();
    assert_eq!(gen("id"), Generator::Skip);
    assert_eq!(gen("total"), Generator::Skip);
    assert_eq!(gen("state"), Generator::PickList { values: vec!["new".into(), "paid".into(), "void".into()] });
    assert_eq!(gen("is_active"), Generator::Boolean);
    assert_eq!(gen("rating"), Generator::Integer { min: 1, max: 5 });

    generate(&st, k, &id, "td_parent", 300, 5).await;
    assert_eq!(one(&st, k, &id, "SELECT count(DISTINCT email) FROM td_parent").await, "300");
    generate(&st, k, &id, "td_child", 1000, 6).await;
    assert_eq!(one(&st, k, &id, "SELECT count(*) FROM td_child c JOIN td_parent p ON p.id = c.parent_id").await, "1000");
    assert_eq!(
        one(&st, k, &id, "SELECT count(*) FROM td_child WHERE price > 999.99 OR char_length(code) > 6 OR is_active NOT IN (0,1) \
            OR (rating IS NOT NULL AND rating NOT BETWEEN 1 AND 5) OR updated_at < created_at OR length(raw) > 16 \
            OR (doc IS NOT NULL AND JSON_TYPE(doc) <> 'OBJECT')").await,
        "0"
    );
    assert_eq!(one(&st, k, &id, "SELECT count(DISTINCT state) <= 3 FROM td_child").await, "1");

    // 中文语言:人名不含 ASCII
    let mut plan = api::analyze(&st, &k, &id, "td_parent", Locale::Zh).await.unwrap().plan;
    plan.rows = 20;
    let pv = api::preview(&st, &k, &id, &plan).await.unwrap();
    let ni = pv.columns.iter().position(|c| c == "name").unwrap();
    assert!(pv.rows.iter().all(|r| r[ni].as_deref().is_some_and(|s| !s.is_ascii())), "{:?}", pv.rows);
    exec(&st, k, &id, "DROP DATABASE datum_testdata").await;
}
