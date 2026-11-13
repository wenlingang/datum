//! SQLite 改表结构:各种变更组合生成的脚本,在内存库上真实执行验证。

use datum_core::sqlite::alter::{self, AlterMode, AlterPlan, AlterScript, ColumnSpec, FkSpec, TableSnapshot};
use rusqlite::Connection;

fn db(sql: &str) -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(sql).unwrap();
    c
}

fn snap(c: &Connection, t: &str) -> TableSnapshot {
    alter::snapshot(c, t).unwrap()
}

/// 以现状为起点的目标结构(之后按需修改)。
fn plan_of(s: &TableSnapshot) -> AlterPlan {
    AlterPlan { table: s.table.clone(), new_name: s.table.clone(), columns: s.columns.clone(), fks: s.fks.clone() }
}

fn col<'a>(p: &'a mut AlterPlan, name: &str) -> &'a mut ColumnSpec {
    p.columns.iter_mut().find(|c| c.name == name).unwrap()
}

fn new_col(name: &str, ty: &str, not_null: bool, default: Option<&str>) -> ColumnSpec {
    ColumnSpec {
        orig: None,
        name: name.into(),
        data_type: ty.into(),
        not_null,
        default: default.map(String::from),
        pk: false,
        generated: false,
    }
}

fn rows(c: &Connection, sql: &str) -> Vec<Vec<String>> {
    let mut st = c.prepare(sql).unwrap();
    let n = st.column_count();
    st.query_map([], |r| {
        Ok((0..n)
            .map(|i| match r.get_ref(i).unwrap() {
                rusqlite::types::ValueRef::Null => "NULL".to_string(),
                rusqlite::types::ValueRef::Integer(v) => v.to_string(),
                rusqlite::types::ValueRef::Real(v) => v.to_string(),
                rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                rusqlite::types::ValueRef::Blob(_) => "<blob>".into(),
            })
            .collect())
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

fn master(c: &Connection, name: &str) -> Option<String> {
    c.query_row("SELECT sql FROM sqlite_master WHERE name = ?1", [name], |r| r.get(0)).ok()
}

fn pragma(c: &Connection, name: &str) -> i64 {
    c.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0)).unwrap()
}

fn gen_apply(c: &Connection, p: &AlterPlan) -> AlterScript {
    let s = alter::generate(&snap(c, &p.table), p).unwrap();
    let out = alter::apply(c, &s).unwrap_or_else(|e| panic!("{}\n{}", e.display(), s.sql));
    assert!(out.applied, "{:?}\n{}", out.violations, s.sql);
    s
}

const SHOP: &str = "
    CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, age INT, note TEXT);
    CREATE INDEX users_name ON users (name);
    CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INT REFERENCES users (id) ON DELETE CASCADE, total REAL);
    CREATE TABLE audit (msg TEXT);
    CREATE TRIGGER users_ins AFTER INSERT ON users BEGIN INSERT INTO audit VALUES ('new ' || NEW.name); END;
    CREATE VIEW adults AS SELECT id, name FROM users WHERE age >= 18;
    INSERT INTO users (id, name, age, note) VALUES (1, 'ann', 30, 'a'), (2, 'bob', 12, NULL);
    INSERT INTO orders VALUES (10, 1, 9.5), (11, 2, 3);
";

#[test]
fn noop_and_simple_alters() {
    let c = db(SHOP);
    let s = snap(&c, "users");
    let r = alter::generate(&s, &plan_of(&s)).unwrap();
    assert_eq!(r.mode, AlterMode::Noop);
    assert!(r.statements.is_empty());

    // 改表名 + 改列名 + 删一列 + 末尾加列:都能用 ALTER TABLE 完成
    let mut p = plan_of(&s);
    p.new_name = "people".into();
    col(&mut p, "name").name = "full_name".into();
    p.columns.retain(|c| c.name != "note");
    p.columns.push(new_col("city", "TEXT", true, Some("'?'")));
    let r = gen_apply(&c, &p);
    assert_eq!(r.mode, AlterMode::Simple, "{}", r.sql);
    assert!(r.sql.contains("ALTER TABLE \"users\" RENAME COLUMN \"name\" TO \"full_name\";"), "{}", r.sql);
    assert!(r.sql.contains("ALTER TABLE \"users\" DROP COLUMN \"note\";"));
    assert!(r.sql.contains("ALTER TABLE \"users\" ADD COLUMN \"city\" TEXT NOT NULL DEFAULT '?';"));
    assert!(r.sql.contains("ALTER TABLE \"users\" RENAME TO \"people\";"));
    assert!(!r.sql.contains("PRAGMA foreign_keys"));
    assert_eq!(rows(&c, "SELECT id, full_name, age, city FROM people ORDER BY id"), vec![
        vec!["1", "ann", "30", "?"],
        vec!["2", "bob", "12", "?"],
    ]);
    // SQLite 自己把索引、视图、触发器、外键里的引用都改了
    assert!(master(&c, "users_name").unwrap().contains("full_name"));
    assert!(master(&c, "adults").unwrap().contains("people"));
    assert!(master(&c, "orders").unwrap().contains("people"));
    assert_eq!(rows(&c, "SELECT full_name FROM adults"), vec![vec!["ann"]]);
}

#[test]
fn rebuild_keeps_data_objects_and_children() {
    let c = db(SHOP);
    c.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    let s = snap(&c, "users");
    assert!(s.foreign_keys_on);
    let mut p = plan_of(&s);
    // 改类型 + 加非空 + 改默认值 + 调整顺序 + 在中间加列
    col(&mut p, "age").data_type = "INTEGER".into();
    col(&mut p, "age").not_null = true;
    col(&mut p, "age").default = Some("0".into());
    p.columns.swap(1, 2);
    p.columns.insert(1, new_col("email", "TEXT", false, Some("(lower('X@Y'))")));
    let r = gen_apply(&c, &p);
    assert_eq!(r.mode, AlterMode::Rebuild);
    let sql = &r.sql;
    // 12 步的顺序:关外键在 BEGIN 前,外键检查在 COMMIT 前,恢复外键在 COMMIT 后
    let pos = |needle: &str| sql.find(needle).unwrap_or_else(|| panic!("缺 {needle}:\n{sql}"));
    assert!(pos("PRAGMA foreign_keys = OFF;") < pos("BEGIN;"));
    assert!(pos("BEGIN;") < pos("CREATE TABLE \"_datum_new_users\""));
    assert!(pos("INSERT INTO \"_datum_new_users\" (\"id\", \"age\", \"name\", \"note\") SELECT \"id\", \"age\", \"name\", \"note\" FROM \"users\";") < pos("DROP TABLE \"users\";"));
    assert!(pos("DROP TABLE \"users\";") < pos("ALTER TABLE \"_datum_new_users\" RENAME TO \"users\";"));
    assert!(pos("RENAME TO \"users\";") < pos("CREATE INDEX users_name"));
    assert!(pos("PRAGMA foreign_key_check(\"users\");") < pos("COMMIT;"));
    assert!(pos("PRAGMA foreign_key_check(\"orders\");") < pos("COMMIT;"));
    assert!(pos("COMMIT;") < pos("PRAGMA foreign_keys = ON;"));
    // AUTOINCREMENT 原样保留
    assert!(sql.contains("\"id\" INTEGER PRIMARY KEY AUTOINCREMENT"), "{sql}");

    assert_eq!(rows(&c, "SELECT * FROM users ORDER BY id"), vec![
        vec!["1", "x@y", "30", "ann", "a"],
        vec!["2", "x@y", "12", "bob", "NULL"],
    ]);
    // 外键关着重建:ON DELETE CASCADE 没把子表删空;索引、触发器、视图都在
    assert_eq!(rows(&c, "SELECT count(*) FROM orders"), vec![vec!["2"]]);
    assert!(master(&c, "users_name").is_some());
    assert!(master(&c, "users_ins").is_some());
    assert_eq!(rows(&c, "SELECT name FROM adults"), vec![vec!["ann"]]);
    c.execute_batch("INSERT INTO users (name) VALUES ('cy')").unwrap();
    assert_eq!(rows(&c, "SELECT msg FROM audit ORDER BY rowid DESC LIMIT 1"), vec![vec!["new cy"]]);
    assert_eq!(rows(&c, "SELECT age FROM users WHERE name = 'cy'"), vec![vec!["0"]]);
    // 连接设置恢复原样
    assert_eq!(pragma(&c, "foreign_keys"), 1);
    assert_eq!(pragma(&c, "legacy_alter_table"), 0);
    assert!(rows(&c, "PRAGMA foreign_key_check").is_empty());
    assert!(!rows(&c, "SELECT name FROM sqlite_master").iter().any(|r| r[0].starts_with("_datum_new")));
}

#[test]
fn rebuild_follows_column_rename_in_objects() {
    let c = db(SHOP);
    let s = snap(&c, "users");
    let mut p = plan_of(&s);
    // 改名 + 改类型:必须重建,索引 / 触发器 / 视图里的列名跟着换
    col(&mut p, "name").data_type = "VARCHAR(40)".into();
    col(&mut p, "name").name = "nick".into();
    let r = gen_apply(&c, &p);
    assert_eq!(r.mode, AlterMode::Rebuild);
    assert!(master(&c, "users_name").unwrap().contains("\"nick\""), "{}", r.sql);
    assert!(master(&c, "users_ins").unwrap().contains("NEW.\"nick\""));
    // 字符串里的 'new ' 不受影响
    assert!(master(&c, "users_ins").unwrap().contains("'new '"));
    assert_eq!(rows(&c, "SELECT * FROM adults"), vec![vec!["1", "ann"]]);
    // 触发器 / 视图按名替换,提示人工检查
    assert!(r.warnings.iter().any(|w| w.contains("users_ins")), "{:?}", r.warnings);
    assert!(r.warnings.iter().any(|w| w.contains("adults")), "{:?}", r.warnings);
}

#[test]
fn drop_indexed_column_rebuilds_and_comments_index() {
    let c = db(SHOP);
    let s = snap(&c, "users");
    let mut p = plan_of(&s);
    p.columns.retain(|c| c.name != "name");
    // 视图与触发器也用到了 name:先改掉它们,只剩索引
    c.execute_batch("DROP VIEW adults; DROP TRIGGER users_ins;").unwrap();
    let s = snap(&c, "users");
    let r = alter::generate(&s, &p).unwrap();
    assert_eq!(r.mode, AlterMode::Rebuild, "被索引的列不能 DROP COLUMN");
    assert!(r.sql.contains("-- CREATE INDEX users_name"), "{}", r.sql);
    assert!(r.warnings.iter().any(|w| w.contains("users_name")));
    assert!(alter::apply(&c, &r).unwrap().applied);
    assert!(master(&c, "users_name").is_none());
    assert_eq!(rows(&c, "SELECT id, age FROM users ORDER BY id"), vec![vec!["1", "30"], vec!["2", "12"]]);
}

#[test]
fn rename_table_during_rebuild_rewrites_references() {
    let c = db(SHOP);
    let s = snap(&c, "users");
    let mut p = plan_of(&s);
    p.new_name = "members".into();
    col(&mut p, "age").not_null = true;
    col(&mut p, "age").default = Some("0".into());
    gen_apply(&c, &p);
    assert!(master(&c, "users").is_none());
    assert!(master(&c, "adults").unwrap().contains("members"));
    assert!(master(&c, "orders").unwrap().contains("members"));
    assert!(master(&c, "users_ins").unwrap().contains("members"));
    assert_eq!(rows(&c, "SELECT count(*) FROM members"), vec![vec!["2"]]);
}

#[test]
fn foreign_keys_add_drop_and_violations() {
    let c = db(SHOP);
    // 加外键:orders.total 没什么可引用的,换一张表演示
    c.execute_batch(
        "CREATE TABLE tags (id INTEGER PRIMARY KEY, user_id INT); INSERT INTO tags VALUES (1, 1), (2, 99);",
    )
    .unwrap();
    let s = snap(&c, "tags");
    let mut p = plan_of(&s);
    p.fks.push(FkSpec {
        columns: vec!["user_id".into()],
        ref_table: "users".into(),
        ref_columns: vec!["id".into()],
        on_delete: "CASCADE".into(),
        on_update: "".into(),
    });
    let r = alter::generate(&s, &p).unwrap();
    assert_eq!(r.mode, AlterMode::Rebuild);
    assert!(r.sql.contains("FOREIGN KEY (\"user_id\") REFERENCES \"users\" (\"id\") ON DELETE CASCADE"), "{}", r.sql);
    // user_id = 99 不存在:外键检查不过,整体回滚并列出违规行
    let out = alter::apply(&c, &r).unwrap();
    assert!(!out.applied);
    assert_eq!(out.violations.len(), 1);
    assert_eq!(out.violations[0].table, "tags");
    assert_eq!(out.violations[0].rowid, Some(2));
    assert_eq!(out.violations[0].parent, "users");
    assert!(snap(&c, "tags").fks.is_empty(), "已回滚");
    assert_eq!(pragma(&c, "legacy_alter_table"), 0);

    c.execute_batch("DELETE FROM tags WHERE id = 2").unwrap();
    let r = gen_apply(&c, &p);
    assert_eq!(snap(&c, "tags").fks.len(), 1, "{}", r.sql);

    // 删外键(orders 的列级 REFERENCES)
    let s = snap(&c, "orders");
    let mut p = plan_of(&s);
    p.fks.clear();
    gen_apply(&c, &p);
    assert!(snap(&c, "orders").fks.is_empty());
    assert_eq!(rows(&c, "SELECT count(*) FROM orders"), vec![vec!["2"]]);
}

#[test]
fn failure_rolls_back_and_restores_pragmas() {
    let c = db(SHOP);
    c.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    let s = snap(&c, "users");
    let mut p = plan_of(&s);
    // 新加 NOT NULL 无默认值的列:有数据时复制失败
    p.columns.insert(1, new_col("must", "TEXT", true, None));
    let r = alter::generate(&s, &p).unwrap();
    assert_eq!(r.mode, AlterMode::Rebuild);
    assert!(r.warnings.iter().any(|w| w.contains("must")), "{:?}", r.warnings);
    let e = alter::apply(&c, &r).unwrap_err();
    assert!(!e.display().is_empty());
    assert_eq!(snap(&c, "users").columns.len(), 4, "已回滚");
    assert_eq!(rows(&c, "SELECT count(*) FROM orders"), vec![vec!["2"]]);
    assert_eq!(pragma(&c, "foreign_keys"), 1, "外键设置恢复");
    assert_eq!(pragma(&c, "legacy_alter_table"), 0);
    assert!(c.is_autocommit(), "没有残留的事务");
}

#[test]
fn composite_primary_key_and_generated_column() {
    let c = db("CREATE TABLE t (a INT, b INT, c INT GENERATED ALWAYS AS (a + b) VIRTUAL, CHECK (a >= 0));
                INSERT INTO t (a, b) VALUES (1, 2), (3, 4);");
    let s = snap(&c, "t");
    assert!(s.columns[2].generated);
    let mut p = plan_of(&s);
    col(&mut p, "a").pk = true;
    col(&mut p, "b").pk = true;
    let r = gen_apply(&c, &p);
    assert!(r.sql.contains("PRIMARY KEY (\"a\", \"b\")"), "{}", r.sql);
    assert!(r.sql.contains("CHECK (a >= 0)"));
    assert!(r.sql.contains("SELECT \"a\", \"b\" FROM"), "生成列不复制:{}", r.sql);
    assert_eq!(rows(&c, "SELECT a, b, c FROM t ORDER BY a"), vec![vec!["1", "2", "3"], vec!["3", "4", "7"]]);
    let s = snap(&c, "t");
    assert!(s.columns[0].pk && s.columns[1].pk);
    assert!(c.execute_batch("INSERT INTO t (a, b) VALUES (1, 2)").is_err(), "主键生效");
    assert!(c.execute_batch("INSERT INTO t (a, b) VALUES (-1, 9)").is_err(), "CHECK 保留");
}

#[test]
fn hostile_input_is_rejected_or_quoted() {
    let c = db(SHOP);
    let s = snap(&c, "users");
    for bad in ["1); DROP TABLE orders; --", "0 -- x", "'a'\nDROP TABLE orders", "(1"] {
        let mut p = plan_of(&s);
        col(&mut p, "age").default = Some(bad.into());
        assert!(alter::generate(&s, &p).is_err(), "{bad}");
    }
    let mut p = plan_of(&s);
    col(&mut p, "age").data_type = "INT); DROP TABLE orders; --".into();
    assert!(alter::generate(&s, &p).is_err());
    let mut p = plan_of(&s);
    p.fks.push(FkSpec {
        columns: vec!["age".into()],
        ref_table: "users".into(),
        ref_columns: vec![],
        on_delete: "CASCADE; DROP TABLE orders".into(),
        on_update: String::new(),
    });
    assert!(alter::generate(&s, &p).is_err());

    // 标识符里的引号、分号、换行都被双引号转义,照样能执行
    let mut p = plan_of(&s);
    col(&mut p, "note").name = "n\"o;te\n--x".into();
    col(&mut p, "age").data_type = "TEXT".into();
    p.new_name = "we\"ird; DROP TABLE orders".into();
    gen_apply(&c, &p);
    assert!(master(&c, "orders").is_some());
    assert_eq!(rows(&c, "SELECT \"n\"\"o;te\n--x\" FROM \"we\"\"ird; DROP TABLE orders\" WHERE id = 1"), vec![vec!["a"]]);
}

#[test]
fn invalid_plans() {
    let c = db(SHOP);
    let s = snap(&c, "users");
    let mut p = plan_of(&s);
    p.new_name = "orders".into();
    assert!(alter::generate(&s, &p).is_err(), "与已有表重名");
    let mut p = plan_of(&s);
    p.columns[1].name = "ID".into();
    assert!(alter::generate(&s, &p).is_err(), "列名重复(不分大小写)");
    let mut p = plan_of(&s);
    p.columns.clear();
    assert!(alter::generate(&s, &p).is_err());
    let mut p = plan_of(&s);
    p.fks.push(FkSpec { columns: vec!["nope".into()], ref_table: "x".into(), ref_columns: vec![], on_delete: String::new(), on_update: String::new() });
    assert!(alter::generate(&s, &p).is_err());
}

#[test]
fn preview_must_match_before_apply() {
    let c = db(SHOP);
    let s = snap(&c, "users");
    let mut p = plan_of(&s);
    col(&mut p, "age").data_type = "TEXT".into();
    let r = alter::generate(&s, &p).unwrap();
    // 预览之后表被别处改了:拒绝执行
    c.execute_batch("CREATE INDEX users_age ON users (age)").unwrap();
    assert!(alter::preview_and_apply(&c, &p, &r.sql).is_err());
    let r = alter::generate(&snap(&c, "users"), &p).unwrap();
    assert!(alter::preview_and_apply(&c, &p, &r.sql).unwrap().applied);
    assert!(master(&c, "users_age").is_some());
}

#[test]
fn executor_runs_one_statement_at_a_time() {
    let c = db(SHOP);
    let script = AlterScript {
        mode: AlterMode::Simple,
        sql: String::new(),
        statements: vec!["ALTER TABLE users ADD COLUMN x INT; DROP TABLE orders".into()],
        fk_check: vec![],
        foreign_keys_off: false,
        warnings: vec![],
    };
    // 一条里夹带第二条语句:拒绝执行,整体回滚
    assert!(alter::apply(&c, &script).is_err());
    assert!(master(&c, "orders").is_some());
    assert_eq!(snap(&c, "users").columns.len(), 4);
}

#[test]
fn self_referencing_fk_follows_rename() {
    let c = db("CREATE TABLE node (id INTEGER PRIMARY KEY, parent INT REFERENCES node (id), label TEXT);
                INSERT INTO node VALUES (1, NULL, 'root'), (2, 1, 'kid');");
    let s = snap(&c, "node");
    let mut p = plan_of(&s);
    col(&mut p, "id").name = "node_id".into();
    col(&mut p, "label").not_null = true;
    p.fks[0].ref_columns = vec!["node_id".into()];
    let r = gen_apply(&c, &p);
    assert_eq!(r.mode, AlterMode::Rebuild);
    let s = snap(&c, "node");
    assert_eq!(s.fks[0].ref_columns, vec!["node_id"], "{}", r.sql);
    assert_eq!(rows(&c, "SELECT node_id, parent FROM node ORDER BY 1"), vec![vec!["1", "NULL"], vec!["2", "1"]]);
}

#[test]
fn view_on_dropped_column_without_legacy_mode_would_fail() {
    // 说明为什么重建时要开 legacy_alter_table:视图引用了删掉的列,
    // 非 legacy 模式下 RENAME 会去解析整个 schema 里的视图而报错
    let c = db("CREATE TABLE t (a INT, b INT); CREATE VIEW v AS SELECT a, b FROM t; INSERT INTO t VALUES (1, 2);");
    c.execute_batch(
        "BEGIN; CREATE TABLE n (a INT); INSERT INTO n SELECT a FROM t; DROP TABLE t;",
    )
    .unwrap();
    assert!(c.execute_batch("ALTER TABLE n RENAME TO t").is_err());
    c.execute_batch("ROLLBACK").unwrap();
    c.execute_batch(
        "BEGIN; PRAGMA legacy_alter_table = ON; CREATE TABLE n (a INT); INSERT INTO n SELECT a FROM t; DROP TABLE t; \
         ALTER TABLE n RENAME TO t; PRAGMA legacy_alter_table = OFF; ROLLBACK;",
    )
    .unwrap();
}
