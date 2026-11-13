//! 表结构对比:比较两个同类型的库(如 staging 与 prod),列出差异并生成迁移脚本
//! (让「目标库」与「源库」一致)。
//!
//! 比较:表的增删;列的增删、类型、非空、默认值、注释;索引的增删(主键只提示,不自动改);
//! 外键(按列与引用对齐,不看约束名);表注释;视图与触发器的增删改。
//! 脚本里删除类语句(删表、删列、删索引、删外键、删视图 / 触发器)一律注释掉,确认后再取消注释;
//! 只有「改视图 / 触发器」需要的先删后建是可执行的(不丢数据)。
//! SQLite 不支持修改列定义与追加外键,写成注释说明需要重建表;
//! 触发器体里有多条语句(BEGIN … END)时整段注释,需在支持 DELIMITER 的客户端里手动执行。

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
    /// table / view / trigger
    pub kind: &'static str,
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

/// 能否把这段文字当作「一条语句」放进可执行脚本:除末尾的分号外不能再有分号——
/// 连字符串、引号标识符、注释里的也不行。不去猜引号与注释的边界:各数据库(以及编辑器的语句切分)
/// 对 `\'`、`"it's"`、`/* ' */` 这类写法的理解不一致,只要没有分号,谁都拆不出第二条语句。
/// 代价是定义里恰好有分号的(如字符串常量 'a;b')也会被注释掉,交给人工确认。
fn single_statement(stmt: &str) -> bool {
    !stmt.trim().trim_end_matches(';').contains(';')
}

/// 单行语句(列定义、索引等)是否异常:多条语句,或含换行(来自数据库的类型、默认值里出现这些,说明定义异常)。
fn suspicious(stmt: &str) -> bool {
    !single_statement(stmt) || stmt.contains(['\n', '\r'])
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
    // MySQL 的列注释是列定义的一部分:MODIFY 不带上就会把注释清掉
    if d == Dialect::Mysql {
        if let Some(cm) = comment_of(&c.comment) {
            s.push_str(&format!(" COMMENT {}", literal(cm, d)));
        }
    }
    s
}

/// 注释:空串与没有视为相同。
fn comment_of(c: &Option<String>) -> Option<&str> {
    c.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// 空白折叠成一个空格(比较视图 / 触发器定义时忽略排版差异)。
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------- 表以外的对象

/// 外键(可能是复合外键)。比较时不看约束名:自动生成的名字在两个库里常常不同。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fk {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    /// 规范成大写的动作(NO ACTION / CASCADE / SET NULL / SET DEFAULT / RESTRICT)
    pub on_delete: String,
    pub on_update: String,
}

impl Fk {
    fn same(&self, o: &Fk) -> bool {
        let lc = |v: &[String]| v.iter().map(|x| norm(x)).collect::<Vec<_>>();
        lc(&self.columns) == lc(&o.columns)
            && norm(&self.ref_table) == norm(&o.ref_table)
            && lc(&self.ref_columns) == lc(&o.ref_columns)
            && self.on_delete == o.on_delete
            && self.on_update == o.on_update
    }

    fn describe(&self) -> String {
        format!("({}) → {}({})", self.columns.join(", "), self.ref_table, self.ref_columns.join(", "))
    }

    fn add_sql(&self, table: &str, d: Dialect) -> String {
        let cols = |v: &[String]| v.iter().map(|c| ident(c, d)).collect::<Vec<_>>().join(", ");
        let mut s = format!(
            "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {}",
            ident(table, d),
            ident(&self.name, d),
            cols(&self.columns),
            ident(&self.ref_table, d)
        );
        if !self.ref_columns.is_empty() {
            s.push_str(&format!(" ({})", cols(&self.ref_columns)));
        }
        for (what, act) in [("DELETE", &self.on_delete), ("UPDATE", &self.on_update)] {
            if fk_action(act).is_some() && act != "NO ACTION" {
                s.push_str(&format!(" ON {what} {act}"));
            }
        }
        s.push(';');
        s
    }

    fn drop_sql(&self, table: &str, d: Dialect) -> String {
        match d {
            Dialect::Mysql => format!("ALTER TABLE {} DROP FOREIGN KEY {};", ident(table, d), ident(&self.name, d)),
            _ => format!("ALTER TABLE {} DROP CONSTRAINT {};", ident(table, d), ident(&self.name, d)),
        }
    }
}

/// 只认这几种外键动作(来自数据库的值,写进脚本前白名单校验)。
fn fk_action(a: &str) -> Option<&'static str> {
    ["NO ACTION", "CASCADE", "SET NULL", "SET DEFAULT", "RESTRICT"].into_iter().find(|x| *x == a)
}

/// 触发器:所属表与完整的建触发器语句。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub table: String,
    pub sql: String,
}

/// 表以外的结构:视图、触发器、外键、表注释。
#[derive(Debug, Clone, Default)]
pub struct Extras {
    /// 视图名 → 定义(PG / MySQL 为 SELECT 部分;SQLite 为完整的 CREATE VIEW)
    pub views: BTreeMap<String, String>,
    pub triggers: BTreeMap<String, Trigger>,
    /// 表名 → 外键
    pub fks: BTreeMap<String, Vec<Fk>>,
    /// 表名 → 表注释(没有的不出现)
    pub table_comments: BTreeMap<String, String>,
}

fn view_sql(name: &str, def: &str, d: Dialect) -> String {
    let body = def.trim().trim_end_matches(';').trim_end();
    match d {
        Dialect::Sqlite => format!("{body};"),
        _ => format!("CREATE OR REPLACE VIEW {} AS\n{body};", ident(name, d)),
    }
}

/// 多行定义(视图、触发器)的出口:不是单条语句(见 [`single_statement`])就整段注释并提示。
fn checked_block(stmt: &str, note: &str) -> Vec<String> {
    if !single_statement(stmt) {
        vec![format!("-- {}", comment_text(note)), commented(stmt.trim())]
    } else {
        vec![stmt.trim().to_string()]
    }
}

fn table_comment_sql(table: &str, comment: Option<&str>, d: Dialect) -> Option<String> {
    let tq = ident(table, d);
    match d {
        Dialect::Pg => Some(format!(
            "COMMENT ON TABLE {tq} IS {};",
            comment.map(|c| literal(c, d)).unwrap_or_else(|| "NULL".into())
        )),
        Dialect::Mysql => Some(format!("ALTER TABLE {tq} COMMENT = {};", literal(comment.unwrap_or(""), d))),
        Dialect::Sqlite => None,
    }
}

/// 视图与触发器的差异 → (报告条目, 脚本块)。
fn diff_objects(src: &Extras, dst: &Extras, d: Dialect) -> (Vec<TableDiff>, Vec<String>) {
    let mut items = vec![];
    let mut blocks = vec![];
    let entry = |kind, name: &str, status, change: String| TableDiff { kind, name: name.to_string(), status, changes: vec![change] };

    for (name, def) in &src.views {
        let header = format!("-- {} {}", t!("core.diff.view"), comment_text(name));
        match dst.views.get(name) {
            Some(o) if squash(o) == squash(def) => {}
            found => {
                let mut sql = vec![header];
                let create = checked_block(&view_sql(name, def, d), &t!("core.diff.suspicious"));
                if found.is_some() {
                    items.push(entry("view", name, "changed", t!("core.diff.view_changed").to_string()));
                    if d == Dialect::Sqlite {
                        // 先删后建:新定义被注释(要人工确认)时,删除也注释,不能只删不建
                        let drop = format!("DROP VIEW {};", ident(name, d));
                        sql.push(if create.len() == 1 { drop } else { commented(&drop) });
                    }
                } else {
                    items.push(entry("view", name, "added", t!("core.diff.view_added").to_string()));
                }
                sql.extend(create);
                blocks.push(sql.join("\n"));
            }
        }
    }
    for name in dst.views.keys().filter(|n| !src.views.contains_key(*n)) {
        items.push(entry("view", name, "removed", t!("core.diff.view_removed").to_string()));
        blocks.push(format!(
            "-- {} {}\n{}",
            t!("core.diff.view"),
            comment_text(name),
            commented(&format!("DROP VIEW {};", ident(name, d)))
        ));
    }

    let drop_trigger = |name: &str, tr: &Trigger| match d {
        Dialect::Pg => format!("DROP TRIGGER {} ON {};", ident(name, d), ident(&tr.table, d)),
        _ => format!("DROP TRIGGER {};", ident(name, d)),
    };
    for (name, tr) in &src.triggers {
        let header = format!("-- {} {} ({})", t!("core.diff.trigger"), comment_text(name), comment_text(&tr.table));
        match dst.triggers.get(name) {
            Some(o) if squash(&o.sql) == squash(&tr.sql) && o.table == tr.table => {}
            found => {
                let mut sql = vec![header];
                let create = format!("{};", tr.sql.trim().trim_end_matches(';'));
                // 触发器体是多条语句时(BEGIN … END)按分号执行会被拆开:整段(连同删除)注释,手动执行
                let manual = !single_statement(&tr.sql);
                if let Some(o) = found {
                    items.push(entry("trigger", name, "changed", t!("core.diff.trigger_changed").to_string()));
                    let drop = drop_trigger(name, o);
                    sql.push(if manual { commented(&drop) } else { drop });
                } else {
                    items.push(entry("trigger", name, "added", t!("core.diff.trigger_added").to_string()));
                }
                if manual {
                    sql.push(format!("-- {}", comment_text(&t!("core.diff.trigger_manual"))));
                    sql.push(commented(&create));
                } else {
                    sql.extend(checked_block(&create, &t!("core.diff.suspicious")));
                }
                blocks.push(sql.join("\n"));
            }
        }
    }
    for (name, tr) in dst.triggers.iter().filter(|(n, _)| !src.triggers.contains_key(*n)) {
        items.push(entry("trigger", name, "removed", t!("core.diff.trigger_removed").to_string()));
        blocks.push(format!(
            "-- {} {} ({})\n{}",
            t!("core.diff.trigger"),
            comment_text(name),
            comment_text(&tr.table),
            commented(&drop_trigger(name, tr))
        ));
    }
    (items, blocks)
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

/// 一张两边都有的表:列、索引、外键、表注释的差异 → (说明, 语句)。
fn diff_table(
    name: &str,
    src: &TableDetail,
    dst: &TableDetail,
    (sx, dx): (&Extras, &Extras),
    d: Dialect,
) -> (Vec<String>, Vec<String>) {
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
                let comment_changed = d != Dialect::Sqlite && comment_of(&o.comment) != comment_of(&c.comment);
                if !(type_changed || null_changed || def_changed || comment_changed) {
                    continue;
                }
                if comment_changed {
                    notes.push(
                        t!(
                            "core.diff.col_comment",
                            col = c.name,
                            from = comment_of(&o.comment).unwrap_or("—"),
                            to = comment_of(&c.comment).unwrap_or("—")
                        )
                        .to_string(),
                    );
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
                        if comment_changed {
                            sql.push(column_comment_sql(&tq, &cq, comment_of(&c.comment), d));
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
    // PG 新加的列:注释另写一条
    if d == Dialect::Pg {
        for c in src.columns.iter().filter(|c| !dst_cols.contains_key(c.name.as_str())) {
            if let Some(cm) = comment_of(&c.comment) {
                sql.push(column_comment_sql(&tq, &ident(&c.name, d), Some(cm), d));
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

    // 外键:按列与引用对齐(不看约束名)
    let none = vec![];
    let src_fks = sx.fks.get(name).unwrap_or(&none);
    let dst_fks = dx.fks.get(name).unwrap_or(&none);
    for f in src_fks.iter().filter(|f| !dst_fks.iter().any(|o| o.same(f))) {
        notes.push(t!("core.diff.fk_added", fk = f.describe()).to_string());
        if d == Dialect::Sqlite {
            sql.push(format!("-- {}", comment_text(&t!("core.diff.sqlite_fk", table = name))));
            sql.push(commented(&f.add_sql(name, d)));
        } else {
            sql.extend(checked(f.add_sql(name, d)));
        }
    }
    for o in dst_fks.iter().filter(|o| !src_fks.iter().any(|f| f.same(o))) {
        notes.push(t!("core.diff.fk_removed", fk = o.describe()).to_string());
        if d == Dialect::Sqlite {
            sql.push(format!("-- {}", comment_text(&t!("core.diff.sqlite_fk", table = name))));
        }
        sql.push(commented(&o.drop_sql(name, d)));
    }

    // 表注释
    let (sc, dc) = (sx.table_comments.get(name).map(String::as_str), dx.table_comments.get(name).map(String::as_str));
    let (sc, dc) = (sc.map(str::trim).filter(|c| !c.is_empty()), dc.map(str::trim).filter(|c| !c.is_empty()));
    if sc != dc {
        if let Some(stmt) = table_comment_sql(name, sc, d) {
            notes.push(t!("core.diff.table_comment", from = dc.unwrap_or("—"), to = sc.unwrap_or("—")).to_string());
            sql.push(stmt);
        }
    }
    (notes, sql)
}

fn column_comment_sql(tq: &str, cq: &str, comment: Option<&str>, d: Dialect) -> String {
    format!(
        "COMMENT ON COLUMN {tq}.{cq} IS {};",
        comment.map(|c| literal(c, d)).unwrap_or_else(|| "NULL".into())
    )
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
    extras: (&Extras, &Extras),
    d: Dialect,
) -> DiffReport {
    let (sx, dx) = extras;
    let mut tables = vec![];
    let mut blocks: Vec<String> = vec![];
    // 新表的外键放在脚本最后:被引用的表可能排在后面才建
    let mut late: Vec<String> = vec![];
    let mut same = 0;
    for (name, src) in source {
        match target.get(name) {
            None => {
                tables.push(TableDiff {
                    kind: "table",
                    name: name.clone(),
                    status: "added",
                    changes: vec![t!("core.diff.table_added").to_string()],
                });
                let header = format!("-- {}", comment_text(name));
                match create_sql.get(name) {
                    // 建表语句本来就跨多行;只查引号外的分号(会拆出第二条语句),有就整段注释掉
                    Some(ddl) if !single_statement(ddl) => blocks.push(format!(
                        "{header}\n-- {}\n{}",
                        t!("core.diff.suspicious"),
                        commented(ddl.trim())
                    )),
                    Some(ddl) => blocks.push(format!("{header}\n{};", ddl.trim().trim_end_matches(';'))),
                    None => blocks.push(format!("{header}\n-- {}", comment_text(&t!("core.diff.no_ddl", table = name)))),
                }
                // 合成的 PG 建表语句不带外键与注释:另外补上(MySQL 的 SHOW CREATE TABLE、SQLite 的原始建表语句都已包含)
                if d == Dialect::Pg {
                    let tq = ident(name, d);
                    let mut extra = vec![];
                    if let Some(stmt) = sx.table_comments.get(name).and_then(|c| table_comment_sql(name, Some(c), d)) {
                        extra.extend(checked(stmt));
                    }
                    for c in &src.columns {
                        if let Some(cm) = comment_of(&c.comment) {
                            extra.extend(checked(column_comment_sql(&tq, &ident(&c.name, d), Some(cm), d)));
                        }
                    }
                    if let Some(last) = blocks.last_mut() {
                        for e in extra {
                            last.push('\n');
                            last.push_str(&e);
                        }
                    }
                    for f in sx.fks.get(name).into_iter().flatten() {
                        late.extend(checked(f.add_sql(name, d)));
                    }
                }
            }
            Some(dst) => {
                let (notes, sql) = diff_table(name, src, dst, extras, d);
                if notes.is_empty() {
                    same += 1;
                } else {
                    tables.push(TableDiff { kind: "table", name: name.clone(), status: "changed", changes: notes });
                    blocks.push(format!("-- {}\n{}", comment_text(name), sql.join("\n")));
                }
            }
        }
    }
    for name in target.keys() {
        if !source.contains_key(name) {
            tables.push(TableDiff {
                kind: "table",
                name: name.clone(),
                status: "removed",
                changes: vec![t!("core.diff.table_removed").to_string()],
            });
            blocks.push(format!("-- {}\n{}", comment_text(name), commented(&format!("DROP TABLE {};", ident(name, d)))));
        }
    }
    if !late.is_empty() {
        blocks.push(format!("-- {}\n{}", t!("core.diff.new_fks"), late.join("\n")));
    }
    let (objects, object_blocks) = diff_objects(sx, dx, d);
    tables.extend(objects);
    blocks.extend(object_blocks);
    let sql = if blocks.is_empty() {
        format!("-- {}", t!("core.diff.identical"))
    } else {
        format!("-- {}\n\n{}\n", t!("core.diff.header"), blocks.join("\n\n"))
    };
    DiffReport { tables, same, sql }
}

/// 读出一个库的全部表结构(不含视图:MySQL 的 SHOW TABLES 会把视图也列出来)。
async fn snapshot(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    views: &BTreeMap<String, String>,
) -> AppResult<BTreeMap<String, TableDetail>> {
    let mut out = BTreeMap::new();
    for table in service::list_objects(state, kind, id).await? {
        if views.contains_key(&table) {
            continue;
        }
        let detail = service::table_detail(state, kind, id, &table).await?;
        out.insert(table, detail);
    }
    Ok(out)
}

type Rows = Vec<Vec<Option<String>>>;

async fn rows(state: &AppState, kind: &DbKind, id: &str, sql: &str) -> AppResult<Rows> {
    Ok(service::query(state, kind, id, sql, None).await?.rows)
}

fn cell(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

/// PG 外键动作的单字母代码 → 关键字。
fn pg_fk_action(c: &str) -> String {
    match c {
        "c" => "CASCADE",
        "n" => "SET NULL",
        "d" => "SET DEFAULT",
        "r" => "RESTRICT",
        _ => "NO ACTION",
    }
    .into()
}

/// 外键行(约束, 表, 列, 被引表, 被引列, ON DELETE, ON UPDATE)按约束聚成复合外键。
fn group_fks(rows: &Rows, action: impl Fn(&str) -> String) -> BTreeMap<String, Vec<Fk>> {
    let mut out: BTreeMap<String, Vec<Fk>> = BTreeMap::new();
    for r in rows {
        let (name, table) = (cell(r, 0), cell(r, 1));
        let list = out.entry(table).or_default();
        let fk = match list.iter_mut().find(|f| f.name == name) {
            Some(f) => f,
            None => {
                list.push(Fk {
                    name: name.clone(),
                    columns: vec![],
                    ref_table: cell(r, 3),
                    ref_columns: vec![],
                    on_delete: action(&cell(r, 5)),
                    on_update: action(&cell(r, 6)),
                });
                list.last_mut().expect("just pushed")
            }
        };
        fk.columns.push(cell(r, 2));
        let rc = cell(r, 4);
        if !rc.is_empty() {
            fk.ref_columns.push(rc);
        }
    }
    out
}

fn upper_action(a: &str) -> String {
    let a = a.trim().to_ascii_uppercase();
    if a.is_empty() { "NO ACTION".into() } else { a }
}

/// 视图、触发器、外键与表注释。
async fn extras(state: &AppState, kind: &DbKind, id: &str, d: Dialect) -> AppResult<Extras> {
    let mut x = Extras::default();
    match d {
        Dialect::Pg => {
            for r in rows(state, kind, id, "SELECT c.relname, pg_get_viewdef(c.oid, true) FROM pg_class c \
                JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind = 'v' AND n.nspname = 'public' ORDER BY 1").await? {
                x.views.insert(cell(&r, 0), cell(&r, 1));
            }
            for r in rows(state, kind, id, "SELECT t.tgname, c.relname, pg_get_triggerdef(t.oid, true) FROM pg_trigger t \
                JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                WHERE NOT t.tgisinternal AND n.nspname = 'public' ORDER BY 1").await? {
                x.triggers.insert(cell(&r, 0), Trigger { table: cell(&r, 1), sql: cell(&r, 2) });
            }
            let fk = rows(state, kind, id, "SELECT con.conname, rel.relname, att.attname, frel.relname, fatt.attname, \
                con.confdeltype::text, con.confupdtype::text \
                FROM pg_constraint con \
                JOIN pg_class rel ON rel.oid = con.conrelid \
                JOIN pg_class frel ON frel.oid = con.confrelid \
                JOIN pg_namespace ns ON ns.oid = rel.relnamespace \
                CROSS JOIN LATERAL unnest(con.conkey, con.confkey) WITH ORDINALITY AS k(attnum, fattnum, ord) \
                JOIN pg_attribute att ON att.attrelid = con.conrelid AND att.attnum = k.attnum \
                JOIN pg_attribute fatt ON fatt.attrelid = con.confrelid AND fatt.attnum = k.fattnum \
                WHERE con.contype = 'f' AND ns.nspname = 'public' \
                ORDER BY rel.relname, con.conname, k.ord").await?;
            x.fks = group_fks(&fk, pg_fk_action);
            for r in rows(state, kind, id, "SELECT c.relname, obj_description(c.oid, 'pg_class') FROM pg_class c \
                JOIN pg_namespace n ON n.oid = c.relnamespace \
                WHERE c.relkind IN ('r', 'p') AND n.nspname = 'public' AND obj_description(c.oid, 'pg_class') IS NOT NULL").await? {
                x.table_comments.insert(cell(&r, 0), cell(&r, 1));
            }
        }
        Dialect::Mysql => {
            // 视图定义里的表名带库名前缀(`db`.`t`):去掉,否则不同名的两个库永远对不上
            for r in rows(state, kind, id, "SELECT TABLE_NAME, VIEW_DEFINITION, DATABASE() FROM information_schema.VIEWS \
                WHERE TABLE_SCHEMA = DATABASE() ORDER BY 1").await? {
                let prefix = format!("{}.", ident(&cell(&r, 2), d));
                x.views.insert(cell(&r, 0), cell(&r, 1).replace(&prefix, ""));
            }
            for r in rows(state, kind, id, "SELECT TRIGGER_NAME, EVENT_OBJECT_TABLE, ACTION_TIMING, EVENT_MANIPULATION, \
                ACTION_STATEMENT FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = DATABASE() ORDER BY 1").await? {
                let (name, table) = (cell(&r, 0), cell(&r, 1));
                // 时机与事件来自数据库,只认固定的关键字
                let timing = cell(&r, 2).to_ascii_uppercase();
                let event = cell(&r, 3).to_ascii_uppercase();
                if !["BEFORE", "AFTER"].contains(&timing.as_str()) || !["INSERT", "UPDATE", "DELETE"].contains(&event.as_str()) {
                    continue;
                }
                let sql = format!(
                    "CREATE TRIGGER {} {timing} {event} ON {} FOR EACH ROW {}",
                    ident(&name, d),
                    ident(&table, d),
                    cell(&r, 4)
                );
                x.triggers.insert(name, Trigger { table, sql });
            }
            let fk = rows(state, kind, id, "SELECT k.CONSTRAINT_NAME, k.TABLE_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_NAME, \
                k.REFERENCED_COLUMN_NAME, rc.DELETE_RULE, rc.UPDATE_RULE \
                FROM information_schema.KEY_COLUMN_USAGE k \
                JOIN information_schema.REFERENTIAL_CONSTRAINTS rc \
                  ON rc.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND rc.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND rc.TABLE_NAME = k.TABLE_NAME \
                WHERE k.TABLE_SCHEMA = DATABASE() AND k.REFERENCED_TABLE_NAME IS NOT NULL \
                ORDER BY k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION").await?;
            x.fks = group_fks(&fk, upper_action);
            for r in rows(state, kind, id, "SELECT TABLE_NAME, TABLE_COMMENT FROM information_schema.TABLES \
                WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE' AND TABLE_COMMENT <> ''").await? {
                x.table_comments.insert(cell(&r, 0), cell(&r, 1));
            }
        }
        Dialect::Sqlite => {
            for r in rows(state, kind, id, "SELECT name, sql FROM sqlite_master WHERE type = 'view' ORDER BY name").await? {
                x.views.insert(cell(&r, 0), cell(&r, 1));
            }
            for r in rows(state, kind, id, "SELECT name, tbl_name, sql FROM sqlite_master WHERE type = 'trigger' ORDER BY name").await? {
                x.triggers.insert(cell(&r, 0), Trigger { table: cell(&r, 1), sql: cell(&r, 2) });
            }
            // SQLite 的外键没有名字:按表内序号合成
            let fk = rows(state, kind, id, "SELECT 'fk_' || m.name || '_' || f.id, m.name, f.\"from\", f.\"table\", f.\"to\", \
                f.on_delete, f.on_update FROM sqlite_master m JOIN pragma_foreign_key_list(m.name) f \
                WHERE m.type = 'table' ORDER BY m.name, f.id, f.seq").await?;
            x.fks = group_fks(&fk, upper_action);
        }
    }
    Ok(x)
}

/// SQLite 新表用库里保存的原始建表语句(带外键、约束),比按列合成的更完整。
async fn sqlite_create_sql(state: &AppState, kind: &DbKind, id: &str, table: &str) -> Option<String> {
    let sql = format!("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = {}", literal(table, Dialect::Sqlite));
    rows(state, kind, id, &sql).await.ok()?.first().map(|r| cell(r, 0)).filter(|s| !s.is_empty())
}

/// 比较两个连接(必须同一种数据库):生成让目标库与源库一致的脚本。
pub async fn compare(state: &AppState, kind: &DbKind, source_id: &str, target_id: &str) -> AppResult<DiffReport> {
    let d = Dialect::from_kind(kind).ok_or_else(|| AppError::new(ErrorKind::Query, t!("core.err.redis_unsupported")))?;
    let sx = extras(state, kind, source_id, d).await?;
    let dx = extras(state, kind, target_id, d).await?;
    let source = snapshot(state, kind, source_id, &sx.views).await?;
    let target = snapshot(state, kind, target_id, &dx.views).await?;
    let mut create_sql = HashMap::new();
    for name in source.keys().filter(|n| !target.contains_key(*n)) {
        let ddl = if d == Dialect::Sqlite {
            sqlite_create_sql(state, kind, source_id, name).await
        } else {
            None
        };
        let ddl = match ddl {
            Some(s) => Some(s),
            None => service::create_table_ddl(state, kind, source_id, name).await.ok(),
        };
        if let Some(ddl) = ddl {
            create_sql.insert(name.clone(), ddl);
        }
    }
    Ok(diff(&source, &target, &create_sql, (&sx, &dx), d))
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
        let r = diff(&src, &dst, &ddl, (&Extras::default(), &Extras::default()), Dialect::Pg);
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
        let r = diff(&src, &dst, &HashMap::new(), (&Extras::default(), &Extras::default()), Dialect::Mysql);
        assert!(r.sql.contains("ALTER TABLE `orders` MODIFY COLUMN `status` varchar(20) NOT NULL DEFAULT "));
        assert!(r.sql.contains("-- DROP INDEX `orders_legacy_idx` ON `orders`;"));
        // MySQL 的默认值原样是不带引号的值
        assert_eq!(default_sql("new", Dialect::Mysql), "'new'");
        assert_eq!(default_sql("0", Dialect::Mysql), "0");
        assert_eq!(default_sql("CURRENT_TIMESTAMP", Dialect::Mysql), "CURRENT_TIMESTAMP");

        let r = diff(&src, &dst, &HashMap::new(), (&Extras::default(), &Extras::default()), Dialect::Sqlite);
        assert!(r.sql.contains("-- ") && !r.sql.contains("ALTER COLUMN"), "SQLite 不支持改列,只写注释");
        assert!(r.sql.contains("ALTER TABLE \"orders\" ADD COLUMN \"note\" text;"));
    }

    #[test]
    fn identical_schemas() {
        let (src, _) = schemas();
        let r = diff(&src, &src, &HashMap::new(), (&Extras::default(), &Extras::default()), Dialect::Pg);
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
            let r = diff(&src, &dst, &ddl, (&Extras::default(), &Extras::default()), d);
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
        assert!(!suspicious("ALTER TABLE \"ab\" ADD COLUMN c text DEFAULT 'x y';"));
        // 引号、注释里的分号也算:各家对引号 / 注释边界的理解不一致,不去猜
        assert!(suspicious("ALTER TABLE \"a;b\" ADD COLUMN c text;"));
        assert!(suspicious("SELECT 1 -- it's\n; DROP TABLE x; -- '"));
        assert!(!single_statement("CREATE VIEW v AS SELECT \"it's\" FROM t ; DROP TABLE x; SELECT 'z'"));
        assert!(!single_statement("SELECT 'a\\'' ; DROP TABLE x; SELECT '"));
    }

    fn fk(name: &str, cols: &[&str], rt: &str, rc: &[&str], del: &str) -> Fk {
        Fk {
            name: name.into(),
            columns: cols.iter().map(|c| c.to_string()).collect(),
            ref_table: rt.into(),
            ref_columns: rc.iter().map(|c| c.to_string()).collect(),
            on_delete: del.into(),
            on_update: "NO ACTION".into(),
        }
    }

    fn one_table() -> BTreeMap<String, TableDetail> {
        let mut m = BTreeMap::new();
        m.insert(
            "orders".to_string(),
            TableDetail {
                columns: vec![col("id", "integer", true, None, true), col("user_id", "integer", false, None, false)],
                indexes: vec![],
            },
        );
        m
    }

    #[test]
    fn views_triggers_fks_and_comments() {
        let tables = one_table();
        let mut sx = Extras::default();
        let mut dx = Extras::default();
        sx.views.insert("v_paid".into(), "SELECT id FROM orders WHERE paid".into());
        dx.views.insert("v_paid".into(), "SELECT  id\n FROM orders\n WHERE paid".into());
        sx.views.insert("v_new".into(), "SELECT 1".into());
        dx.views.insert("v_old".into(), "SELECT 2".into());
        sx.triggers.insert("t_upd".into(), Trigger { table: "orders".into(), sql: "CREATE TRIGGER t_upd BEFORE UPDATE ON orders FOR EACH ROW EXECUTE FUNCTION touch()".into() });
        dx.triggers.insert("t_upd".into(), Trigger { table: "orders".into(), sql: "CREATE TRIGGER t_upd AFTER UPDATE ON orders FOR EACH ROW EXECUTE FUNCTION touch()".into() });
        // 名字不同但内容一样的外键不算差异;动作不同算
        sx.fks.insert("orders".into(), vec![fk("orders_user_fk", &["user_id"], "users", &["id"], "CASCADE")]);
        dx.fks.insert("orders".into(), vec![fk("orders_ibfk_1", &["user_id"], "users", &["id"], "NO ACTION")]);
        sx.table_comments.insert("orders".into(), "订单".into());

        let r = diff(&tables, &tables, &HashMap::new(), (&sx, &dx), Dialect::Pg);
        let got: Vec<(&str, &str, &str)> = r.tables.iter().map(|t| (t.kind, t.name.as_str(), t.status)).collect();
        assert_eq!(
            got,
            vec![
                ("table", "orders", "changed"),
                ("view", "v_new", "added"),
                ("view", "v_old", "removed"),
                ("trigger", "t_upd", "changed"),
            ],
            "排版不同的视图不算差异"
        );
        let s = &r.sql;
        assert!(s.contains("ALTER TABLE \"orders\" ADD CONSTRAINT \"orders_user_fk\" FOREIGN KEY (\"user_id\") REFERENCES \"users\" (\"id\") ON DELETE CASCADE;"));
        assert!(s.contains("-- ALTER TABLE \"orders\" DROP CONSTRAINT \"orders_ibfk_1\";"));
        assert!(s.contains("COMMENT ON TABLE \"orders\" IS '订单';"));
        assert!(s.contains("CREATE OR REPLACE VIEW \"v_new\" AS\nSELECT 1;"));
        assert!(s.contains("-- DROP VIEW \"v_old\";"));
        // 改触发器:先删后建,都可执行
        assert!(s.contains("DROP TRIGGER \"t_upd\" ON \"orders\";\nCREATE TRIGGER t_upd BEFORE UPDATE"));

        // MySQL:触发器体是 BEGIN … END 时整段注释;表注释用 ALTER TABLE … COMMENT
        sx.triggers.insert(
            "t_upd".into(),
            Trigger { table: "orders".into(), sql: "CREATE TRIGGER `t_upd` BEFORE UPDATE ON `orders` FOR EACH ROW BEGIN SET NEW.a = 1; SET NEW.b = 2; END".into() },
        );
        let r = diff(&tables, &tables, &HashMap::new(), (&sx, &dx), Dialect::Mysql);
        assert!(r.sql.contains("-- DROP TRIGGER `t_upd`;"));
        assert!(r.sql.contains("-- CREATE TRIGGER `t_upd` BEFORE UPDATE"));
        assert!(r.sql.contains("ALTER TABLE `orders` COMMENT = '订单';"));
        assert!(r.sql.contains("ALTER TABLE `orders` DROP FOREIGN KEY `orders_ibfk_1`;"));

        // SQLite:外键不能追加,只写注释;改视图先删后建
        let r = diff(&tables, &tables, &HashMap::new(), (&sx, &dx), Dialect::Sqlite);
        assert!(!r.sql.lines().any(|l| l.starts_with("ALTER TABLE") && l.contains("FOREIGN KEY")));
        assert!(!r.sql.contains("COMMENT"));
    }

    #[test]
    fn sqlite_view_drop_only_with_executable_create() {
        let tables = one_table();
        let mut sx = Extras::default();
        let mut dx = Extras::default();
        dx.views.insert("v".into(), "CREATE VIEW v AS SELECT 1".into());
        // 新定义里有分号(哪怕在字符串里):建视图要人工确认,删除也不能直接执行
        sx.views.insert("v".into(), "CREATE VIEW v AS SELECT 'a;b' AS x".into());
        let r = diff(&tables, &tables, &HashMap::new(), (&sx, &dx), Dialect::Sqlite);
        assert!(r.sql.contains("-- DROP VIEW \"v\";"), "{}", r.sql);
        assert!(!r.sql.lines().any(|l| l.starts_with("DROP VIEW")));
        // 正常的新定义:先删后建都可执行
        sx.views.insert("v".into(), "CREATE VIEW v AS SELECT 2".into());
        let r = diff(&tables, &tables, &HashMap::new(), (&sx, &dx), Dialect::Sqlite);
        assert!(r.sql.contains("DROP VIEW \"v\";\nCREATE VIEW v AS SELECT 2;"), "{}", r.sql);
    }

    #[test]
    fn column_comments() {
        let mut src = one_table();
        let dst = one_table();
        src.get_mut("orders").unwrap().columns[1].comment = Some("下单人".into());
        let none = Extras::default();
        let r = diff(&src, &dst, &HashMap::new(), (&none, &none), Dialect::Pg);
        assert!(r.sql.contains("COMMENT ON COLUMN \"orders\".\"user_id\" IS '下单人';"));
        let r = diff(&src, &dst, &HashMap::new(), (&none, &none), Dialect::Mysql);
        assert!(r.sql.contains("ALTER TABLE `orders` MODIFY COLUMN `user_id` integer COMMENT '下单人';"));
        // 空注释与没有注释相同
        let mut blank = one_table();
        blank.get_mut("orders").unwrap().columns[1].comment = Some(" ".into());
        assert!(diff(&blank, &dst, &HashMap::new(), (&none, &none), Dialect::Pg).tables.is_empty());
    }

    #[test]
    fn hostile_view_and_fk_action_stay_inert() {
        let tables = one_table();
        let mut sx = Extras::default();
        sx.views.insert("v".into(), "SELECT 1; DROP TABLE victims".into());
        let mut bad = fk("f", &["user_id"], "users", &["id"], "CASCADE; DROP TABLE victims");
        bad.on_update = "NO ACTION".into();
        sx.fks.insert("orders".into(), vec![bad]);
        for d in [Dialect::Pg, Dialect::Mysql] {
            let r = diff(&tables, &tables, &HashMap::new(), (&sx, &Extras::default()), d);
            for line in r.sql.lines().filter(|l| !l.trim_start().starts_with("--")) {
                assert!(!line.contains("victims"), "{d:?}:{line}");
            }
        }
    }
}
