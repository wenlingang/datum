//! 数据对比在真实库上:识别插入 / 更新 / 删除、复合键、NULL、数值归一、二进制、JSON、上限与取消;
//! 生成的同步脚本在目标库执行后再对比应完全一致。
//! SQLite 默认就跑;PG / MySQL 需要本机数据库,默认忽略:
//!   DATUM_TEST_PG=端口(postgres 用户免密)  DATUM_TEST_MYSQL=端口(root 无密码)
//!   cargo test -p datum-core --test data_diff -- --include-ignored --test-threads=1

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::data_diff::{self, DiffKind, Job, Report, Side, Spec};
use datum_core::service;
use std::sync::Arc;

fn state() -> Arc<AppState> {
    let dir = std::env::temp_dir().join(format!("datum-ddiff-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    Arc::new(AppState::new(dir))
}

async fn connect(st: &AppState, kind: DbKind, port: u16, user: &str, db: &str, file: Option<String>) -> String {
    let id = format!("dd-{kind:?}-{}", uuid::Uuid::new_v4());
    let c = Connection {
        id: id.clone(),
        name: "dd".into(),
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

fn spec(kind: DbKind, src: &str, dst: &str, table: &str) -> Spec {
    let side = |c: &str| Side { conn_id: c.into(), table: table.into() };
    Spec::new(kind, side(src), side(dst))
}

async fn compare(st: &AppState, s: &Spec) -> Report {
    data_diff::run(st, s, &Job::default()).await.unwrap_or_else(|e| panic!("compare: {}", e.display()))
}

fn bin(kind: DbKind, hex: &str) -> String {
    match kind {
        DbKind::Pg => format!("'\\x{hex}'::bytea"),
        _ => format!("X'{hex}'"),
    }
}

/// 主场景:单列主键,覆盖各类差异,分块很小(跨很多页),再脚本往返。
async fn main_scenario(st: &AppState, kind: DbKind, src: &str, dst: &str) {
    let (price_s, price_t, data, doc) = match kind {
        DbKind::Pg => ("NUMERIC(10,2)", "NUMERIC(10,3)", "BYTEA", "JSON"),
        DbKind::Mysql => ("DECIMAL(10,2)", "DECIMAL(10,3)", "VARBINARY(16)", "JSON"),
        _ => ("REAL", "INTEGER", "BLOB", "JSON"),
    };
    let ddl = |price: &str| {
        format!("CREATE TABLE items (id INT PRIMARY KEY, name VARCHAR(50), price {price}, note TEXT, data {data}, doc {doc})")
    };
    exec(st, kind, src, &ddl(price_s)).await;
    exec(st, kind, dst, &format!("{}", ddl(price_t).replace(", doc", ", extra INT, doc"))).await;
    let row = |id: i32, name: &str, price: &str, note: &str, data: &str, doc: &str| {
        format!("({id}, {name}, {price}, {note}, {}, '{doc}')", bin(kind, data))
    };
    let src_rows = [
        row(1, "'a'", "1.0", "'n'", "ff00", r#"{"a":1}"#),
        row(2, "'b '", "2", "'n'", "01", r#"{"a":1}"#),
        row(3, "'c'", "3", "NULL", "01", r#"{"a":1}"#),
        row(4, "'d'", "4", "'n'", "00ff", r#"{"a":1}"#),
        row(5, "'e'", "5", "'n'", "01", r#"{"a":1,"b":[1,2]}"#),
        row(6, "'it''s'", "6.25", "'new'", "", r#"{"x":null}"#),
    ];
    let dst_rows = [
        row(1, "'a'", "1", "'n'", "ff00", r#"{"a":1}"#),
        row(2, "'b'", "2", "'n'", "01", r#"{"a":1}"#),
        row(3, "'c'", "3", "''", "01", r#"{"a":1}"#),
        row(4, "'d'", "4", "'n'", "00fe", r#"{"a":1}"#),
        row(5, "'e'", "5", "'n'", "01", r#"{"b": [1, 2], "a": 1}"#),
        row(7, "'gone'", "7", "'n'", "01", r#"{"a":1}"#),
    ];
    exec(st, kind, src, &format!("INSERT INTO items (id, name, price, note, data, doc) VALUES {}", src_rows.join(", "))).await;
    exec(st, kind, dst, &format!("INSERT INTO items (id, name, price, note, data, doc) VALUES {}", dst_rows.join(", "))).await;

    let ins = data_diff::inspect(st, &kind, &Side { conn_id: src.into(), table: "items".into() }, &Side { conn_id: dst.into(), table: "items".into() })
        .await
        .unwrap();
    assert_eq!(ins.primary_key, vec!["id"]);
    assert_eq!(ins.only_target, vec!["extra"]);
    assert!(ins.only_source.is_empty());

    let mut s = spec(kind, src, dst, "items");
    s.chunk = 2;
    let r = compare(st, &s).await;
    assert_eq!((r.identical, r.different, r.only_source, r.only_target), (2, 3, 1, 1), "{:#?}", r.rows);
    assert_eq!((r.source_rows, r.target_rows), (6, 6));
    assert!(!r.capped && !r.truncated && !r.cancelled);
    assert_eq!(r.only_target_columns, vec!["extra"]);
    let changed = |id: &str| {
        let row = r.rows.iter().find(|x| x.kind == DiffKind::Different && x.key[0].as_deref() == Some(id)).unwrap();
        row.changed.iter().map(|&i| r.columns[i].as_str()).collect::<Vec<_>>()
    };
    assert_eq!(changed("2"), vec!["name"], "尾随空格算差异");
    assert_eq!(changed("3"), vec!["note"], "NULL 与空串不同");
    assert_eq!(changed("4"), vec!["data"], "二进制按字节比较");
    assert!(r.rows.iter().any(|x| x.kind == DiffKind::OnlySource && x.key[0].as_deref() == Some("6")));
    assert!(r.rows.iter().any(|x| x.kind == DiffKind::OnlyTarget && x.key[0].as_deref() == Some("7")));

    // 筛选条件两边都加
    let mut f = spec(kind, src, dst, "items");
    f.filter = Some("id <= 3".into());
    let rf = compare(st, &f).await;
    assert_eq!((rf.identical, rf.different, rf.only_source, rf.only_target), (1, 2, 0, 0));

    // 默认脚本:删除被注释
    let script = data_diff::script(&r, false);
    eprintln!("{kind:?} script:\n{script}");
    assert!(script.contains("-- DELETE FROM"), "{script}");
    assert!(script.contains("UPDATE "), "{script}");
    assert!(script.contains("INSERT INTO "), "{script}");
    apply(st, kind, dst, &script).await;
    let again = compare(st, &s).await;
    assert_eq!((again.identical, again.different, again.only_source, again.only_target), (6, 0, 0, 1));

    // 包含删除:执行后完全一致
    let full = data_diff::script(&again, true);
    assert!(full.lines().any(|l| l.starts_with("DELETE FROM")), "{full}");
    apply(st, kind, dst, &full).await;
    let last = compare(st, &s).await;
    assert_eq!((last.identical, last.total_diffs()), (6, 0));
    assert!(data_diff::script(&last, true).lines().all(|l| l.starts_with("--") || l.is_empty()));
}

/// 没有主键:拒绝并给出唯一键;用复合唯一键(含文本列,大小写混排)对比。
async fn composite_scenario(st: &AppState, kind: DbKind, src: &str, dst: &str) {
    for c in [src, dst] {
        exec(st, kind, c, "CREATE TABLE pairs (a INT NOT NULL, b VARCHAR(10) NOT NULL, v INT, UNIQUE (a, b))").await;
    }
    exec(st, kind, src, "INSERT INTO pairs VALUES (1, 'a', 1), (1, 'B', 2), (2, 'x', 3), (2, 'Z', NULL)").await;
    exec(st, kind, dst, "INSERT INTO pairs VALUES (1, 'B', 2), (1, 'a', 9), (3, 'y', 0), (2, 'Z', 5)").await;
    let mut s = spec(kind, src, dst, "pairs");
    let ins = data_diff::inspect(st, &kind, &s.source, &s.target).await.unwrap();
    if kind == DbKind::Mysql {
        // InnoDB 把第一个全 NOT NULL 的唯一索引当隐式主键(information_schema 里标为 PRI),直接可用
        assert_eq!(ins.primary_key, vec!["a", "b"]);
    } else {
        let err = data_diff::run(st, &s, &Job::default()).await.unwrap_err();
        assert!(err.display().contains("primary key"), "{}", err.display());
        assert!(ins.primary_key.is_empty());
        assert_eq!(ins.unique_keys.iter().map(|u| u.columns.clone()).collect::<Vec<_>>(), vec![vec!["a".to_string(), "b".to_string()]]);
    }
    s.key = vec!["a".into(), "b".into()];
    s.chunk = 1;
    let r = compare(st, &s).await;
    assert_eq!((r.identical, r.different, r.only_source, r.only_target), (1, 2, 1, 1), "{:#?}", r.rows);
    apply(st, kind, dst, &data_diff::script(&r, true)).await;
    let again = compare(st, &s).await;
    assert_eq!((again.identical, again.total_diffs()), (4, 0));
}

/// 行数上限、差异保留上限、取消、后台任务。
async fn limits_scenario(st: &Arc<AppState>, kind: DbKind, src: &str, dst: &str) {
    for c in [src, dst] {
        exec(st, kind, c, "CREATE TABLE big (id INT PRIMARY KEY, v INT)").await;
    }
    let vals: Vec<String> = (1..=60).map(|i| format!("({i}, {i})")).collect();
    exec(st, kind, src, &format!("INSERT INTO big VALUES {}", vals.join(", "))).await;
    // 目标表只有偶数行
    let even: Vec<String> = (1..=60).filter(|i| i % 2 == 0).map(|i| format!("({i}, {i})")).collect();
    exec(st, kind, dst, &format!("INSERT INTO big VALUES {}", even.join(", "))).await;

    let mut s = spec(kind, src, dst, "big");
    s.chunk = 7;
    s.max_rows = 20;
    let r = compare(st, &s).await;
    assert!(r.capped);
    assert_eq!(r.source_rows, 20);
    // 只对比到源表第 20 行(id 20):1..20 里偶数 10 行一致,奇数 10 行只在源表
    assert_eq!((r.identical, r.only_source, r.only_target), (10, 10, 0), "{r:#?}");
    assert!(data_diff::script(&r, false).contains("row limit"));

    let mut s = spec(kind, src, dst, "big");
    s.max_diffs = 5;
    let r = compare(st, &s).await;
    assert!(!r.capped);
    assert_eq!((r.identical, r.only_source), (30, 30));
    assert!(r.truncated);
    assert_eq!(r.rows.len(), 5);

    // 取消:在块之间生效
    let job = Job::default();
    job.cancel();
    let r = data_diff::run(st, &spec(kind, src, dst, "big"), &job).await.unwrap();
    assert!(r.cancelled);
    assert_eq!(r.identical + r.total_diffs(), 0);

    // 后台任务:轮询进度到完成,再取脚本
    let id = st.data_diffs.start(st.clone(), spec(kind, src, dst, "big"));
    let p = loop {
        let p = st.data_diffs.progress(&id).unwrap();
        if p.done {
            break p;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert!(p.error.is_none(), "{:?}", p.error);
    assert_eq!(p.source_rows, 60);
    assert_eq!(p.report.as_ref().unwrap().only_source, 30);
    assert!(st.data_diffs.script(&id, false).unwrap().contains("INSERT INTO"));
    st.data_diffs.close(&id);
    assert!(st.data_diffs.progress(&id).is_none());
    assert!(st.data_diffs.script(&id, false).is_err());
}

async fn all(st: &Arc<AppState>, kind: DbKind, src: &str, dst: &str) {
    main_scenario(st, kind, src, dst).await;
    composite_scenario(st, kind, src, dst).await;
    limits_scenario(st, kind, src, dst).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_data_diff() {
    let st = state();
    let a = st.config_dir.join("a.db");
    let b = st.config_dir.join("b.db");
    rusqlite::Connection::open(&a).unwrap();
    rusqlite::Connection::open(&b).unwrap();
    let src = connect(&st, DbKind::Sqlite, 0, "", "", Some(a.display().to_string())).await;
    let dst = connect(&st, DbKind::Sqlite, 0, "", "", Some(b.display().to_string())).await;
    all(&st, DbKind::Sqlite, &src, &dst).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_same_connection_two_tables() {
    // 同一个库里的两张表(如 orders 与 orders_backup)
    let st = state();
    let a = st.config_dir.join("one.db");
    rusqlite::Connection::open(&a).unwrap();
    let id = connect(&st, DbKind::Sqlite, 0, "", "", Some(a.display().to_string())).await;
    exec(&st, DbKind::Sqlite, &id, "CREATE TABLE t1 (id INTEGER PRIMARY KEY, v TEXT COLLATE NOCASE, b BLOB)").await;
    exec(&st, DbKind::Sqlite, &id, "CREATE TABLE t2 (id INTEGER PRIMARY KEY, v TEXT, b BLOB)").await;
    // id 3:NULL 与空 BLOB 不同(SQLite 的 hex(NULL) 是空串,不能混为一谈)
    exec(&st, DbKind::Sqlite, &id, "INSERT INTO t1 VALUES (1, 'A', NULL), (2, 'b', NULL), (3, 'c', NULL)").await;
    exec(&st, DbKind::Sqlite, &id, "INSERT INTO t2 VALUES (1, 'a', NULL), (2, 'b', NULL), (3, 'c', X'')").await;
    let s = Spec::new(
        DbKind::Sqlite,
        Side { conn_id: id.clone(), table: "t1".into() },
        Side { conn_id: id.clone(), table: "t2".into() },
    );
    let r = compare(&st, &s).await;
    assert_eq!((r.identical, r.different), (1, 2));
    let script = data_diff::script(&r, false);
    assert!(script.contains("UPDATE \"t2\" SET \"v\" = 'A' WHERE \"id\" = 1;"), "{script}");
    assert!(script.contains("UPDATE \"t2\" SET \"b\" = NULL WHERE \"id\" = 3;"), "{script}");
    apply(&st, DbKind::Sqlite, &id, &script).await;
    assert_eq!(compare(&st, &s).await.total_diffs(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_data_diff() {
    let Some(port) = std::env::var("DATUM_TEST_PG").ok().and_then(|p| p.parse().ok()) else { return };
    let st = state();
    let boot = connect(&st, DbKind::Pg, port, "postgres", "postgres", None).await;
    for db in ["dd_src", "dd_dst"] {
        exec(&st, DbKind::Pg, &boot, &format!("DROP DATABASE IF EXISTS {db}")).await;
        exec(&st, DbKind::Pg, &boot, &format!("CREATE DATABASE {db}")).await;
    }
    let src = connect(&st, DbKind::Pg, port, "postgres", "dd_src", None).await;
    let dst = connect(&st, DbKind::Pg, port, "postgres", "dd_dst", None).await;
    all(&st, DbKind::Pg, &src, &dst).await;
    // jsonb 与 uuid / 文本键
    for c in [&src, &dst] {
        exec(&st, DbKind::Pg, c, "CREATE TABLE docs (k text PRIMARY KEY, j jsonb, u uuid)").await;
    }
    exec(&st, DbKind::Pg, &src, "INSERT INTO docs VALUES ('Ä', '{\"a\": 1.0}', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'), ('b', '[1]', NULL), ('B', '{}', NULL)").await;
    exec(&st, DbKind::Pg, &dst, "INSERT INTO docs VALUES ('Ä', '{\"a\": 1}', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'), ('b', '[2]', NULL)").await;
    let s = spec(DbKind::Pg, &src, &dst, "docs");
    let r = compare(&st, &s).await;
    assert_eq!((r.identical, r.different, r.only_source), (1, 1, 1), "{:#?}", r.rows);
    apply(&st, DbKind::Pg, &dst, &data_diff::script(&r, true)).await;
    assert_eq!(compare(&st, &s).await.total_diffs(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_data_diff() {
    let Some(port) = std::env::var("DATUM_TEST_MYSQL").ok().and_then(|p| p.parse().ok()) else { return };
    let st = state();
    let boot = connect(&st, DbKind::Mysql, port, "root", "", None).await;
    for db in ["dd_src", "dd_dst"] {
        exec(&st, DbKind::Mysql, &boot, &format!("DROP DATABASE IF EXISTS {db}")).await;
        exec(&st, DbKind::Mysql, &boot, &format!("CREATE DATABASE {db}")).await;
    }
    let src = connect(&st, DbKind::Mysql, port, "root", "dd_src", None).await;
    let dst = connect(&st, DbKind::Mysql, port, "root", "dd_dst", None).await;
    all(&st, DbKind::Mysql, &src, &dst).await;
    // 大小写不敏感排序规则的文本主键 + BIT 列:键按字节序对齐,BIT 写成数字
    for c in [&src, &dst] {
        exec(&st, DbKind::Mysql, c, "CREATE TABLE tags (name VARCHAR(20) PRIMARY KEY, flag BIT(1), note VARCHAR(10)) COLLATE utf8mb4_0900_ai_ci").await;
    }
    exec(&st, DbKind::Mysql, &src, "INSERT INTO tags VALUES ('apple', b'1', 'x'), ('Banana', b'0', 'y'), ('cherry', b'1', 'z')").await;
    exec(&st, DbKind::Mysql, &dst, "INSERT INTO tags VALUES ('apple', b'0', 'x'), ('Banana', b'0', 'y')").await;
    let s = spec(DbKind::Mysql, &src, &dst, "tags");
    let r = compare(&st, &s).await;
    assert_eq!((r.identical, r.different, r.only_source), (1, 1, 1), "{:#?}", r.rows);
    apply(&st, DbKind::Mysql, &dst, &data_diff::script(&r, true)).await;
    assert_eq!(compare(&st, &s).await.total_diffs(), 0);
}
