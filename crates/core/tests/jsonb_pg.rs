//! 真实 PG 上的 JSONB:按 JSON 路径生成的筛选 WHERE 能执行、结果符合预期(含键名转义、类型混杂的行),
//! 等于走包含查询能用上 GIN 索引;补全 JSON 键名的取样推断。需要本机 PG(用户 postgres、空密码),默认忽略:
//!   DATUM_COMPL_PG=端口 cargo test -p datum-core --test jsonb_pg -- --include-ignored
//! 会建库 datum_jsonb_test,结束时删除。

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::grid::{Conj, FilterOp, FilterRow};
use datum_core::json::{Kind, PathSeg};
use datum_core::service;
use datum_core::sql::{build, Dialect};
use std::collections::HashMap;

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-jsonb-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

async fn connect(st: &AppState, port: u16, db: &str) -> String {
    let id = format!("jsonb-{}", uuid::Uuid::new_v4());
    let c = Connection {
        id: id.clone(),
        name: "c".into(),
        kind: DbKind::Pg,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        user: "postgres".into(),
        database: db.into(),
        plaintext_password: None,
        file_path: None,
        ssl_mode: SslMode::Disable,
        uri: None,
        auth_source: None,
        ssh: None,
    };
    commands::save_connection(st, c, Some(String::new())).unwrap();
    service::connect(st, &DbKind::Pg, &id).await.unwrap();
    id
}

async fn exec(st: &AppState, id: &str, sql: &str) -> Vec<Vec<Option<String>>> {
    service::query(st, &DbKind::Pg, id, sql, None)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {}", e.display()))
        .rows
}

fn key(s: &str) -> PathSeg {
    PathSeg::Key(s.into())
}

fn cond(col: &str, path: Vec<PathSeg>, ty: Option<Kind>, op: FilterOp, value: &str) -> FilterRow {
    FilterRow { id: 1, col: col.into(), op, value: value.into(), path, json_type: ty }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn jsonb_filters_and_key_inference() {
    let Some(port) = std::env::var("DATUM_COMPL_PG").ok().and_then(|p| p.parse().ok()) else { return };
    let st = state();
    let boot = connect(&st, port, "postgres").await;
    exec(&st, &boot, "DROP DATABASE IF EXISTS datum_jsonb_test").await;
    exec(&st, &boot, "CREATE DATABASE datum_jsonb_test").await;
    let id = connect(&st, port, "datum_jsonb_test").await;

    exec(&st, &id, "CREATE TABLE docs (id int PRIMARY KEY, doc jsonb, raw json)").await;
    // 第 3 行的 n 是文本、第 4 行没有 a:数值比较不能因为它们报错
    exec(
        &st,
        &id,
        r#"INSERT INTO docs VALUES
        (1, '{"a": {"n": 3, "s": "Paid", "ok": true}, "items": [{"sku": 7}], "it''s \"k\"": "q"}', '{"s": "x", "z": null}'),
        (2, '{"a": {"n": 10, "s": "paid", "ok": false}, "items": [{"sku": 8}]}', '{"s": "y"}'),
        (3, '{"a": {"n": "3", "s": "Due"}}', '{"z": 1}'),
        (4, '{"b": 1}', NULL)"#,
    )
    .await;
    exec(&st, &id, "CREATE INDEX docs_doc_gin ON docs USING gin (doc jsonb_path_ops)").await;

    let types: HashMap<String, String> =
        [("id", "integer"), ("doc", "jsonb"), ("raw", "json")].into_iter().map(|(a, b)| (a.into(), b.into())).collect();
    let ids = |f: FilterRow| {
        let w = build::where_clause(&[f], Conj::And, Dialect::Pg, &types).unwrap();
        let sql = format!("SELECT id FROM docs WHERE {w} ORDER BY id");
        let st = &st;
        let id = &id;
        async move {
            exec(st, id, &sql).await.into_iter().map(|r| r[0].clone().unwrap()).collect::<Vec<_>>().join(",")
        }
    };
    let an = || vec![key("a"), key("n")];
    let as_ = || vec![key("a"), key("s")];
    use FilterOp::*;
    // 带类型的等于(包含查询):3 与 "3" 不相等,字符串区分大小写
    assert_eq!(ids(cond("doc", an(), Some(Kind::Number), Eq, "3")).await, "1");
    assert_eq!(ids(cond("doc", an(), Some(Kind::Number), Eq, "3.0")).await, "1");
    assert_eq!(ids(cond("doc", as_(), Some(Kind::String), Eq, "Paid")).await, "1");
    assert_eq!(ids(cond("doc", vec![key("a"), key("ok")], Some(Kind::Bool), Eq, "false")).await, "2");
    // 不等于:字段缺失的行也算
    assert_eq!(ids(cond("doc", as_(), Some(Kind::String), Ne, "Paid")).await, "2,3,4");
    // 数值大小比较:第 3 行是文本 "3"、第 4 行没有该字段,只是不命中
    assert_eq!(ids(cond("doc", an(), Some(Kind::Number), Gt, "2")).await, "1,2");
    assert_eq!(ids(cond("doc", an(), Some(Kind::Number), Lte, "3")).await, "1");
    // 文本操作比较 ->> 的文本
    assert_eq!(ids(cond("doc", as_(), Some(Kind::String), Contains, "AI")).await, "1,2");
    assert_eq!(ids(cond("doc", vec![key("a")], None, Empty, "")).await, "4");
    // 路径里有下标
    assert_eq!(ids(cond("doc", vec![key("items"), PathSeg::Index(0), key("sku")], Some(Kind::Number), Eq, "8")).await, "2");
    // 键名带引号:转义后照常命中,不会注入
    assert_eq!(ids(cond("doc", vec![key("it's \"k\"")], Some(Kind::String), Eq, "q")).await, "1");
    assert_eq!(ids(cond("doc", vec![key("x'); DROP TABLE docs; --")], None, NotEmpty, "")).await, "");
    // json 列:没有 @>,按类型函数比较
    assert_eq!(ids(cond("raw", vec![key("s")], Some(Kind::String), Eq, "y")).await, "2");
    assert_eq!(ids(cond("raw", vec![key("z")], Some(Kind::Null), Eq, "null")).await, "1");
    assert_eq!(ids(cond("raw", vec![key("z")], Some(Kind::Number), Gte, "1")).await, "3");
    assert_eq!(exec(&st, &id, "SELECT count(*) FROM docs").await[0][0].as_deref(), Some("4"));

    // 等于走包含查询:禁用顺序扫描后计划里用上 GIN 索引
    let w = build::where_clause(&[cond("doc", an(), Some(Kind::Number), Eq, "3")], Conj::And, Dialect::Pg, &types).unwrap();
    exec(&st, &id, "SET enable_seqscan = off").await;
    let plan = exec(&st, &id, &format!("EXPLAIN SELECT * FROM docs WHERE {w}")).await;
    let plan: Vec<String> = plan.into_iter().filter_map(|r| r[0].clone()).collect();
    assert!(plan.iter().any(|l| l.contains("docs_doc_gin")), "{plan:?}");
    exec(&st, &id, "RESET enable_seqscan").await;

    // 键名推断:按出现次数排序,逐级、数组元素合并
    let tree = service::json_key_tree(&st, &DbKind::Pg, &id, "docs", "doc").await.unwrap().unwrap();
    let names = |p: &[PathSeg]| tree.at(p).map(|t| t.keys.iter().map(|e| e.name.clone()).collect::<Vec<_>>()).unwrap_or_default();
    assert_eq!(names(&[])[..2], ["a".to_string(), "items".to_string()]);
    assert_eq!(names(&[key("a")]), vec!["n", "s", "ok"]);
    assert_eq!(names(&[key("items"), PathSeg::Index(0)]), vec!["sku"]);
    // json 类型的列也能推断;非 PG 不做
    assert_eq!(service::json_key_tree(&st, &DbKind::Pg, &id, "docs", "raw").await.unwrap().unwrap().keys.len(), 2);

    exec(&st, &boot, "DROP DATABASE IF EXISTS datum_jsonb_test WITH (FORCE)").await;
}
