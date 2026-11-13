//! 关系库诊断的真实执行。SQLite 默认就跑;PG / MySQL 需要本机数据库,默认忽略:
//!   DATUM_DIAG_PG=端口      PG(用户 postgres、库 postgres、免密;开 pg_stat_statements 才测慢 SQL 排行)
//!     docker run --rm -p 55491:5432 -e POSTGRES_HOST_AUTH_METHOD=trust postgres:16-alpine \
//!       -c shared_preload_libraries=pg_stat_statements
//!   DATUM_DIAG_MYSQL=端口   MySQL(用户 root、空密码,会建库 datum_diag_test、datum_charset_test,用完删掉)
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
    assert!(o.rows[0][col(o, "version")].as_deref().is_some_and(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit())));
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
    // PG 15 之前统计由收集进程异步更新:等它反映出来
    wait_for(&st, kind, &id, "bloat", |b| {
        b.rows
            .iter()
            .any(|r| r[col(b, "table")].as_deref() == Some("diag_items") && r[col(b, "last_vacuum")].is_some())
    })
    .await;
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
    assert!(o.rows[0][col(o, "version")].as_deref().is_some_and(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit())));
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

/// 字符集体检:utf8mb3 的库 / 表 / 列、外键两侧与推断的 JOIN 列规则不同、COMPACT 行格式下转 utf8mb4 索引超长。
/// 再验证生成的脚本:改表评估判为 COPY 高风险;不先改行格式时转换确实失败,按脚本改完后 utf8mb3 问题消失。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_charset() {
    let Some(port) = std::env::var("DATUM_DIAG_MYSQL").ok().and_then(|p| p.parse::<u16>().ok()) else {
        return;
    };
    use mysql_async::prelude::Queryable;
    let mut admin = mysql_async::Conn::from_url(format!("mysql://root@127.0.0.1:{port}")).await.unwrap();
    admin.query_drop("DROP DATABASE IF EXISTS datum_charset_test").await.unwrap();
    // 5.7 / MariaDB 只认 utf8_general_ci 这类老名字(8.0 也接受,存成 utf8mb3_*);没有 0900 规则时用 unicode_ci
    admin.query_drop("CREATE DATABASE datum_charset_test CHARACTER SET utf8 COLLATE utf8_general_ci").await.unwrap();
    admin.query_drop("USE datum_charset_test").await.unwrap();
    let has_0900: Option<String> = admin
        .query_first("SELECT COLLATION_NAME FROM information_schema.COLLATIONS WHERE COLLATION_NAME = 'utf8mb4_0900_ai_ci'")
        .await
        .unwrap();
    let new = if has_0900.is_some() { "utf8mb4_0900_ai_ci" } else { "utf8mb4_unicode_ci" };
    for sql in [
        format!(
            "CREATE TABLE users (id INT PRIMARY KEY, code VARCHAR(32) NOT NULL, nick VARCHAR(40) COLLATE utf8mb4_bin, \
             UNIQUE KEY uk_code (code)) CHARACTER SET utf8mb4 COLLATE {new}"
        ),
        format!(
            "CREATE TABLE orders (id INT PRIMARY KEY, \
             user_code VARCHAR(32) CHARACTER SET utf8mb4 COLLATE {new} NOT NULL DEFAULT '', email VARCHAR(255), \
             note VARCHAR(20) CHARACTER SET latin1, KEY idx_email (email), \
             CONSTRAINT fk_orders_user FOREIGN KEY (user_code) REFERENCES users (code)) \
             CHARACTER SET utf8 COLLATE utf8_general_ci ROW_FORMAT=COMPACT"
        ),
        format!("CREATE TABLE accounts (id VARCHAR(36) PRIMARY KEY) CHARACTER SET utf8mb4 COLLATE {new}"),
        format!(
            "CREATE TABLE tags (id INT PRIMARY KEY, account_id VARCHAR(36) COLLATE utf8mb4_general_ci) \
             CHARACTER SET utf8mb4 COLLATE {new}"
        ),
        "CREATE TABLE `odd``name` (id INT PRIMARY KEY, v VARCHAR(10)) CHARACTER SET latin1".to_string(),
    ] {
        admin.query_drop(sql).await.unwrap();
    }
    // 两侧字符集不同的外键建不出来,只能关掉 foreign_key_checks 后改一侧(老库升级、导入时常见的来历);
    // MariaDB 连这样也拒绝(错误 1832),外键两侧不一致在那里造不出来,跳过相关断言
    admin.query_drop("SET foreign_key_checks = 0").await.unwrap();
    let fk_mismatch = admin
        .query_drop("ALTER TABLE orders MODIFY user_code VARCHAR(32) CHARACTER SET utf8 COLLATE utf8_general_ci NOT NULL DEFAULT ''")
        .await
        .is_ok();
    admin.query_drop("SET foreign_key_checks = 1").await.unwrap();

    let st = state();
    let (kind, id) = connect(&st, conn(DbKind::Mysql, port, "root", "datum_charset_test", None)).await;
    let r = service::diag_run(&st, &kind, &id, "charset").await.unwrap();
    assert!(r.unavailable.is_none());
    let (sev, issue, object, suggested, fix) =
        (col(&r, "severity"), col(&r, "issue"), col(&r, "object"), col(&r, "suggested"), col(&r, "fix"));
    let find = |i: &str, o: &str| {
        r.rows
            .iter()
            .find(|row| row[issue].as_deref() == Some(i) && row[object].as_deref() == Some(o))
            .unwrap_or_else(|| panic!("缺 {i} {o}: {:?}", r.rows.iter().map(|x| (&x[issue], &x[object])).collect::<Vec<_>>()))
    };
    assert_eq!(r.rows[0][sev].as_deref(), Some("high"));
    if fk_mismatch {
        find("fk_mismatch", "orders.user_code → users.code");
    }
    assert_eq!(find("join_mismatch", "tags.account_id → accounts.id")[suggested].as_deref(), Some(new));
    find("index_too_long", "orders.idx_email");
    assert_eq!(find("utf8mb3_schema", "datum_charset_test")[suggested].as_deref(), Some("utf8mb4_general_ci"));
    assert_eq!(find("utf8mb3_table", "orders")[suggested].as_deref(), Some("utf8mb4_general_ci"));
    find("utf8mb3_column", "orders.email");
    find("column_collation", "users.nick");
    find("table_collation", "odd`name");
    // 8.0+ 提示 0900 规则,8.0 以前没有
    let note = find("utf8mb3_table", "orders")[col(&r, "note")].clone().unwrap();
    assert_eq!(note.contains("utf8mb4_0900_ai_ci"), has_0900.is_some(), "{note}");

    let script = r.script.clone().expect("有转换脚本");
    let convert = script.lines().find(|l| l.contains("`orders` CONVERT TO CHARACTER SET")).unwrap();
    assert_eq!(convert, "ALTER TABLE `datum_charset_test`.`orders` CONVERT TO CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci;");
    // 执行时经过改表结构风险评估:CONVERT 是 COPY、高风险
    let a = service::alter_assess(&st, &kind, &id, convert).await.unwrap().expect("有评估");
    assert_eq!(a.algorithm, Some(datum_core::sql::alter::Algorithm::Copy));
    assert_eq!(a.risk, datum_core::sql::alter::Risk::High);

    // COMPACT 行格式下真的转不过去(索引 1020 字节 > 767),证明检测不是误报
    admin.query_drop("SET foreign_key_checks = 0").await.unwrap();
    // MariaDB 关了 foreign_key_checks 也不让改外键里的列(错误 1832,脚本注释里有说明):先删外键,改完再加回
    if !fk_mismatch {
        admin.query_drop("ALTER TABLE orders DROP FOREIGN KEY fk_orders_user").await.unwrap();
    }
    let e = admin.query_drop(convert).await.unwrap_err().to_string();
    assert!(e.contains("767"), "{e}");
    // 按建议先改行格式,再把脚本里的语句全部执行一遍(外键两侧在同一会话里关掉检查后一起改)
    let row_fix = find("index_too_long", "orders.idx_email")[fix].clone().unwrap();
    admin.query_drop(row_fix.trim()).await.unwrap();
    for stmt in script.lines().filter(|l| !l.starts_with("--") && !l.trim().is_empty()) {
        admin.query_drop(stmt).await.unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }
    if !fk_mismatch {
        // CONVERT 把外键列也统一成了表的规则,加回前要和被引用列一致(脚本注释列出了会被改掉的列)
        admin
            .query_drop(format!("ALTER TABLE orders MODIFY user_code VARCHAR(32) CHARACTER SET utf8mb4 COLLATE {new} NOT NULL DEFAULT ''"))
            .await
            .unwrap();
        admin
            .query_drop("ALTER TABLE orders ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_code) REFERENCES users (code)")
            .await
            .unwrap();
    }
    if fk_mismatch {
        let fk_fix = find("fk_mismatch", "orders.user_code → users.code")[fix].clone().unwrap();
        for stmt in fk_fix.lines().filter(|l| !l.starts_with("--") && !l.trim().is_empty()) {
            admin.query_drop(stmt).await.unwrap_or_else(|e| panic!("{stmt}: {e}"));
        }
        // MODIFY 保留了 NOT NULL 与默认值
        let def: Option<(String, Option<String>)> = admin
            .query_first(
                "SELECT IS_NULLABLE, COLUMN_DEFAULT FROM information_schema.COLUMNS \
                 WHERE TABLE_SCHEMA = 'datum_charset_test' AND TABLE_NAME = 'orders' AND COLUMN_NAME = 'user_code'",
            )
            .await
            .unwrap();
        assert_eq!(def, Some(("NO".into(), Some(String::new()))));
    }
    admin.query_drop("SET foreign_key_checks = 1").await.unwrap();

    let r = service::diag_run(&st, &kind, &id, "charset").await.unwrap();
    let issues: Vec<(Option<String>, Option<String>)> =
        r.rows.iter().map(|row| (row[col(&r, "issue")].clone(), row[col(&r, "object")].clone())).collect();
    assert!(!issues.iter().any(|(i, _)| i.as_deref().is_some_and(|i| i.starts_with("utf8mb3_"))), "{issues:?}");
    assert!(!issues.iter().any(|(i, _)| i.as_deref() == Some("fk_mismatch")), "{issues:?}");
    assert!(r.script.is_none());

    admin.query_drop("DROP DATABASE datum_charset_test").await.unwrap();
}
