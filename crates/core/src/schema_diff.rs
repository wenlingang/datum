//! 表结构对比:比较两个同类型的库(如 staging 与 prod),列出差异并生成迁移脚本
//! (让「目标库」与「源库」一致)。
//!
//! 比较:表的增删;列的增删、类型、非空、默认值;索引的增删(主键只提示,不自动改)。
//! 脚本里删除类语句(删表、删列、删索引)一律注释掉,确认后再取消注释;
//! SQLite 不支持修改列定义,写成注释说明需要重建表。

use crate::core::connection::DbKind;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::pg::browse::{ColumnInfo, IndexInfo, TableDetail};
use crate::service;
use crate::sql::build::{ident, literal};
use crate::sql::Dialect;
use crate::AppState;
use rust_i18n::t;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

/// 一张表的对比结果。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TableDiff {
    pub name: String,
    /// added:只在源库;removed:只在目标库;changed:两边都有但不同
    pub status: &'static str,
    /// 每条差异的说明(给人看)
    pub changes: Vec<String>,
}

/// 对比报告。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffReport {
    pub tables: Vec<TableDiff>,
    /// 两边一致的表数
    pub same: usize,
    /// 迁移脚本(在目标库上执行)
    pub sql: String,
}

fn norm(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

fn same_default(a: &Option<String>, b: &Option<String>) -> bool {
    a.as_deref().map(str::trim) == b.as_deref().map(str::trim)
}

/// 默认值写进 DDL 的样子。PG / SQLite 的默认值本身就是表达式;MySQL 返回的是原值,
/// 字符串要补引号,数字、NULL 与函数(CURRENT_TIMESTAMP 等)原样。
fn default_sql(v: &str, d: Dialect) -> String {
    if d != Dialect::Mysql {
        return v.to_string();
    }
    let t = v.trim();
    let upper = t.to_ascii_uppercase();
    let is_num = t.parse::<f64>().is_ok();
    // 只认几种确定的表达式:NULL、CURRENT_TIMESTAMP[(n)]、整体被括号包住的表达式;其余一律当字符串值
    let is_expr = upper == "NULL"
        || upper == "CURRENT_TIMESTAMP"
        || (upper.starts_with("CURRENT_TIMESTAMP(") && upper.ends_with(')') && upper[18..upper.len() - 1].chars().all(|c| c.is_ascii_digit()))
        || (t.starts_with('(') && t.ends_with(')'));
    if is_num || is_expr { t.to_string() } else { literal(t, d) }
}

/// 写进注释的文字:换行、回车等控制字符换成空格,防止对象名把注释截断、后半截变成可执行语句。
fn comment_text(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// 语句在引号外是否含分号或换行(来自数据库的类型、默认值里出现这些,说明定义异常)。
fn suspicious(stmt: &str) -> bool {
    unquoted_hit(stmt, &[';', '\n', '\r'])
}

/// 引号外是否出现给定字符;引号没闭合也算异常。
fn unquoted_hit(stmt: &str, bad: &[char]) -> bool {
    let body = stmt.trim_end().trim_end_matches(';');
    let mut quote: Option<char> = None;
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => {
                if chars.peek() == Some(&q) {
                    chars.next();
                } else {
                    quote = None;
                }
            }
            Some(_) => {}
            None => match c {
                '\'' | '"' | '`' => quote = Some(c),
                c if bad.contains(&c) => return true,
                _ => {}
            },
        }
    }
    quote.is_some()
}

/// 可执行语句的出口:定义里有异常字符时整条注释掉并提示人工检查。
fn checked(stmt: String) -> Vec<String> {
    if suspicious(&stmt) {
        vec![format!("-- {}", t!("core.diff.suspicious")), commented(&stmt)]
    } else {
        vec![stmt]
    }
}

/// 列定义:类型 [NOT NULL] [DEFAULT …]。
fn column_def(c: &ColumnInfo, d: Dialect) -> String {
    let mut s = c.data_type.clone();
    if c.not_null && !c.is_pk {
        s.push_str(" NOT NULL");
    }
    if let Some(def) = &c.default {
        s.push_str(&format!(" DEFAULT {}", default_sql(def, d)));
    }
    s
}

fn index_cols(i: &IndexInfo, d: Dialect) -> String {
    i.columns.split(',').map(str::trim).filter(|c| !c.is_empty()).map(|c| ident(c, d)).collect::<Vec<_>>().join(", ")
}

fn same_index(a: &IndexInfo, b: &IndexInfo) -> bool {
    norm(&a.columns.replace(' ', "")) == norm(&b.columns.replace(' ', "")) && a.is_unique == b.is_unique
}

/// 被注释掉的语句(删除类):每一行都加 `-- `,控制字符换成空格(对象名里的换行截不断注释)。
fn commented(stmt: &str) -> String {
    stmt.split(['\n', '\r']).map(|l| format!("-- {}", comment_text(l))).collect::<Vec<_>>().join("\n")
}

/// 一张两边都有的表:列与索引的差异 → (说明, 语句)。
fn diff_table(name: &str, src: &TableDetail, dst: &TableDetail, d: Dialect) -> (Vec<String>, Vec<String>) {
    let tq = ident(name, d);
    let mut notes = vec![];
    let mut sql = vec![];
    let dst_cols: HashMap<&str, &ColumnInfo> = dst.columns.iter().map(|c| (c.name.as_str(), c)).collect();
    let src_cols: HashMap<&str, &ColumnInfo> = src.columns.iter().map(|c| (c.name.as_str(), c)).collect();

    for c in &src.columns {
        let cq = ident(&c.name, d);
        match dst_cols.get(c.name.as_str()) {
            None => {
                notes.push(t!("core.diff.col_added", col = c.name, def = column_def(c, d)).to_string());
                if d == Dialect::Sqlite && c.not_null && c.default.is_none() {
                    sql.push(format!("-- {}", comment_text(&t!("core.diff.sqlite_not_null", table = name, col = c.name))));
                    sql.push(commented(&format!("ALTER TABLE {tq} ADD COLUMN {cq} {};", column_def(c, d))));
                } else {
                    sql.extend(checked(format!("ALTER TABLE {tq} ADD COLUMN {cq} {};", column_def(c, d))));
                }
            }
            Some(o) => {
                let type_changed = norm(&o.data_type) != norm(&c.data_type);
                let null_changed = o.not_null != c.not_null && !c.is_pk;
                let def_changed = !same_default(&o.default, &c.default);
                if !(type_changed || null_changed || def_changed) {
                    continue;
                }
                if type_changed {
                    notes.push(t!("core.diff.col_type", col = c.name, from = o.data_type, to = c.data_type).to_string());
                }
                if null_changed {
                    notes.push(
                        t!("core.diff.col_null", col = c.name, to = if c.not_null { "NOT NULL" } else { "NULL" }).to_string(),
                    );
                }
                if def_changed {
                    notes.push(
                        t!(
                            "core.diff.col_default",
                            col = c.name,
                            from = o.default.clone().unwrap_or_else(|| "—".into()),
                            to = c.default.clone().unwrap_or_else(|| "—".into())
                        )
                        .to_string(),
                    );
                }
                match d {
                    Dialect::Pg => {
                        if type_changed {
                            sql.extend(checked(format!("ALTER TABLE {tq} ALTER COLUMN {cq} TYPE {};", c.data_type)));
                        }
                        if null_changed {
                            sql.push(format!(
                                "ALTER TABLE {tq} ALTER COLUMN {cq} {} NOT NULL;",
                                if c.not_null { "SET" } else { "DROP" }
                            ));
                        }
                        if def_changed {
                            sql.extend(checked(match &c.default {
                                Some(v) => format!("ALTER TABLE {tq} ALTER COLUMN {cq} SET DEFAULT {v};"),
                                None => format!("ALTER TABLE {tq} ALTER COLUMN {cq} DROP DEFAULT;"),
                            }));
                        }
                    }
                    Dialect::Mysql => sql.extend(checked(format!("ALTER TABLE {tq} MODIFY COLUMN {cq} {};", column_def(c, d)))),
                    Dialect::Sqlite => {
                        sql.push(format!("-- {}", comment_text(&t!("core.diff.sqlite_rebuild", table = name, col = c.name))))
                    }
                }
            }
        }
    }
    for c in &dst.columns {
        if !src_cols.contains_key(c.name.as_str()) {
            notes.push(t!("core.diff.col_removed", col = c.name).to_string());
            sql.push(commented(&format!("ALTER TABLE {tq} DROP COLUMN {};", ident(&c.name, d))));
        }
    }

    // 主键:只提示
    let pk = |t: &TableDetail| {
        let mut v: Vec<String> = t.columns.iter().filter(|c| c.is_pk).map(|c| c.name.clone()).collect();
        v.sort();
        v
    };
    if pk(src) != pk(dst) {
        notes.push(t!("core.diff.pk_changed", from = pk(dst).join(", "), to = pk(src).join(", ")).to_string());
        sql.push(format!("-- {}", comment_text(&t!("core.diff.pk_manual", table = name))));
    }

    // 索引(不含主键):按名字对齐,列或唯一性不同视为先删后建
    let src_idx: Vec<&IndexInfo> = src.indexes.iter().filter(|i| !i.is_primary).collect();
    let dst_idx: Vec<&IndexInfo> = dst.indexes.iter().filter(|i| !i.is_primary).collect();
    for i in &src_idx {
        match dst_idx.iter().find(|o| o.name == i.name) {
            Some(o) if same_index(i, o) => {}
            found => {
                if let Some(o) = found {
                    notes.push(t!("core.diff.idx_changed", name = i.name).to_string());
                    sql.push(commented(&drop_index(o, name, d)));
                } else {
                    notes.push(t!("core.diff.idx_added", name = i.name, cols = i.columns).to_string());
                }
                sql.extend(checked(format!(
                    "CREATE {}INDEX {} ON {tq} ({});",
                    if i.is_unique { "UNIQUE " } else { "" },
                    ident(&i.name, d),
                    index_cols(i, d)
                )));
            }
        }
    }
    for o in &dst_idx {
        if !src_idx.iter().any(|i| i.name == o.name) {
            notes.push(t!("core.diff.idx_removed", name = o.name).to_string());
            sql.push(commented(&drop_index(o, name, d)));
        }
    }
    (notes, sql)
}

fn drop_index(i: &IndexInfo, table: &str, d: Dialect) -> String {
    match d {
        Dialect::Mysql => format!("DROP INDEX {} ON {};", ident(&i.name, d), ident(table, d)),
        _ => format!("DROP INDEX {};", ident(&i.name, d)),
    }
}

/// 纯函数:比较两份结构,生成报告与脚本。`create_sql` 给出源库里新表的建表语句。
pub fn diff(
    source: &BTreeMap<String, TableDetail>,
    target: &BTreeMap<String, TableDetail>,
    create_sql: &HashMap<String, String>,
    d: Dialect,
) -> DiffReport {
    let mut tables = vec![];
    let mut blocks: Vec<String> = vec![];
    let mut same = 0;
    for (name, src) in source {
        match target.get(name) {
            None => {
                tables.push(TableDiff { name: name.clone(), status: "added", changes: vec![t!("core.diff.table_added").to_string()] });
                let header = format!("-- {}", comment_text(name));
                match create_sql.get(name) {
                    // 建表语句本来就跨多行;只查引号外的分号(会拆出第二条语句),有就整段注释掉
                    Some(ddl) if unquoted_hit(ddl.trim().trim_end_matches(';'), &[';']) => blocks.push(format!(
                        "{header}\n-- {}\n{}",
                        t!("core.diff.suspicious"),
                        commented(ddl.trim())
                    )),
                    Some(ddl) => blocks.push(format!("{header}\n{};", ddl.trim().trim_end_matches(';'))),
                    None => blocks.push(format!("{header}\n-- {}", comment_text(&t!("core.diff.no_ddl", table = name)))),
                }
            }
            Some(dst) => {
                let (notes, sql) = diff_table(name, src, dst, d);
                if notes.is_empty() {
                    same += 1;
                } else {
                    tables.push(TableDiff { name: name.clone(), status: "changed", changes: notes });
                    blocks.push(format!("-- {}\n{}", comment_text(name), sql.join("\n")));
                }
            }
        }
    }
    for name in target.keys() {
        if !source.contains_key(name) {
            tables.push(TableDiff { name: name.clone(), status: "removed", changes: vec![t!("core.diff.table_removed").to_string()] });
            blocks.push(format!("-- {}\n{}", comment_text(name), commented(&format!("DROP TABLE {};", ident(name, d)))));
        }
    }
    let sql = if blocks.is_empty() {
        format!("-- {}", t!("core.diff.identical"))
    } else {
        format!("-- {}\n\n{}\n", t!("core.diff.header"), blocks.join("\n\n"))
    };
    DiffReport { tables, same, sql }
}

/// 读出一个库的全部表结构。
async fn snapshot(state: &AppState, kind: &DbKind, id: &str) -> AppResult<BTreeMap<String, TableDetail>> {
    let mut out = BTreeMap::new();
    for table in service::list_objects(state, kind, id).await? {
        let detail = service::table_detail(state, kind, id, &table).await?;
        out.insert(table, detail);
    }
    Ok(out)
}

/// 比较两个连接(必须同一种数据库):生成让目标库与源库一致的脚本。
pub async fn compare(state: &AppState, kind: &DbKind, source_id: &str, target_id: &str) -> AppResult<DiffReport> {
    let d = Dialect::from_kind(kind).ok_or_else(|| AppError::new(ErrorKind::Query, t!("core.err.redis_unsupported")))?;
    let source = snapshot(state, kind, source_id).await?;
    let target = snapshot(state, kind, target_id).await?;
    let mut create_sql = HashMap::new();
    for name in source.keys().filter(|n| !target.contains_key(*n)) {
        if let Ok(ddl) = service::create_table_ddl(state, kind, source_id, name).await {
            create_sql.insert(name.clone(), ddl);
        }
    }
    Ok(diff(&source, &target, &create_sql, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str, not_null: bool, default: Option<&str>, pk: bool) -> ColumnInfo {
        ColumnInfo { name: name.into(), data_type: ty.into(), default: default.map(String::from), comment: None, not_null, is_pk: pk }
    }

    fn idx(name: &str, cols: &str, unique: bool) -> IndexInfo {
        IndexInfo { name: name.into(), columns: cols.into(), is_primary: false, is_unique: unique }
    }

    fn schemas() -> (BTreeMap<String, TableDetail>, BTreeMap<String, TableDetail>) {
        let mut src = BTreeMap::new();
        let mut dst = BTreeMap::new();
        src.insert(
            "orders".to_string(),
            TableDetail {
                columns: vec![
                    col("id", "integer", true, None, true),
                    col("status", "varchar(20)", true, Some("'new'"), false),
                    col("total", "numeric(10,2)", false, None, false),
                    col("note", "text", false, None, false),
                ],
                indexes: vec![idx("orders_status_idx", "status", false)],
            },
        );
        dst.insert(
            "orders".to_string(),
            TableDetail {
                columns: vec![
                    col("id", "integer", true, None, true),
                    col("status", "varchar(10)", false, None, false),
                    col("total", "numeric(10,2)", false, None, false),
                    col("legacy", "text", false, None, false),
                ],
                indexes: vec![idx("orders_legacy_idx", "legacy", false)],
            },
        );
        src.insert("new_table".into(), TableDetail { columns: vec![col("id", "integer", true, None, true)], indexes: vec![] });
        dst.insert("old_table".into(), TableDetail { columns: vec![col("id", "integer", true, None, true)], indexes: vec![] });
        src.insert("same".into(), TableDetail { columns: vec![col("id", "integer", true, None, true)], indexes: vec![] });
        dst.insert("same".into(), TableDetail { columns: vec![col("id", "INTEGER", true, None, true)], indexes: vec![] });
        (src, dst)
    }

    #[test]
    fn pg_script_and_report() {
        let (src, dst) = schemas();
        let ddl: HashMap<String, String> = [("new_table".to_string(), "CREATE TABLE \"new_table\" (\"id\" integer PRIMARY KEY);".to_string())].into();
        let r = diff(&src, &dst, &ddl, Dialect::Pg);
        assert_eq!(r.same, 1, "类型大小写不同不算差异");
        let status: Vec<(&str, &str)> = r.tables.iter().map(|t| (t.name.as_str(), t.status)).collect();
        assert_eq!(status, vec![("new_table", "added"), ("orders", "changed"), ("old_table", "removed")]);
        let s = &r.sql;
        assert!(s.contains("CREATE TABLE \"new_table\" (\"id\" integer PRIMARY KEY);"));
        assert!(s.contains("ALTER TABLE \"orders\" ALTER COLUMN \"status\" TYPE varchar(20);"));
        assert!(s.contains("ALTER TABLE \"orders\" ALTER COLUMN \"status\" SET NOT NULL;"));
        assert!(s.contains("ALTER TABLE \"orders\" ALTER COLUMN \"status\" SET DEFAULT 'new';"));
        assert!(s.contains("ALTER TABLE \"orders\" ADD COLUMN \"note\" text;"));
        assert!(s.contains("CREATE INDEX \"orders_status_idx\" ON \"orders\" (\"status\");"));
        // 删除类全部注释掉
        assert!(s.contains("-- ALTER TABLE \"orders\" DROP COLUMN \"legacy\";"));
        assert!(s.contains("-- DROP INDEX \"orders_legacy_idx\";"));
        assert!(s.contains("-- DROP TABLE \"old_table\";"));
        assert!(!s.lines().any(|l| l.starts_with("DROP") || l.contains(" DROP COLUMN") && !l.starts_with("--")));
    }

    #[test]
    fn mysql_modify_and_sqlite_limits() {
        let (src, dst) = schemas();
        let r = diff(&src, &dst, &HashMap::new(), Dialect::Mysql);
        assert!(r.sql.contains("ALTER TABLE `orders` MODIFY COLUMN `status` varchar(20) NOT NULL DEFAULT "));
        assert!(r.sql.contains("-- DROP INDEX `orders_legacy_idx` ON `orders`;"));
        // MySQL 的默认值原样是不带引号的值
        assert_eq!(default_sql("new", Dialect::Mysql), "'new'");
        assert_eq!(default_sql("0", Dialect::Mysql), "0");
        assert_eq!(default_sql("CURRENT_TIMESTAMP", Dialect::Mysql), "CURRENT_TIMESTAMP");

        let r = diff(&src, &dst, &HashMap::new(), Dialect::Sqlite);
        assert!(r.sql.contains("-- ") && !r.sql.contains("ALTER COLUMN"), "SQLite 不支持改列,只写注释");
        assert!(r.sql.contains("ALTER TABLE \"orders\" ADD COLUMN \"note\" text;"));
    }

    #[test]
    fn identical_schemas() {
        let (src, _) = schemas();
        let r = diff(&src, &src, &HashMap::new(), Dialect::Pg);
        assert!(r.tables.is_empty());
        assert_eq!(r.same, 3);
        assert!(r.sql.starts_with("-- "));
    }

    /// 恶意对象名 / 默认值:任何一行要么是注释,要么不含注入的语句。
    #[test]
    fn hostile_names_and_defaults_stay_inert() {
        let evil = "x\nDROP TABLE victims; --";
        let mut src = BTreeMap::new();
        let mut dst = BTreeMap::new();
        dst.insert(evil.to_string(), TableDetail { columns: vec![col("id", "integer", true, None, true)], indexes: vec![] });
        src.insert(
            "t".to_string(),
            TableDetail {
                columns: vec![
                    col("id", "integer", true, None, true),
                    col("a", "text", false, Some("x'); DROP TABLE victims; (1"), false),
                    col("b", "int; DROP TABLE victims", false, None, false),
                ],
                indexes: vec![],
            },
        );
        dst.insert("t".to_string(), TableDetail { columns: vec![col("id", "integer", true, None, true)], indexes: vec![] });
        src.insert("fresh".to_string(), TableDetail { columns: vec![col("id", "integer", true, None, true)], indexes: vec![] });
        let ddl: HashMap<String, String> =
            [("fresh".to_string(), "CREATE TABLE fresh (\n  id integer DEFAULT 1); DROP TABLE victims; --\n)".to_string())].into();
        for d in [Dialect::Pg, Dialect::Mysql, Dialect::Sqlite] {
            let r = diff(&src, &dst, &ddl, d);
            for line in r.sql.lines() {
                if line.trim_start().starts_with("--") {
                    continue;
                }
                // 注入的文字只能出现在转义好的字符串里:引号外不能有分号或换行
                assert!(!suspicious(line), "{d:?} 注入到了可执行行:{line}");
                assert!(!line.starts_with("DROP"), "{d:?}:{line}");
            }
        }
        // MySQL:非表达式的默认值当字符串
        assert_eq!(default_sql("x') ; DROP TABLE v; --(", Dialect::Mysql), "'x'') ; DROP TABLE v; --('");
        assert_eq!(default_sql("CURRENT_TIMESTAMP(3)", Dialect::Mysql), "CURRENT_TIMESTAMP(3)");
        assert!(suspicious("ALTER TABLE t ADD COLUMN b int; DROP TABLE v;"));
        assert!(!suspicious("ALTER TABLE \"a;b\" ADD COLUMN c text DEFAULT 'x;y';"));
    }
}
