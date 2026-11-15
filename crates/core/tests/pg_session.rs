//! PG 会话安全与并发:
//! - 只读辅助查询(影响预估、撤销快照、免确认执行、AI)不能结束用户在编辑器里开的事务;
//! - 事务内的只读查询看得到事务里未提交的改动,写入被拒绝且事务照常可用;
//! - 一条连接上的慢查询不挡住其他连接,也不挡住本连接的元数据 / 诊断。
//! 需要本机 PG,默认忽略:
//!   DATUM_TEST_PG=端口(用户 postgres、库 postgres、空密码)
//!   cargo test -p datum-core --test pg_session -- --include-ignored

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;
use std::time::{Duration, Instant};

const PG: DbKind = DbKind::Pg;

fn port() -> u16 {
    std::env::var("DATUM_TEST_PG").expect("DATUM_TEST_PG=端口").parse().unwrap()
}

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-pgsess-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

async fn connect(st: &AppState) -> String {
    let id = format!("pgsess-{}", uuid::Uuid::new_v4());
    let c = Connection {
        id: id.clone(),
        name: "pgsess".into(),
        kind: PG,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port: port(),
        user: "postgres".into(),
        database: "postgres".into(),
        plaintext_password: None,
        file_path: None,
        ssl_mode: SslMode::Disable,
        ssh: None,
    };
    commands::save_connection(st, c, Some(String::new())).unwrap();
    service::connect(st, &PG, &id).await.unwrap();
    id
}

async fn exec(st: &AppState, id: &str, sql: &str) {
    service::query(st, &PG, id, sql, None).await.unwrap();
}

async fn scalar(st: &AppState, id: &str, sql: &str) -> String {
    let r = service::query(st, &PG, id, sql, None).await.unwrap();
    r.rows[0][0].clone().unwrap()
}

/// 每个测试用自己的表,可并行跑。
async fn fresh_table(st: &AppState, id: &str) -> String {
    let t = format!("pgsess_{}", uuid::Uuid::new_v4().simple());
    exec(st, id, &format!("CREATE TABLE {t} (id INT PRIMARY KEY, v TEXT)")).await;
    exec(st, id, &format!("INSERT INTO {t} VALUES (1, 'a'), (2, 'b')")).await;
    t
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn read_only_helpers_keep_user_transaction() {
    let st = state();
    let id = connect(&st).await;
    let t = fresh_table(&st, &id).await;

    exec(&st, &id, "BEGIN").await;
    exec(&st, &id, &format!("UPDATE {t} SET v = 'x' WHERE id = 1")).await;

    // 写语句执行前的影响预估与撤销快照(都走只读辅助查询)
    let impact = service::write_impact(&st, &PG, &id, &format!("UPDATE {t} SET v = 'y' WHERE id = 1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(impact.count, 1);
    let prep = service::undo_prepare(&st, &PG, &id, &format!("UPDATE {t} SET v = 'y' WHERE id = 1"))
        .await
        .unwrap();
    // 快照看得到事务里未提交的改动:反向语句把值恢复成 x
    let snap = prep.statements.join("; ");
    assert!(snap.contains("= 'x'"), "快照应是事务内的值 x,实际 {snap}");

    // 免确认的只读执行:看得到未提交的改动
    let r = service::query_read_only(&st, &PG, &id, &format!("SELECT v FROM {t} WHERE id = 1"), None)
        .await
        .unwrap();
    assert_eq!(r.rows[0][0].as_deref(), Some("x"));
    // 写入被拒,但用户事务不受影响
    assert!(service::query_read_only(&st, &PG, &id, &format!("DELETE FROM {t}"), None).await.is_err());

    exec(&st, &id, &format!("UPDATE {t} SET v = 'y' WHERE id = 2")).await;
    exec(&st, &id, "ROLLBACK").await;

    // 用户的 ROLLBACK 撤回了事务里的全部改动
    assert_eq!(scalar(&st, &id, &format!("SELECT v FROM {t} WHERE id = 1")).await, "a");
    assert_eq!(scalar(&st, &id, &format!("SELECT v FROM {t} WHERE id = 2")).await, "b");
    // 会话恢复读写、无残留限时
    assert_eq!(scalar(&st, &id, "SHOW transaction_read_only").await, "off");
    assert_eq!(scalar(&st, &id, "SHOW statement_timeout").await, "0");
    exec(&st, &id, &format!("DROP TABLE {t}")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn read_only_in_aborted_transaction_errors_without_ending_it() {
    let st = state();
    let id = connect(&st).await;
    let t = fresh_table(&st, &id).await;

    exec(&st, &id, "BEGIN").await;
    exec(&st, &id, &format!("UPDATE {t} SET v = 'x' WHERE id = 1")).await;
    assert!(service::query(&st, &PG, &id, "SELECT * FROM no_such_table_pgsess", None).await.is_err());
    // 事务已中止:只读辅助查询报错,不能替用户回滚 / 提交
    assert!(service::query_read_only(&st, &PG, &id, &format!("SELECT v FROM {t}"), None).await.is_err());
    // 仍处于中止状态,普通语句照样报错,直到用户自己 ROLLBACK
    assert!(service::query(&st, &PG, &id, "SELECT 1", None).await.is_err());
    exec(&st, &id, "ROLLBACK").await;
    assert_eq!(scalar(&st, &id, &format!("SELECT v FROM {t} WHERE id = 1")).await, "a");
    exec(&st, &id, &format!("DROP TABLE {t}")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn read_only_outside_transaction_still_rejects_writes() {
    let st = state();
    let id = connect(&st).await;
    let t = fresh_table(&st, &id).await;
    assert!(service::query_read_only(&st, &PG, &id, &format!("DELETE FROM {t}"), None).await.is_err());
    assert!(service::query_read_only(&st, &PG, &id, &format!("COMMIT; DELETE FROM {t}"), None).await.is_err());
    assert_eq!(scalar(&st, &id, &format!("SELECT count(*) FROM {t}")).await, "2");
    // 会话级设置(临时表)对只读执行可见:它跑在用户会话上
    exec(&st, &id, "CREATE TEMP TABLE pgsess_tmp (x INT)").await;
    assert!(service::query_read_only(&st, &PG, &id, "SELECT * FROM pgsess_tmp", None).await.is_ok());
    exec(&st, &id, &format!("DROP TABLE {t}")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn slow_query_does_not_block_others() {
    let st = std::sync::Arc::new(state());
    let a = connect(&st).await;
    let b = connect(&st).await;

    let slow = {
        let (st, a) = (st.clone(), a.clone());
        tokio::spawn(async move { service::query(&st, &PG, &a, "SELECT pg_sleep(3)", None).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 另一条连接
    let t0 = Instant::now();
    service::query(&st, &PG, &b, "SELECT 1", None).await.unwrap();
    assert!(t0.elapsed() < Duration::from_secs(1), "其他连接被慢查询挡住了 {:?}", t0.elapsed());

    // 同一连接的元数据(侧栏表清单、列信息)与诊断
    let t0 = Instant::now();
    service::list_objects(&st, &PG, &a).await.unwrap();
    service::fetch_columns(&st, &PG, &a).await.unwrap();
    service::diag_run(&st, &PG, &a, "sessions").await.unwrap();
    assert!(t0.elapsed() < Duration::from_secs(1), "元数据 / 诊断被本连接的慢查询挡住了 {:?}", t0.elapsed());

    slow.await.unwrap().unwrap();
}
