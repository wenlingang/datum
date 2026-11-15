//! MySQL 编辑器会话:同一连接上的语句固定在一条会话里(事务、会话变量、临时表有效),
//! 只读辅助查询不破坏用户事务,元数据不排在会话上的长查询后面。
//! 需要本机 MySQL,默认忽略:
//!   DATUM_TEST_MYSQL=端口(用户 root、空密码、库 datum_test)
//!   cargo test -p datum-core --test my_session -- --include-ignored

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MY: DbKind = DbKind::Mysql;

async fn connect(st: &AppState) -> String {
    let port: u16 = std::env::var("DATUM_TEST_MYSQL").expect("DATUM_TEST_MYSQL=端口").parse().unwrap();
    let id = format!("mysess-{}", uuid::Uuid::new_v4());
    let c = Connection {
        id: id.clone(),
        name: "mysess".into(),
        kind: MY,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        user: "root".into(),
        database: "datum_test".into(),
        plaintext_password: None,
        file_path: None,
        ssl_mode: SslMode::Disable,
        ssh: None,
    };
    commands::save_connection(st, c, Some(String::new())).unwrap();
    service::connect(st, &MY, &id).await.unwrap();
    id
}

fn state() -> Arc<AppState> {
    let dir = std::env::temp_dir().join(format!("datum-mysess-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    Arc::new(AppState::new(dir))
}

async fn exec(st: &AppState, id: &str, sql: &str) {
    service::query(st, &MY, id, sql, None).await.unwrap();
}

async fn scalar(st: &AppState, id: &str, sql: &str) -> String {
    service::query(st, &MY, id, sql, None).await.unwrap().rows[0][0].clone().unwrap()
}

async fn fresh_table(st: &AppState, id: &str) -> String {
    let t = format!("mysess_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    exec(st, id, &format!("CREATE TABLE {t} (id INT PRIMARY KEY, v VARCHAR(10))")).await;
    exec(st, id, &format!("INSERT INTO {t} VALUES (1, 'a'), (2, 'b')")).await;
    t
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn editor_transaction_spans_statements() {
    let st = state();
    let id = connect(&st).await;
    let t = fresh_table(&st, &id).await;
    // 会话变量与临时表在后续语句里仍有效
    exec(&st, &id, "SET @datum_x = 42").await;
    assert_eq!(scalar(&st, &id, "SELECT @datum_x").await, "42");
    exec(&st, &id, "CREATE TEMPORARY TABLE mysess_tmp (x INT)").await;
    exec(&st, &id, "INSERT INTO mysess_tmp VALUES (1)").await;

    exec(&st, &id, "BEGIN").await;
    exec(&st, &id, &format!("UPDATE {t} SET v = 'x' WHERE id = 1")).await;
    // 写语句执行前的影响预估 / 撤销快照:看得到事务里的改动,不结束事务
    let impact = service::write_impact(&st, &MY, &id, &format!("UPDATE {t} SET v = 'y' WHERE id = 1")).await.unwrap().unwrap();
    assert_eq!(impact.count, 1);
    let prep = service::undo_prepare(&st, &MY, &id, &format!("UPDATE {t} SET v = 'y' WHERE id = 1")).await.unwrap();
    assert!(prep.statements.join(";").contains("'x'"), "{:?}", prep.statements);
    // 免确认只读执行看得到未提交的改动
    let r = service::query_read_only(&st, &MY, &id, &format!("SELECT v FROM {t} WHERE id = 1"), None).await.unwrap();
    assert_eq!(r.rows[0][0].as_deref(), Some("x"));
    exec(&st, &id, &format!("UPDATE {t} SET v = 'y' WHERE id = 2")).await;
    exec(&st, &id, "ROLLBACK").await;
    assert_eq!(scalar(&st, &id, &format!("SELECT v FROM {t} WHERE id = 1")).await, "a");
    assert_eq!(scalar(&st, &id, &format!("SELECT v FROM {t} WHERE id = 2")).await, "b");

    // 不在事务里:只读执行由服务器拒绝写入,之后会话恢复读写
    assert!(service::query_read_only(&st, &MY, &id, &format!("DELETE FROM {t}"), None).await.is_err());
    exec(&st, &id, &format!("INSERT INTO {t} VALUES (3, 'c')")).await;
    assert_eq!(scalar(&st, &id, &format!("SELECT COUNT(*) FROM {t}")).await, "3");
    exec(&st, &id, &format!("DROP TABLE {t}")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn metadata_does_not_wait_for_session() {
    let st = state();
    let id = connect(&st).await;
    let slow = {
        let (st, id) = (st.clone(), id.clone());
        tokio::spawn(async move { service::query(&st, &MY, &id, "SELECT SLEEP(3)", None).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let t0 = Instant::now();
    service::list_objects(&st, &MY, &id).await.unwrap();
    service::fetch_columns(&st, &MY, &id).await.unwrap();
    service::diag_run(&st, &MY, &id, "sessions").await.unwrap();
    assert!(t0.elapsed() < Duration::from_secs(1), "元数据被会话上的慢查询挡住了 {:?}", t0.elapsed());
    slow.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn session_reconnects_after_being_killed() {
    let st = state();
    let id = connect(&st).await;
    let thread = scalar(&st, &id, "SELECT CONNECTION_ID()").await;
    // 从别的连接把会话杀掉:下一条语句报错,再下一条用新会话
    let other = connect(&st).await;
    service::query_meta(&st, &MY, &other, &format!("KILL {thread}"), None).await.unwrap();
    let _ = service::query(&st, &MY, &id, "SELECT 1", None).await;
    assert_eq!(scalar(&st, &id, "SELECT 1").await, "1");
    assert_ne!(scalar(&st, &id, "SELECT CONNECTION_ID()").await, thread);
}

/// 事务里的只读执行没法让服务器强制只读:放在保存点里执行,语句造成的写入随后撤销。
/// 用视图里调用会写数据的函数模拟静态检查看不出的副作用。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn read_only_in_transaction_undoes_hidden_writes() {
    let st = state();
    let id = connect(&st).await;
    let t = fresh_table(&st, &id).await;
    let (f, v) = (format!("{t}_f"), format!("{t}_v"));
    exec(&st, &id, "SET GLOBAL log_bin_trust_function_creators = 1").await;
    exec(&st, &id, &format!("CREATE FUNCTION {f}() RETURNS INT MODIFIES SQL DATA BEGIN INSERT INTO {t} VALUES (99, 'side'); RETURN 1; END")).await;
    exec(&st, &id, &format!("CREATE VIEW {v} AS SELECT {f}() AS x")).await;

    // 不在事务里:服务器拒绝
    assert!(service::query_read_only(&st, &MY, &id, &format!("SELECT * FROM {v}"), None).await.is_err());
    assert_eq!(scalar(&st, &id, &format!("SELECT COUNT(*) FROM {t}")).await, "2");

    // 在事务里:语句能执行(看得到事务里的改动),但它的写入被撤销,用户自己的改动保留
    exec(&st, &id, "BEGIN").await;
    exec(&st, &id, &format!("UPDATE {t} SET v = 'x' WHERE id = 1")).await;
    let _ = service::query_read_only(&st, &MY, &id, &format!("SELECT * FROM {v}"), None).await;
    assert_eq!(scalar(&st, &id, &format!("SELECT COUNT(*) FROM {t}")).await, "2", "函数里的 INSERT 应被撤销");
    assert_eq!(scalar(&st, &id, &format!("SELECT v FROM {t} WHERE id = 1")).await, "x", "用户的改动应保留");
    // 限时版本(AI、影响预估走这条)同样
    let _ = service::query_read_only_timeout(&st, &MY, &id, &format!("SELECT * FROM {v}"), 10, 5000).await;
    assert_eq!(scalar(&st, &id, &format!("SELECT COUNT(*) FROM {t}")).await, "2");
    exec(&st, &id, "ROLLBACK").await;
    assert_eq!(scalar(&st, &id, &format!("SELECT v FROM {t} WHERE id = 1")).await, "a");

    exec(&st, &id, &format!("DROP VIEW {v}")).await;
    exec(&st, &id, &format!("DROP FUNCTION {f}")).await;
    exec(&st, &id, &format!("DROP TABLE {t}")).await;
}
