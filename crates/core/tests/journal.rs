//! 写操作留痕:执行写语句 → 记进写日志(带撤销数据)→ 换一个 AppState(相当于重启)→ 从写日志撤销,数据回到原样。
//! SQLite 默认就跑;PG / MySQL / Redis 需要本机服务,默认忽略:
//!   DATUM_TEST_PG=端口      PG(用户 postgres、库 postgres、空密码)
//!   DATUM_TEST_MYSQL=端口   MySQL(用户 root、空密码,会建库 datum_journal_test)
//!   DATUM_TEST_REDIS=端口   Redis
//!   cargo test -p datum-core --test journal -- --include-ignored --test-threads=1

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::core::journal::{JournalEntry, JournalFilter, UndoData};
use datum_core::pg::edit::{CellEdit, RowDelete};
use datum_core::service;
use std::path::PathBuf;

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("datum-journal-it-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn conn(kind: DbKind, port: u16, user: &str, db: &str, file: Option<String>) -> Connection {
    Connection {
        id: format!("journal-{kind:?}-{}", uuid::Uuid::new_v4()),
        name: "journal".into(),
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

async fn exec(st: &AppState, kind: DbKind, id: &str, sql: &str) -> Option<u64> {
    service::query(st, &kind, id, sql, None).await.unwrap().affected
}

async fn dump(st: &AppState, kind: DbKind, id: &str) -> Vec<Vec<Option<String>>> {
    service::query(st, &kind, id, "SELECT id, status, note FROM journal_items ORDER BY id", None).await.unwrap().rows
}

/// 照界面的做法:执行前抓撤销,执行后记一条(这里把会话改成「上次运行」的)。
async fn run_logged(st: &AppState, kind: DbKind, id: &str, sql: &str, source: &str, batch: Option<&str>) -> String {
    let prep = service::undo_prepare(st, &kind, id, sql).await.unwrap();
    assert!(prep.reason.is_none(), "{sql}: {:?}", prep.reason);
    let affected = exec(st, kind, id, sql).await;
    let entry = JournalEntry {
        session: "previous-run".into(),
        conn_id: id.into(),
        conn_name: "journal".into(),
        env: "local".into(),
        kind: format!("{kind:?}").to_lowercase(),
        source: source.into(),
        batch: batch.map(String::from),
        sql: sql.into(),
        affected,
        ok: true,
        undo: Some(UndoData::from(&prep)),
        ..Default::default()
    };
    commands::journal_append(st, entry).unwrap().unwrap()
}

/// 从写日志撤销(界面「撤销…」的做法):取完整记录 → 核对 → 执行反向语句 → 记一条撤销。
async fn undo_from_journal(st: &AppState, kind: DbKind, id: &str, entry_id: &str) {
    let detail = commands::journal_get(st, id, entry_id).unwrap();
    let undo = detail.entry.undo.clone().expect("有撤销数据");
    service::import_sql(st, &kind, id, undo.statements.clone()).await.unwrap();
    let entry = JournalEntry {
        conn_id: id.into(),
        source: "undo".into(),
        sql: undo.statements.join(";\n"),
        affected: Some(undo.rows as u64),
        ok: true,
        undoes: vec![entry_id.into()],
        ..Default::default()
    };
    commands::journal_append(st, entry).unwrap();
}

async fn check_restart_round_trip(dir: PathBuf, c: Connection) {
    let kind = c.kind;
    let id = c.id.clone();
    // 第一次运行:建表、执行写语句并留痕
    let (single, multi_a, multi_b, before) = {
        let st = AppState::new(dir.clone());
        commands::save_connection(&st, c, Some(String::new())).unwrap();
        service::connect(&st, &kind, &id).await.unwrap();
        exec(&st, kind, &id, "DROP TABLE IF EXISTS journal_items").await;
        exec(&st, kind, &id, "CREATE TABLE journal_items (id INT PRIMARY KEY, status VARCHAR(20), note VARCHAR(50))").await;
        exec(&st, kind, &id, "INSERT INTO journal_items VALUES (1,'paid',NULL),(2,'paid','it''s'),(3,'cancelled','x'),(4,'cancelled',NULL)").await;
        let before = dump(&st, kind, &id).await;
        let single = run_logged(&st, kind, &id, "UPDATE journal_items SET status = 'archived' WHERE status = 'paid'", "editor", None).await;
        // 一次执行两条:各记一条,同属一批
        let a = run_logged(&st, kind, &id, "UPDATE journal_items SET note = 'n' WHERE id = 3", "editor", Some("b1")).await;
        let b = run_logged(&st, kind, &id, "DELETE FROM journal_items WHERE id = 4", "editor", Some("b1")).await;
        service::disconnect(&st, &kind, &id).await;
        (single, a, b, before)
    };

    // 「重启」:新的 AppState,同一个配置目录
    let st = AppState::new(dir.clone());
    service::connect(&st, &kind, &id).await.unwrap();
    let list = commands::journal_list(&st, &id, &JournalFilter::default());
    assert_eq!(list.len(), 3);
    assert!(list.iter().all(|e| e.previous_session));
    assert_eq!(list[0].id, multi_b, "最新在前");
    assert_eq!(list.iter().filter(|e| e.batch.as_deref() == Some("b1")).count(), 2);
    let single_summary = list.iter().find(|e| e.id == single).unwrap();
    assert_eq!(single_summary.undo_rows, Some(2));
    assert_eq!(single_summary.affected, Some(2));

    // 详情:改动前的行与预览
    let d = commands::journal_get(&st, &id, &single).unwrap();
    let snap = &d.entry.undo.as_ref().unwrap().snapshots[0];
    assert_eq!(snap.total, 2);
    assert!(!snap.delete);
    assert_eq!(snap.rows[1][2].as_deref(), Some("it's"), "预览是给人看的原值");
    assert_eq!(snap.primary_key, vec!["id".to_string()]);
    // 之后的两条改的也是这张表
    assert_eq!(d.later_writes, 2);

    // 撤销前核对:行都还在,没有冲突
    let check = service::journal_undo_check(&st, &kind, &id, &multi_b).await.unwrap();
    assert!(check.previous_session && check.checked, "{check:?}");
    assert_eq!((check.missing, check.existing), (0, 0));

    // 倒序撤销这一批,再撤销第一条:数据回到原样
    undo_from_journal(&st, kind, &id, &multi_b).await;
    undo_from_journal(&st, kind, &id, &multi_a).await;
    undo_from_journal(&st, kind, &id, &single).await;
    assert_eq!(dump(&st, kind, &id).await, before, "重启后从写日志撤销,数据应回到原样");
    let list = commands::journal_list(&st, &id, &JournalFilter::default());
    for orig in [&single, &multi_a, &multi_b] {
        assert!(list.iter().find(|e| &e.id == orig).unwrap().undone_by.is_some(), "{orig} 应标为已撤销");
    }
    assert!(commands::journal_undoable(&st, &id).is_empty());
    assert_eq!(list.iter().filter(|e| e.source == "undo").count(), 3);

    // 冲突核对:DELETE 掉的行又被插回来了 / UPDATE 过的行被删掉了
    let del = run_logged(&st, kind, &id, "DELETE FROM journal_items WHERE id = 1", "editor", None).await;
    exec(&st, kind, &id, "INSERT INTO journal_items VALUES (1,'again',NULL)").await;
    let check = service::journal_undo_check(&st, &kind, &id, &del).await.unwrap();
    assert_eq!(check.existing, 1, "{check:?}");
    let upd = run_logged(&st, kind, &id, "UPDATE journal_items SET note = 'z' WHERE id IN (2, 3)", "editor", None).await;
    exec(&st, kind, &id, "DELETE FROM journal_items WHERE id = 3").await;
    let check = service::journal_undo_check(&st, &kind, &id, &upd).await.unwrap();
    assert_eq!(check.missing, 1, "{check:?}");

    // 表格编辑:暂存的改动写成等价 SQL 抓快照,提交后可撤销
    exec(&st, kind, &id, "DELETE FROM journal_items").await;
    exec(&st, kind, &id, "INSERT INTO journal_items VALUES (1,'a',NULL),(2,'b','x')").await;
    let before = dump(&st, kind, &id).await;
    let edits = vec![CellEdit { table: "journal_items".into(), pk_col: "id".into(), pk_value: "1".into(), column: "status".into(), new_value: "edited".into() }];
    let deletes = vec![RowDelete { table: "journal_items".into(), pk_col: "id".into(), pk_value: "2".into() }];
    let (stmts, prep) = service::grid_undo_prepare(&st, &kind, &id, &edits, &deletes).await.unwrap();
    assert_eq!(stmts.len(), 2);
    assert!(prep.reason.is_none(), "{:?}", prep.reason);
    assert_eq!(prep.rows, 2);
    service::commit_edits(&st, &kind, &id, edits, deletes).await.unwrap();
    assert_ne!(dump(&st, kind, &id).await, before);
    service::import_sql(&st, &kind, &id, prep.statements).await.unwrap();
    assert_eq!(dump(&st, kind, &id).await, before, "表格编辑撤销后应回到原样");

    exec(&st, kind, &id, "DROP TABLE journal_items").await;
    service::disconnect(&st, &kind, &id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_undo_after_restart() {
    let dir = temp_dir();
    let file = dir.join("journal.db");
    rusqlite::Connection::open(&file).unwrap();
    check_restart_round_trip(dir.clone(), conn(DbKind::Sqlite, 0, "", "", Some(file.display().to_string()))).await;
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_undo_after_restart() {
    let Some(port) = std::env::var("DATUM_TEST_PG").ok().and_then(|p| p.parse().ok()) else {
        return;
    };
    let dir = temp_dir();
    check_restart_round_trip(dir.clone(), conn(DbKind::Pg, port, "postgres", "postgres", None)).await;
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_undo_after_restart() {
    let Some(port) = std::env::var("DATUM_TEST_MYSQL").ok().and_then(|p| p.parse().ok()) else {
        return;
    };
    let dir = temp_dir();
    {
        let st = AppState::new(dir.clone());
        let boot = conn(DbKind::Mysql, port, "root", "", None);
        let bid = boot.id.clone();
        commands::save_connection(&st, boot, Some(String::new())).unwrap();
        service::connect(&st, &DbKind::Mysql, &bid).await.unwrap();
        exec(&st, DbKind::Mysql, &bid, "CREATE DATABASE IF NOT EXISTS datum_journal_test").await;
    }
    check_restart_round_trip(dir.clone(), conn(DbKind::Mysql, port, "root", "datum_journal_test", None)).await;
    std::fs::remove_dir_all(dir).ok();
}

/// 写日志关掉:执行照常,什么都不写;只关「保存行数据」:记语句,不留撤销数据。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_off_writes_nothing_or_no_rows() {
    let dir = temp_dir();
    let st = AppState::new(dir.clone());
    let file = dir.join("s.db");
    rusqlite::Connection::open(&file).unwrap();
    let c = conn(DbKind::Sqlite, 0, "", "", Some(file.display().to_string()));
    let id = c.id.clone();
    commands::save_connection(&st, c, Some(String::new())).unwrap();
    service::connect(&st, &DbKind::Sqlite, &id).await.unwrap();
    exec(&st, DbKind::Sqlite, &id, "CREATE TABLE journal_items (id INT PRIMARY KEY, status TEXT, note TEXT)").await;
    exec(&st, DbKind::Sqlite, &id, "INSERT INTO journal_items VALUES (1,'secret-value',NULL)").await;

    let mut s = commands::journal_settings(&st);
    s.enabled = false;
    commands::journal_save_settings(&st, &s).unwrap();
    run_logged_none(&st, &id).await;
    assert!(commands::journal_list(&st, &id, &JournalFilter::default()).is_empty());
    assert!(!dir.join("journal").exists());

    s.enabled = true;
    s.store_rows = false;
    commands::journal_save_settings(&st, &s).unwrap();
    let prep = service::undo_prepare(&st, &DbKind::Sqlite, &id, "DELETE FROM journal_items").await.unwrap();
    exec(&st, DbKind::Sqlite, &id, "DELETE FROM journal_items").await;
    let jid = commands::journal_append(
        &st,
        JournalEntry { conn_id: id.clone(), source: "editor".into(), sql: "DELETE FROM journal_items".into(), ok: true, undo: Some(UndoData::from(&prep)), ..Default::default() },
    )
    .unwrap()
    .unwrap();
    let d = commands::journal_get(&st, &id, &jid).unwrap();
    assert!(d.entry.undo.is_none() && d.entry.undo_reason.is_some());
    let raw = std::fs::read_to_string(dir.join("journal").join(format!("{id}.jsonl"))).unwrap();
    assert!(!raw.contains("secret-value"), "关掉保存行数据后文件里不能有改动前的行");
    std::fs::remove_dir_all(dir).ok();
}

async fn run_logged_none(st: &AppState, id: &str) {
    exec(st, DbKind::Sqlite, id, "UPDATE journal_items SET note = 'x'").await;
    let r = commands::journal_append(st, JournalEntry { conn_id: id.into(), sql: "UPDATE journal_items SET note = 'x'".into(), ok: true, ..Default::default() }).unwrap();
    assert!(r.is_none());
}

/// Redis 的写命令也记(没有撤销数据);真实 Redis 可选。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_entry() {
    let dir = temp_dir();
    let st = AppState::new(dir.clone());
    let id = "redis-journal";
    if let Some(port) = std::env::var("DATUM_TEST_REDIS").ok().and_then(|p| p.parse::<u16>().ok()) {
        let mut c = conn(DbKind::Redis, port, "", "0", None);
        c.id = id.into();
        commands::save_connection(&st, c, Some(String::new())).unwrap();
        service::connect(&st, &DbKind::Redis, id).await.unwrap();
        service::redis_exec(&st, id, "SET datum:journal:k v").await.unwrap();
        service::redis_exec(&st, id, "DEL datum:journal:k").await.unwrap();
    }
    commands::journal_append(
        &st,
        JournalEntry { conn_id: id.into(), kind: "redis".into(), source: "redis".into(), sql: "SET datum:journal:k v".into(), ok: true, ..Default::default() },
    )
    .unwrap();
    let list = commands::journal_list(&st, id, &JournalFilter { source: Some("redis".into()), ..Default::default() });
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].undo_rows, None);
    let md = commands::journal_export(&st, id, &JournalFilter::default(), "md");
    assert!(md.contains("```\nSET datum:journal:k v\n```"), "{md}");
    commands::journal_clear(&st, Some(id)).unwrap();
    assert!(commands::journal_list(&st, id, &JournalFilter::default()).is_empty());
    std::fs::remove_dir_all(dir).ok();
}
