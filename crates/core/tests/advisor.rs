//! 索引建议的真实执行。SQLite 默认就跑;PG / MySQL 需要本机数据库,默认忽略:
//!   DATUM_TEST_PG=端口         装有 hypopg 扩展包的 PG(用户 postgres、免密;会建库 datum_adv_test,用完删掉)
//!     FROM postgres:16 + apt-get install postgresql-16-hypopg
//!   DATUM_TEST_PG_PLAIN=端口   没有 hypopg 扩展包的 PG(可选:测「服务器上没有 hypopg」)
//!   DATUM_TEST_MYSQL=端口      MySQL 8(root、空密码;会建库 datum_adv_test,用完删掉)
//!   cargo test -p datum-core --test advisor -- --include-ignored --test-threads=1

use datum_core::advisor::{self, Mode, Options, Source, Verdict};
use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-adv-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

async fn connect(st: &AppState, kind: DbKind, port: u16, user: &str, db: &str, file: Option<String>, env: Env) -> String {
    let id = format!("adv-{kind:?}-{}", uuid::Uuid::new_v4());
    let c = Connection {
        id: id.clone(),
        name: "adv".into(),
        kind,
        env,
        host: "127.0.0.1".into(),
        port,
        user: user.into(),
        database: db.into(),
        plaintext_password: None,
        file_path: file,
        ssl_mode: SslMode::Disable,
        ssh: None,
    };
    commands::save_connection(st, c, Some(String::new())).unwrap();
    service::connect(st, &kind, &id).await.unwrap();
    id
}

async fn exec(st: &AppState, kind: DbKind, id: &str, sql: &str) {
    service::query(st, &kind, id, sql, None).await.unwrap_or_else(|e| panic!("{sql}: {}", e.display()));
}

async fn scalar(st: &AppState, kind: DbKind, id: &str, sql: &str) -> String {
    let r = service::query_meta(st, &kind, id, sql, None).await.unwrap_or_else(|e| panic!("{sql}: {}", e.display()));
    r.rows.first().and_then(|r| r.first().cloned().flatten()).unwrap_or_default()
}

const Q: &str = "SELECT o.id, o.total FROM adv_orders o WHERE o.status = 'paid' AND o.customer_id = 42 ORDER BY o.created_at DESC LIMIT 20";

// ------------------------------------------------------------ SQLite

fn sqlite_file(st: &AppState, rows: u32) -> String {
    let file = st.config_dir.join("adv.db");
    let c = rusqlite::Connection::open(&file).unwrap();
    c.execute_batch(
        "CREATE TABLE adv_orders (id INTEGER PRIMARY KEY, customer_id INT, status TEXT, total REAL, created_at TEXT);
         CREATE TABLE adv_customers (id INTEGER PRIMARY KEY, name TEXT, country TEXT);
         CREATE INDEX adv_customers_country ON adv_customers (country);",
    )
    .unwrap();
    c.execute_batch(&format!(
        "WITH RECURSIVE g(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM g WHERE n < {rows})
         INSERT INTO adv_orders SELECT n, n % 500, CASE n % 4 WHEN 0 THEN 'paid' ELSE 'open' END, n * 1.5, date('2026-01-01', '+' || (n % 300) || ' days') FROM g;
         WITH RECURSIVE g(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM g WHERE n < 500)
         INSERT INTO adv_customers SELECT n, 'c' || n, CASE n % 3 WHEN 0 THEN 'KE' ELSE 'PA' END FROM g;"
    ))
    .unwrap();
    file.display().to_string()
}

fn schema_of(file: &str) -> Vec<String> {
    let c = rusqlite::Connection::open(file).unwrap();
    let mut s = c.prepare("SELECT type || ':' || name || ':' || coalesce(sql, '') FROM sqlite_master ORDER BY name").unwrap();
    s.query_map([], |r| r.get::<_, String>(0)).unwrap().map(|r| r.unwrap()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_verifies_in_rolled_back_transaction() {
    let st = state();
    let file = sqlite_file(&st, 5000);
    let before = schema_of(&file);
    let id = connect(&st, DbKind::Sqlite, 0, "", "", Some(file.clone()), Env::Local).await;
    let a = advisor::advise(&st, &DbKind::Sqlite, &id, Q, &Options::default()).await.unwrap();
    assert_eq!(a.mode, Mode::SqliteTxn);
    assert!(a.baseline.as_ref().and_then(|p| p.tree.as_ref()).is_some());
    let first = &a.candidates[0];
    assert_eq!(first.candidate.table, "adv_orders");
    assert_eq!(first.candidate.cols(), vec!["status", "customer_id", "created_at"]);
    assert_eq!(first.candidate.source, Source::Rule);
    let v = first.verify.as_ref().expect("验证过");
    assert_eq!(v.verdict, Verdict::Helps, "{v:?}");
    assert!(v.uses_index);
    assert!(v.full_scans_after < v.full_scans_before);
    let after_plan = format!("{:?}", v.plan.as_ref().unwrap().tree);
    assert!(after_plan.contains("idx_adv_orders_status_customer_id_created_at"), "计划里换成建议名: {after_plan}");
    assert!(!after_plan.contains("datum_advisor_"));
    // 什么都没留下;连接回到自动提交
    assert_eq!(schema_of(&file), before);
    exec(&st, DbKind::Sqlite, &id, "CREATE TABLE after_check (x int)").await;

    // 已有索引覆盖的列不再建议
    let a = advisor::advise(&st, &DbKind::Sqlite, &id, "SELECT * FROM adv_customers WHERE country = 'KE'", &Options::default()).await.unwrap();
    assert!(a.candidates.is_empty(), "{:?}", a.candidates);

    // JOIN:连接列 + 两张表的候选,组合验证
    let a = advisor::advise(
        &st,
        &DbKind::Sqlite,
        &id,
        "SELECT c.name, o.total FROM adv_customers c JOIN adv_orders o ON o.customer_id = c.id WHERE c.name = 'c7' AND o.status = 'paid'",
        &Options::default(),
    )
    .await
    .unwrap();
    assert!(a.candidates.iter().any(|c| c.candidate.table == "adv_customers" && c.candidate.cols() == vec!["name"]), "{:?}", a.candidates.iter().map(|c| &c.candidate.ddl).collect::<Vec<_>>());
    assert!(a.candidates.iter().any(|c| c.candidate.cols()[0] == "customer_id"));
    assert_eq!(schema_of(&file).len(), before.len() + 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_skips_large_tables_open_transactions_and_read_only_files() {
    let st = state();
    let file = sqlite_file(&st, 3000);
    let id = connect(&st, DbKind::Sqlite, 0, "", "", Some(file.clone()), Env::Local).await;
    let opts = Options { with_ai: false, sqlite_max_rows: 1000 };
    let a = advisor::advise(&st, &DbKind::Sqlite, &id, Q, &opts).await.unwrap();
    let v = a.candidates[0].verify.as_ref().unwrap();
    assert_eq!(v.verdict, Verdict::Skipped);
    assert!(v.message.as_deref().unwrap_or("").contains("adv_orders"));

    exec(&st, DbKind::Sqlite, &id, "BEGIN").await;
    let a = advisor::advise(&st, &DbKind::Sqlite, &id, Q, &Options::default()).await.unwrap();
    assert_eq!(a.mode, Mode::Unverified);
    assert!(a.candidates.iter().all(|c| c.verify.is_none()));
    exec(&st, DbKind::Sqlite, &id, "ROLLBACK").await;

    // 只读文件
    let st2 = state();
    let ro = sqlite_file(&st2, 100);
    let mut perm = std::fs::metadata(&ro).unwrap().permissions();
    perm.set_readonly(true);
    std::fs::set_permissions(&ro, perm).unwrap();
    let id2 = connect(&st2, DbKind::Sqlite, 0, "", "", Some(ro.clone()), Env::Local).await;
    let a = advisor::advise(&st2, &DbKind::Sqlite, &id2, Q, &Options::default()).await.unwrap();
    assert_eq!(a.mode, Mode::Unverified);
    assert!(!a.candidates.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_writes_and_placeholders() {
    let st = state();
    let file = sqlite_file(&st, 10);
    let id = connect(&st, DbKind::Sqlite, 0, "", "", Some(file), Env::Local).await;
    for bad in ["INSERT INTO adv_orders (id) VALUES (1)", "SELECT * FROM adv_orders WHERE id = ?", "SELECT * FROM adv_orders WHERE id = :id", "SELECT 1; SELECT 2"] {
        assert!(advisor::advise(&st, &DbKind::Sqlite, &id, bad, &Options::default()).await.is_err(), "{bad}");
    }
    // UPDATE / DELETE 的 WHERE 也给建议
    let a = advisor::advise(&st, &DbKind::Sqlite, &id, "DELETE FROM adv_orders WHERE customer_id = 3", &Options::default()).await.unwrap();
    assert_eq!(a.candidates[0].candidate.cols(), vec!["customer_id"]);
    assert_eq!(service::query(&st, &DbKind::Sqlite, &id, "SELECT count(*) FROM adv_orders", None).await.unwrap().rows[0][0].as_deref(), Some("10"), "DELETE 没有执行");
}

// ------------------------------------------------------------ AI(本地 mock)

/// 起一个 OpenAI 兼容的 mock:回固定内容,收到的请求体存起来。
async fn ai_mock(reply: &str) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(vec![]));
    let (log2, reply) = (log.clone(), reply.to_string());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let mut buf = vec![];
            let mut tmp = [0u8; 8192];
            let head_end = loop {
                let n = sock.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break p + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
            let len: usize = head.lines().find_map(|l| l.strip_prefix("content-length:")).map(|v| v.trim().parse().unwrap()).unwrap_or(0);
            while buf.len() < head_end + len {
                let n = sock.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            log2.lock().unwrap().push(String::from_utf8_lossy(&buf[head_end..]).to_string());
            let frame = serde_json::json!({"choices": [{"delta": {"content": reply}}]}).to_string();
            let body = format!("data: {frame}\n\ndata: [DONE]\n\n");
            let resp = format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.shutdown().await.ok();
        }
    });
    (format!("http://{addr}"), log)
}

fn ai_settings(st: &AppState, base: &str) {
    let s = format!(
        r#"{{"providers":[{{"id":"mock","name":"Mock","protocol":"openai","baseUrl":"{base}","apiKey":"sk-test","enabled":true,"models":[{{"id":"mock-model","enabled":true}}]}}],"current":{{"providerId":"mock","model":"mock-model"}}}}"#
    );
    std::fs::write(st.config_dir.join("ai_settings.json"), s).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ai_candidates_are_validated_merged_and_masked_on_prod() {
    let reply = r#"{"indexes":[{"table":"adv_orders","columns":["customer_id","created_at DESC"],"reason":"lookup by customer, newest first"},{"table":"adv_orders","columns":["no_such_col"],"reason":"x"}]}"#;
    for env in [Env::Local, Env::Prod] {
        let st = state();
        let (base, log) = ai_mock(reply).await;
        ai_settings(&st, &base);
        let file = sqlite_file(&st, 2000);
        let id = connect(&st, DbKind::Sqlite, 0, "", "", Some(file.clone()), env).await;
        let before = schema_of(&file);
        let a = advisor::advise(&st, &DbKind::Sqlite, &id, Q, &Options { with_ai: true, ..Default::default() }).await.unwrap();
        let ai = a.ai.as_ref().unwrap();
        assert_eq!(ai.error, None);
        assert_eq!(ai.added, 1);
        assert_eq!(ai.dropped, vec!["adv_orders(no_such_col)"]);
        let c = a.candidates.iter().find(|c| c.candidate.source == Source::Ai).expect("AI 候选");
        assert_eq!(c.candidate.ddl, "CREATE INDEX idx_adv_orders_customer_id_created_at ON adv_orders (customer_id, created_at DESC);");
        assert!(c.verify.is_some(), "AI 候选同样验证");
        assert_eq!(schema_of(&file), before);
        let sent = log.lock().unwrap().join("\n");
        assert!(sent.contains("CREATE TABLE"), "带上表结构");
        assert!(sent.contains("existing indexes") || !sent.contains("adv_customers_country"));
        assert!(!sent.contains("Coffee") && !sent.contains("c7"), "不发行数据");
        match env {
            Env::Prod => {
                assert!(ai.masked);
                assert!(!sent.contains("'paid'") && !sent.contains("= 42"), "生产库字面量打码: {sent}");
            }
            _ => {
                assert!(!ai.masked);
                assert!(sent.contains("'paid'"));
            }
        }
    }
    // AI 没配置:规则候选照常,AI 部分报原因
    let st = state();
    let file = sqlite_file(&st, 100);
    let id = connect(&st, DbKind::Sqlite, 0, "", "", Some(file), Env::Local).await;
    let a = advisor::advise(&st, &DbKind::Sqlite, &id, Q, &Options { with_ai: true, ..Default::default() }).await.unwrap();
    assert!(a.ai.unwrap().error.is_some());
    assert!(!a.candidates.is_empty());
}

// ------------------------------------------------------------ PostgreSQL

async fn pg_seed(st: &AppState, port: u16) -> String {
    let admin = connect(st, DbKind::Pg, port, "postgres", "postgres", None, Env::Local).await;
    let _ = service::query(st, &DbKind::Pg, &admin, "DROP DATABASE IF EXISTS datum_adv_test WITH (FORCE)", None).await;
    exec(st, DbKind::Pg, &admin, "CREATE DATABASE datum_adv_test").await;
    let id = connect(st, DbKind::Pg, port, "postgres", "datum_adv_test", None, Env::Local).await;
    exec(
        st,
        DbKind::Pg,
        &id,
        "CREATE TABLE adv_orders (id int PRIMARY KEY, customer_id int, status text, total numeric, created_at timestamptz);
         INSERT INTO adv_orders SELECT g, g % 2000, CASE g % 10 WHEN 0 THEN 'paid' ELSE 'open' END, g, now() - g * interval '1 minute'
           FROM generate_series(1, 50000) g;
         CREATE TABLE adv_customers (id int PRIMARY KEY, name text, country text);
         INSERT INTO adv_customers SELECT g, 'c' || g, CASE g % 3 WHEN 0 THEN 'KE' ELSE 'PA' END FROM generate_series(1, 2000) g;
         ANALYZE;",
    )
    .await;
    id
}

async fn pg_drop(st: &AppState, port: u16) {
    let admin = connect(st, DbKind::Pg, port, "postgres", "postgres", None, Env::Local).await;
    let _ = service::query(st, &DbKind::Pg, &admin, "DROP DATABASE IF EXISTS datum_adv_test WITH (FORCE)", None).await;
}

fn pg_port(var: &str) -> Option<u16> {
    std::env::var(var).ok().and_then(|p| p.parse().ok())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_hypopg_proves_benefit_and_leaves_nothing() {
    let Some(port) = pg_port("DATUM_TEST_PG") else { return };
    let st = state();
    let id = pg_seed(&st, port).await;
    let kind = DbKind::Pg;
    let indexes = "SELECT count(*) FROM pg_indexes WHERE schemaname = 'public'";
    let before = scalar(&st, kind, &id, indexes).await;

    // 扩展包在,但库里没装:给出安装语句,不自动执行
    exec(&st, kind, &id, "DROP EXTENSION IF EXISTS hypopg").await;
    let a = advisor::advise(&st, &kind, &id, Q, &Options::default()).await.unwrap();
    assert_eq!(a.mode, Mode::Unverified);
    assert_eq!(a.install_sql.as_deref(), Some("CREATE EXTENSION hypopg;"));
    assert!(!a.candidates.is_empty() && a.candidates.iter().all(|c| c.verify.is_none()));
    assert!(a.baseline.as_ref().unwrap().tree.as_ref().unwrap().cost.unwrap() > 0.0);
    assert_eq!(scalar(&st, kind, &id, "SELECT count(*) FROM pg_extension WHERE extname = 'hypopg'").await, "0");

    exec(&st, kind, &id, "CREATE EXTENSION hypopg").await;
    let a = advisor::advise(&st, &kind, &id, Q, &Options::default()).await.unwrap();
    assert_eq!(a.mode, Mode::Hypopg, "{}", a.note);
    let first = &a.candidates[0];
    assert_eq!(first.candidate.cols(), vec!["status", "customer_id", "created_at"]);
    assert!(first.candidate.editor_sql.starts_with("CREATE INDEX CONCURRENTLY idx_adv_orders_status_customer_id_created_at ON adv_orders"));
    let v = first.verify.as_ref().unwrap();
    assert_eq!(v.verdict, Verdict::Helps, "{v:?}");
    assert!(v.uses_index);
    assert!(v.cost_after.unwrap() < v.cost_before.unwrap());
    assert!(v.improvement_pct.unwrap() > 50.0, "{v:?}");
    let plan = format!("{:?}", v.plan.as_ref().unwrap().tree);
    assert!(plan.contains("idx_adv_orders_status_customer_id_created_at"), "{plan}");
    assert!(!plan.contains("btree_"), "假想索引名已换掉: {plan}");

    // 什么都没留下:没有真实索引,辅助会话上没有假想索引、不在事务里、限时已恢复
    assert_eq!(scalar(&st, kind, &id, indexes).await, before);
    assert_eq!(scalar(&st, kind, &id, "SELECT count(*) FROM hypopg_list_indexes").await, "0");
    assert_eq!(scalar(&st, kind, &id, "SELECT statement_timestamp() <> transaction_timestamp()").await, "f");
    assert_eq!(scalar(&st, kind, &id, "SHOW statement_timeout").await, "0");
    assert_eq!(scalar(&st, kind, &id, "SHOW transaction_read_only").await, "off");

    // JOIN + 两个有收益的候选:组合验证
    let a = advisor::advise(
        &st,
        &kind,
        &id,
        "SELECT c.name, o.total FROM adv_customers c JOIN adv_orders o ON o.customer_id = c.id WHERE c.name = 'c7' AND o.status = 'paid' AND o.total > 49000",
        &Options::default(),
    )
    .await
    .unwrap();
    assert!(a.candidates.iter().any(|c| c.verify.as_ref().is_some_and(|v| v.verdict == Verdict::Helps)), "{:#?}", a.candidates);
    // 两个都有收益:一起加上再验证一次,并报出计划实际用上了哪个
    let combo = a.combo.as_ref().expect("组合验证");
    assert!(combo.names.len() >= 2);
    assert!(!combo.used.is_empty() && combo.used.iter().all(|u| combo.names.contains(u)));
    assert!(combo.verify.cost_after.unwrap() <= combo.verify.cost_before.unwrap());
    assert_eq!(scalar(&st, kind, &id, "SELECT count(*) FROM hypopg_list_indexes").await, "0");

    // 已有索引覆盖:不再建议
    exec(&st, kind, &id, "CREATE INDEX adv_orders_cust ON adv_orders (customer_id)").await;
    let a = advisor::advise(&st, &kind, &id, "SELECT * FROM adv_orders WHERE customer_id = 3", &Options::default()).await.unwrap();
    assert!(a.candidates.is_empty(), "{:?}", a.candidates);

    // UPDATE 的 WHERE:只 EXPLAIN,不执行
    let a = advisor::advise(&st, &kind, &id, "UPDATE adv_orders SET total = 0 WHERE status = 'paid' AND created_at < now() - interval '30 days'", &Options::default()).await.unwrap();
    assert!(!a.candidates.is_empty());
    assert_eq!(scalar(&st, kind, &id, "SELECT count(*) FROM adv_orders WHERE total = 0").await, "0");
    pg_drop(&st, port).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_without_hypopg_package_is_unverified() {
    let Some(port) = pg_port("DATUM_TEST_PG_PLAIN") else { return };
    let st = state();
    let id = pg_seed(&st, port).await;
    let a = advisor::advise(&st, &DbKind::Pg, &id, Q, &Options::default()).await.unwrap();
    assert_eq!(a.mode, Mode::Unverified);
    assert_eq!(a.install_sql, None);
    assert!(!a.candidates.is_empty());
    assert!(a.baseline.is_some());
    pg_drop(&st, port).await;
}

// ------------------------------------------------------------ MySQL

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_rules_only_with_baseline() {
    let Some(port) = pg_port("DATUM_TEST_MYSQL") else { return };
    let st = state();
    let admin = connect(&st, DbKind::Mysql, port, "root", "", None, Env::Local).await;
    exec(&st, DbKind::Mysql, &admin, "DROP DATABASE IF EXISTS datum_adv_test").await;
    exec(&st, DbKind::Mysql, &admin, "CREATE DATABASE datum_adv_test").await;
    let id = connect(&st, DbKind::Mysql, port, "root", "datum_adv_test", None, Env::Local).await;
    let kind = DbKind::Mysql;
    exec(&st, kind, &id, "CREATE TABLE adv_orders (id INT PRIMARY KEY, customer_id INT, status VARCHAR(10), total DECIMAL(10,2), created_at DATETIME)").await;
    exec(&st, kind, &id, "INSERT INTO adv_orders SELECT n, n % 50, IF(n % 4 = 0, 'paid', 'open'), n, NOW() FROM (SELECT a.n + b.n * 10 + 1 AS n FROM (SELECT 0 n UNION SELECT 1 UNION SELECT 2 UNION SELECT 3 UNION SELECT 4 UNION SELECT 5 UNION SELECT 6 UNION SELECT 7 UNION SELECT 8 UNION SELECT 9) a, (SELECT 0 n UNION SELECT 1 UNION SELECT 2 UNION SELECT 3 UNION SELECT 4 UNION SELECT 5 UNION SELECT 6 UNION SELECT 7 UNION SELECT 8 UNION SELECT 9) b) x").await;
    let count_idx = "SELECT COUNT(*) FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = 'datum_adv_test'";
    let before = scalar(&st, kind, &id, count_idx).await;
    let a = advisor::advise(&st, &kind, &id, Q, &Options::default()).await.unwrap();
    assert_eq!(a.mode, Mode::Unverified);
    assert!(a.note.contains("MySQL"));
    let c = &a.candidates[0];
    assert!(c.verify.is_none());
    assert_eq!(c.candidate.editor_sql, "ALTER TABLE adv_orders ADD INDEX idx_adv_orders_status_customer_id_created_at (status, customer_id, created_at), ALGORITHM=INPLACE, LOCK=NONE;");
    let tree = a.baseline.as_ref().unwrap().tree.as_ref().expect("FORMAT=TREE");
    fn any_full(n: &datum_core::explain::PlanNode) -> bool {
        n.full_scan || n.children.iter().any(any_full)
    }
    assert!(any_full(tree), "基线计划标出全表扫描: {tree:?}");
    assert_eq!(scalar(&st, kind, &id, count_idx).await, before);
    exec(&st, DbKind::Mysql, &admin, "DROP DATABASE datum_adv_test").await;
}
