//! MySQL 字符集 / 排序规则体检(诊断面板的「字符集」分区)。
//!
//! 查询在 diag.rs 里做(只读会话 + 服务端限时),这里只放纯逻辑:把 information_schema 的结果
//! 整理成 [`Snapshot`],再由 [`check`] 得出按严重程度排好的问题清单和转 utf8mb4 的脚本。
//!
//! 检查的几类问题:
//! - 仍用 `utf8` / `utf8mb3` 的库、表、列(存不了 4 字节字符,如 emoji);
//! - 外键两侧、以及按命名推断的常见 JOIN 列两侧字符集 / 排序规则不同(隐式转换,用不上索引);
//! - 列与表、表与库的排序规则不一致;
//! - 转成 utf8mb4 后索引超长(COMPACT / REDUNDANT 行格式下单列前缀上限 767 字节),转换会失败。
//!
//! 生成的语句只放进编辑器,执行时照常经过危险语句审查与改表结构风险评估(CONVERT 会判为 COPY)。

use crate::erd::infer_foreign_keys;
use crate::pg::client::QueryResult;
use crate::schema::{ColumnBrief, ColumnsByTable};
use crate::sql::alter::{Flavor, Server};
use crate::sql::build::{ident, literal};
use crate::sql::Dialect;
use rust_i18n::t;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// 系统库:不检查。
pub const SYSTEM_SCHEMAS: &[&str] = &["mysql", "information_schema", "performance_schema", "sys"];

/// 问题清单最多显示多少行(脚本里仍包含全部转换)。
pub const MAX_FINDINGS: usize = 500;

/// utf8mb4 每个字符最多占的字节数。
const MB4: u64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// 影响查询正确性 / 性能,或会让转换直接失败
    High,
    /// 存不了 4 字节字符
    Medium,
    /// 不一致但通常无害
    Low,
}

impl Severity {
    pub fn code(self) -> &'static str {
        match self {
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
        }
    }
}

/// 问题类型,顺序即同一严重程度内的显示顺序。
const ISSUES: &[&str] = &[
    "fk_mismatch",
    "join_mismatch",
    "index_too_long",
    "utf8mb3_schema",
    "utf8mb3_table",
    "utf8mb3_column",
    "column_collation",
    "table_collation",
    "truncated",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    pub name: String,
    pub collation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    pub schema: String,
    pub name: String,
    pub engine: Option<String>,
    pub row_format: Option<String>,
    /// TABLE_COLLATION
    pub collation: Option<String>,
}

/// 字符串列(CHARACTER_SET_NAME 非空的列)。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Column {
    pub schema: String,
    pub table: String,
    pub name: String,
    /// COLUMN_TYPE 原文(`varchar(255)`、`enum('a','b')`)
    pub column_type: String,
    /// CHARACTER_MAXIMUM_LENGTH(字符数)
    pub max_chars: Option<u64>,
    pub charset: String,
    pub collation: String,
    pub nullable: bool,
    /// COLUMN_DEFAULT 原文:MySQL 是值本身,MariaDB 10.2.7+ 是表达式(字符串带引号、`NULL`)
    pub default: Option<String>,
    pub extra: String,
    pub comment: String,
}

/// 索引里的一列(STATISTICS 的一行)。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IndexPart {
    pub schema: String,
    pub table: String,
    pub index: String,
    pub column: String,
    /// 前缀索引的长度(字符数)
    pub sub_part: Option<u64>,
    pub unique: bool,
    /// BTREE / FULLTEXT / SPATIAL / HASH
    pub kind: String,
}

/// 物理外键的一列配对。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fk {
    pub constraint: String,
    pub schema: String,
    pub table: String,
    pub column: String,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_column: String,
}

/// 一次体检需要的全部元数据。
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub server: Server,
    /// 检查了多个库:对象名带上库名
    pub multi_schema: bool,
    pub schemas: Vec<Schema>,
    pub tables: Vec<Table>,
    pub columns: Vec<Column>,
    pub indexes: Vec<IndexPart>,
    pub fks: Vec<Fk>,
    /// 服务器上可用的 utf8mb4 排序规则(小写);取不到为空,此时按名字直接映射
    pub utf8mb4_collations: Vec<String>,
    /// utf8mb4 的默认排序规则
    pub utf8mb4_default: Option<String>,
    /// 5.7 的 innodb_large_prefix;8.0 / MariaDB 10.3+ 已移除(None)
    pub large_prefix: Option<bool>,
    /// 列信息被截断时的上限(只检查了前这么多列)
    pub truncated_columns: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub issue: &'static str,
    pub object: String,
    pub current: String,
    pub suggested: String,
    pub note: String,
    /// 修复语句(放进编辑器,不执行);没有自动修复为 None
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    /// 把全部 utf8 / utf8mb3 的库和表转成 utf8mb4 的脚本;没有可转换的为 None
    pub script: Option<String>,
}

// ---------- 排序规则 ----------

pub fn is_utf8mb3(charset: &str) -> bool {
    charset.eq_ignore_ascii_case("utf8") || charset.eq_ignore_ascii_case("utf8mb3")
}

/// 排序规则 → 字符集(`utf8mb4_0900_ai_ci` → `utf8mb4`;字符集名里没有下划线)。
pub fn charset_of(collation: &str) -> String {
    collation.split('_').next().unwrap_or(collation).to_ascii_lowercase()
}

/// utf8 / utf8mb3 排序规则对应的 utf8mb4 版本:去掉前缀换成 `utf8mb4_`(general_ci、bin、unicode_ci、
/// unicode_520_ci、各语言规则、MariaDB 的 nopad / uca1400 都有同名的 utf8mb4 版本);
/// `general_mysql500_ci` 只有 utf8mb3 有,退回 general_ci。服务器上没有这个名字时用 utf8mb4 的默认规则。
/// 不是 utf8mb3 的规则原样返回。
pub fn utf8mb4_collation(old: &str, available: &[String], default: Option<&str>) -> String {
    let lower = old.to_ascii_lowercase();
    let Some(suffix) = lower.strip_prefix("utf8mb3_").or_else(|| lower.strip_prefix("utf8_")) else {
        return lower;
    };
    let suffix = if suffix == "general_mysql500_ci" { "general_ci" } else { suffix };
    let candidate = format!("utf8mb4_{suffix}");
    if available.is_empty() || available.iter().any(|c| c == &candidate) {
        return candidate;
    }
    default.map(str::to_ascii_lowercase).unwrap_or_else(|| "utf8mb4_general_ci".into())
}

/// 转换后可以另外考虑的新规则:MySQL 8.0+ 的 0900、MariaDB 10.10+ 的 uca1400(都要服务器上真有)。
fn modern_alternative(s: &Snapshot, target: &str) -> Option<&'static str> {
    let has = |c: &str| s.utf8mb4_collations.iter().any(|x| x == c);
    if !matches!(target, "utf8mb4_general_ci" | "utf8mb4_unicode_ci" | "utf8mb4_unicode_520_ci") {
        return None;
    }
    match s.server.flavor {
        Flavor::Mysql if s.server.version >= (8, 0, 0) && has("utf8mb4_0900_ai_ci") => Some("utf8mb4_0900_ai_ci"),
        Flavor::Mariadb if s.server.version >= (10, 10, 0) && has("utf8mb4_uca1400_ai_ci") => Some("utf8mb4_uca1400_ai_ci"),
        _ => None,
    }
}

fn upgrade(s: &Snapshot, collation: &str) -> String {
    if is_utf8mb3(&charset_of(collation)) {
        utf8mb4_collation(collation, &s.utf8mb4_collations, s.utf8mb4_default.as_deref())
    } else {
        collation.to_ascii_lowercase()
    }
}

// ---------- 从查询结果整理 ----------

fn cell(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

fn opt(row: &[Option<String>], i: usize) -> Option<String> {
    row.get(i).cloned().flatten().filter(|v| !v.is_empty())
}

fn num(row: &[Option<String>], i: usize) -> Option<u64> {
    row.get(i).cloned().flatten().and_then(|v| v.parse().ok())
}

/// `SELECT SCHEMA_NAME, DEFAULT_COLLATION_NAME FROM information_schema.SCHEMATA …`
pub fn schemas_from(r: &QueryResult) -> Vec<Schema> {
    r.rows.iter().map(|row| Schema { name: cell(row, 0), collation: cell(row, 1).to_ascii_lowercase() }).collect()
}

/// `SELECT TABLE_SCHEMA, TABLE_NAME, ENGINE, ROW_FORMAT, TABLE_COLLATION FROM information_schema.TABLES …`
pub fn tables_from(r: &QueryResult) -> Vec<Table> {
    r.rows
        .iter()
        .map(|row| Table {
            schema: cell(row, 0),
            name: cell(row, 1),
            engine: opt(row, 2),
            row_format: opt(row, 3),
            collation: opt(row, 4).map(|c| c.to_ascii_lowercase()),
        })
        .collect()
}

/// `SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, CHARACTER_MAXIMUM_LENGTH, CHARACTER_SET_NAME,
/// COLLATION_NAME, IS_NULLABLE, COLUMN_DEFAULT, EXTRA, COLUMN_COMMENT FROM information_schema.COLUMNS …`
pub fn columns_from(r: &QueryResult) -> Vec<Column> {
    r.rows
        .iter()
        .map(|row| Column {
            schema: cell(row, 0),
            table: cell(row, 1),
            name: cell(row, 2),
            column_type: cell(row, 3),
            max_chars: num(row, 4),
            charset: cell(row, 5).to_ascii_lowercase(),
            collation: cell(row, 6).to_ascii_lowercase(),
            nullable: cell(row, 7).eq_ignore_ascii_case("yes"),
            default: row.get(8).cloned().flatten(),
            extra: cell(row, 9),
            comment: cell(row, 10),
        })
        .collect()
}

/// `SELECT TABLE_SCHEMA, TABLE_NAME, INDEX_NAME, COLUMN_NAME, SUB_PART, NON_UNIQUE, INDEX_TYPE
/// FROM information_schema.STATISTICS …`(函数索引的 COLUMN_NAME 为 NULL,跳过)
pub fn indexes_from(r: &QueryResult) -> Vec<IndexPart> {
    r.rows
        .iter()
        .filter(|row| opt(row, 3).is_some())
        .map(|row| IndexPart {
            schema: cell(row, 0),
            table: cell(row, 1),
            index: cell(row, 2),
            column: cell(row, 3),
            sub_part: num(row, 4),
            unique: cell(row, 5) == "0",
            kind: cell(row, 6).to_ascii_uppercase(),
        })
        .collect()
}

/// `SELECT CONSTRAINT_NAME, TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME,
/// REFERENCED_COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE …`
pub fn fks_from(r: &QueryResult) -> Vec<Fk> {
    r.rows
        .iter()
        .map(|row| Fk {
            constraint: cell(row, 0),
            schema: cell(row, 1),
            table: cell(row, 2),
            column: cell(row, 3),
            ref_schema: cell(row, 4),
            ref_table: cell(row, 5),
            ref_column: cell(row, 6),
        })
        .collect()
}

// ---------- 检查 ----------

type ColKey = (String, String, String);

fn key(schema: &str, table: &str, column: &str) -> ColKey {
    (schema.to_string(), table.to_lowercase(), column.to_lowercase())
}

struct Ctx<'a> {
    s: &'a Snapshot,
    columns: HashMap<ColKey, &'a Column>,
    tables: HashMap<(String, String), &'a Table>,
}

impl<'a> Ctx<'a> {
    fn new(s: &'a Snapshot) -> Self {
        Self {
            s,
            columns: s.columns.iter().map(|c| (key(&c.schema, &c.table, &c.name), c)).collect(),
            tables: s.tables.iter().map(|t| ((t.schema.clone(), t.name.to_lowercase()), t)).collect(),
        }
    }

    fn column(&self, schema: &str, table: &str, column: &str) -> Option<&'a Column> {
        self.columns.get(&key(schema, table, column)).copied()
    }

    fn table(&self, schema: &str, table: &str) -> Option<&'a Table> {
        self.tables.get(&(schema.to_string(), table.to_lowercase())).copied()
    }

    /// 界面上显示的对象名:只检查一个库时省略库名。
    fn name(&self, parts: &[&str]) -> String {
        let parts = if self.s.multi_schema { parts } else { &parts[1..] };
        parts.join(".")
    }

    fn table_sql(&self, schema: &str, table: &str) -> String {
        format!("{}.{}", ident(schema, Dialect::Mysql), ident(table, Dialect::Mysql))
    }
}

/// 一张要转 utf8mb4 的表。
struct Conversion<'a> {
    table: &'a Table,
    target: String,
    /// 会被 CONVERT 一起改掉的、不是 utf8mb3 的字符串列(列名, 原规则)
    other_columns: Vec<(String, String)>,
    index_issues: Vec<IndexIssue>,
    in_fk: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexIssue {
    index: String,
    bytes: u64,
    limit: u64,
    /// 「COMPACT 行格式单列」这类说明
    format: String,
    /// 不超限时单列最多的字符数
    max_chars: u64,
    /// 换成 DYNAMIC 行格式就能解决
    row_format_fix: bool,
}

/// 转成 utf8mb4 后超长的索引。InnoDB:COMPACT / REDUNDANT(或 5.7 关了 innodb_large_prefix)单列前缀上限 767 字节,
/// DYNAMIC / COMPRESSED 为 3072;整个索引上限 3072(16K 页)。MyISAM 整个索引 1000 字节。
/// 只累计字符串列(数值列的字节数不计,整体长度是下限估算)。
fn index_issues(ctx: &Ctx, t: &Table) -> Vec<IndexIssue> {
    let engine = t.engine.as_deref().unwrap_or("").to_ascii_lowercase();
    let format = t.row_format.as_deref().unwrap_or("").to_ascii_lowercase();
    let (part_limit, total_limit, label, row_fix) = match engine.as_str() {
        "innodb" => {
            let old_format = matches!(format.as_str(), "compact" | "redundant");
            let no_large_prefix = ctx.s.large_prefix == Some(false);
            let label = if !old_format && no_large_prefix {
                "innodb_large_prefix = OFF".to_string()
            } else {
                format!("{} ROW_FORMAT", format.to_ascii_uppercase())
            };
            // 关了 large_prefix 时 DYNAMIC 也只有 767:改行格式解决不了
            let small = old_format || no_large_prefix;
            (if small { 767 } else { 3072 }, 3072, label, old_format && !no_large_prefix)
        }
        "myisam" => (1000, 1000, "MyISAM".to_string(), false),
        _ => return vec![],
    };
    let mut by_index: BTreeMap<&str, Vec<&IndexPart>> = BTreeMap::new();
    for p in ctx.s.indexes.iter().filter(|p| p.schema == t.schema && p.table.eq_ignore_ascii_case(&t.name)) {
        if matches!(p.kind.as_str(), "FULLTEXT" | "SPATIAL") {
            continue;
        }
        by_index.entry(p.index.as_str()).or_default().push(p);
    }
    let mut out = vec![];
    for (index, parts) in by_index {
        let bytes: Vec<u64> = parts
            .iter()
            .filter_map(|p| {
                let c = ctx.column(&t.schema, &t.name, &p.column)?;
                Some(p.sub_part.or(c.max_chars)? * MB4)
            })
            .collect();
        let widest = bytes.iter().copied().max().unwrap_or(0);
        let total: u64 = bytes.iter().sum();
        if widest > part_limit {
            out.push(IndexIssue {
                index: index.to_string(),
                bytes: widest,
                limit: part_limit,
                format: label.clone(),
                max_chars: part_limit / MB4,
                row_format_fix: row_fix,
            });
        } else if total > total_limit {
            out.push(IndexIssue {
                index: index.to_string(),
                bytes: total,
                limit: total_limit,
                format: label.clone(),
                max_chars: total_limit / MB4,
                row_format_fix: false,
            });
        }
    }
    out
}

/// 表转换用的排序规则:表本身是 utf8mb3 就映射表的规则;表已是 utf8mb4 沿用表的;
/// 否则(如 latin1 表里有 utf8mb3 列)映射第一个 utf8mb3 列的规则。
fn conversion_target(ctx: &Ctx, t: &Table, cols: &[&Column]) -> String {
    let table_coll = t.collation.clone().unwrap_or_default();
    let cs = charset_of(&table_coll);
    if is_utf8mb3(&cs) || cs == "utf8mb4" {
        return upgrade(ctx.s, &table_coll);
    }
    cols.iter()
        .find(|c| is_utf8mb3(&c.charset))
        .map(|c| upgrade(ctx.s, &c.collation))
        .unwrap_or_else(|| ctx.s.utf8mb4_default.clone().unwrap_or_else(|| "utf8mb4_general_ci".into()))
}

fn conversions<'a>(ctx: &Ctx<'a>) -> Vec<Conversion<'a>> {
    let fk_tables: HashSet<(String, String)> = ctx
        .s
        .fks
        .iter()
        .filter(|f| ctx.column(&f.schema, &f.table, &f.column).is_some())
        .flat_map(|f| [(f.schema.clone(), f.table.to_lowercase()), (f.ref_schema.clone(), f.ref_table.to_lowercase())])
        .collect();
    let mut out = vec![];
    for t in &ctx.s.tables {
        let cols: Vec<&Column> =
            ctx.s.columns.iter().filter(|c| c.schema == t.schema && c.table.eq_ignore_ascii_case(&t.name)).collect();
        let table_mb3 = t.collation.as_deref().is_some_and(|c| is_utf8mb3(&charset_of(c)));
        if !table_mb3 && !cols.iter().any(|c| is_utf8mb3(&c.charset)) {
            continue;
        }
        let target = conversion_target(ctx, t, &cols);
        let other_columns = cols
            .iter()
            .filter(|c| !is_utf8mb3(&c.charset) && c.collation != target)
            .map(|c| (c.name.clone(), c.collation.clone()))
            .collect();
        out.push(Conversion {
            table: t,
            target,
            other_columns,
            index_issues: index_issues(ctx, t),
            in_fk: fk_tables.contains(&(t.schema.clone(), t.name.to_lowercase())),
        });
    }
    out
}

fn comment(text: &str) -> String {
    text.lines().map(|l| format!("-- {l}\n")).collect()
}

fn index_text(i: &IndexIssue) -> String {
    t!(
        "core.charset.index_too_long",
        index = i.index,
        bytes = i.bytes,
        limit = i.limit,
        format = i.format,
        chars = i.max_chars
    )
    .to_string()
}

/// 一张表的转换语句(带注释说明)。
fn conversion_sql(ctx: &Ctx, c: &Conversion) -> String {
    let mut out = String::new();
    for i in &c.index_issues {
        out += &comment(&format!("{} {}", t!("core.charset.warning"), index_text(i)));
        if i.row_format_fix {
            out += &format!("-- ALTER TABLE {} ROW_FORMAT=DYNAMIC;\n", ctx.table_sql(&c.table.schema, &c.table.name));
        }
    }
    if !c.other_columns.is_empty() {
        let cols: Vec<String> = c.other_columns.iter().map(|(n, coll)| format!("{n} ({coll})")).collect();
        out += &comment(&t!("core.charset.script_other_columns", cols = cols.join(", "), coll = c.target));
    }
    if c.in_fk {
        out += &comment(&t!("core.charset.script_fk"));
    }
    out += &format!(
        "ALTER TABLE {} CONVERT TO CHARACTER SET utf8mb4 COLLATE {};\n",
        ctx.table_sql(&c.table.schema, &c.table.name),
        c.target
    );
    out
}

fn schema_sql(name: &str, target: &str) -> String {
    format!("ALTER DATABASE {} CHARACTER SET utf8mb4 COLLATE {};\n", ident(name, Dialect::Mysql), target)
}

/// 改一列的字符集与排序规则:`ALTER TABLE … MODIFY 列 类型 CHARACTER SET … COLLATE … [NOT] NULL [DEFAULT …] [COMMENT …]`。
/// 生成列、表达式默认值、ON UPDATE、不可见列等(EXTRA 非空)拼不全定义,返回 None。
pub fn modify_column(server: &Server, c: &Column, collation: &str, table_sql: &str) -> Option<String> {
    if !c.extra.trim().is_empty() {
        return None;
    }
    let mut s = format!(
        "ALTER TABLE {table_sql} MODIFY {} {} CHARACTER SET {} COLLATE {}",
        ident(&c.name, Dialect::Mysql),
        c.column_type,
        charset_of(collation),
        collation
    );
    s += if c.nullable { " NULL" } else { " NOT NULL" };
    // MariaDB 10.2.7 起 COLUMN_DEFAULT 是 SQL 表达式(字符串带引号,无默认值为 NULL);MySQL 是值本身
    let maria_expr = server.flavor == Flavor::Mariadb && server.version >= (10, 2, 7);
    match &c.default {
        Some(v) if maria_expr && !v.eq_ignore_ascii_case("null") => s += &format!(" DEFAULT {v}"),
        Some(v) if !maria_expr => s += &format!(" DEFAULT {}", literal(v, Dialect::Mysql)),
        _ => {}
    }
    if !c.comment.is_empty() {
        s += &format!(" COMMENT {}", literal(&c.comment, Dialect::Mysql));
    }
    Some(s + ";\n")
}

/// 一列改到目标排序规则的修复语句;拼不出 MODIFY 时给一行注释说明怎么手写。
fn column_fix(ctx: &Ctx, c: &Column, target: &str) -> String {
    let table_sql = ctx.table_sql(&c.schema, &c.table);
    modify_column(&ctx.s.server, c, target, &table_sql).unwrap_or_else(|| {
        comment(&t!(
            "core.charset.fix_manual",
            col = ctx.name(&[&c.schema, &c.table, &c.name]),
            cs = charset_of(target),
            coll = target
        ))
    })
}

/// 两列比较时该统一到的规则:先把 utf8mb3 升到 utf8mb4,优先被引用方(父表)那一侧,父表不是 utf8mb4 时看子表。
fn pair_target(s: &Snapshot, child: &Column, parent: &Column) -> String {
    let (p, c) = (upgrade(s, &parent.collation), upgrade(s, &child.collation));
    if charset_of(&p) == "utf8mb4" || charset_of(&c) != "utf8mb4" {
        p
    } else {
        c
    }
}

/// 两侧规则不同的列对(外键或推断的 JOIN 列)。
fn pair_finding(ctx: &Ctx, child: &Column, parent: &Column, issue: &'static str, why: String) -> Option<Finding> {
    if child.collation == parent.collation {
        return None;
    }
    let target = pair_target(ctx.s, child, parent);
    let mut fix = String::new();
    if issue == "fk_mismatch" {
        fix += &comment(&t!("core.charset.script_fk"));
    }
    fix += &comment(&t!("core.charset.fix_check"));
    for side in [child, parent] {
        if side.collation != target {
            fix += &column_fix(ctx, side, &target);
        }
    }
    let what = if child.charset == parent.charset { t!("core.charset.what_collation") } else { t!("core.charset.what_charset") };
    Some(Finding {
        severity: Severity::High,
        issue,
        object: format!(
            "{} → {}",
            ctx.name(&[&child.schema, &child.table, &child.name]),
            ctx.name(&[&parent.schema, &parent.table, &parent.name])
        ),
        current: format!("{} ↔ {}", child.collation, parent.collation),
        suggested: target,
        note: format!("{} {}", why, t!("core.charset.pair_note", what = what)),
        fix: Some(fix),
    })
}

/// 外键与推断的 JOIN 列。推断两种:`xxx_id` → `xxx(s)`.`id`(复用 ER 图的命名推断);
/// 同名列且对侧是单列主键 / 唯一键(如 orders.user_code ↔ users.user_code)。只看同库、两侧都是字符串列的。
fn pair_findings(ctx: &Ctx) -> Vec<Finding> {
    let mut out = vec![];
    let mut seen: HashSet<(ColKey, ColKey)> = HashSet::new();
    for f in &ctx.s.fks {
        let (Some(c), Some(p)) =
            (ctx.column(&f.schema, &f.table, &f.column), ctx.column(&f.ref_schema, &f.ref_table, &f.ref_column))
        else {
            continue;
        };
        let k = (key(&c.schema, &c.table, &c.name), key(&p.schema, &p.table, &p.name));
        if !seen.insert(k) {
            continue;
        }
        if let Some(x) = pair_finding(ctx, c, p, "fk_mismatch", t!("core.charset.why_fk", name = f.constraint).to_string()) {
            out.push(x);
        }
    }
    let schemas: BTreeSet<&str> = ctx.s.tables.iter().map(|t| t.schema.as_str()).collect();
    for schema in schemas {
        let tables: Vec<String> =
            ctx.s.tables.iter().filter(|t| t.schema == schema).map(|t| t.name.clone()).collect();
        let mut by_table = ColumnsByTable::new();
        for c in ctx.s.columns.iter().filter(|c| c.schema == schema) {
            by_table
                .entry(c.table.clone())
                .or_default()
                .push(ColumnBrief { name: c.name.clone(), data_type: c.column_type.clone() });
        }
        let mut pairs: Vec<(String, String, String, String, String)> = infer_foreign_keys(&tables, &by_table, &[])
            .into_iter()
            .map(|f| {
                let why = t!("core.charset.why_naming", col = f.column, table = f.ref_table).to_string();
                (f.table, f.column, f.ref_table, f.ref_column, why)
            })
            .collect();
        // 单列主键 / 唯一键
        let mut parts: BTreeMap<(&str, &str), Vec<&IndexPart>> = BTreeMap::new();
        for p in ctx.s.indexes.iter().filter(|p| p.schema == schema && p.unique) {
            parts.entry((p.table.as_str(), p.index.as_str())).or_default().push(p);
        }
        for ((table, _), ps) in parts {
            let [p] = ps.as_slice() else { continue };
            if p.sub_part.is_some() {
                continue;
            }
            for other in ctx.s.columns.iter().filter(|c| {
                c.schema == schema && !c.table.eq_ignore_ascii_case(table) && c.name.eq_ignore_ascii_case(&p.column)
            }) {
                let why = t!("core.charset.why_same_name", col = p.column, table = table).to_string();
                pairs.push((other.table.clone(), other.name.clone(), table.to_string(), p.column.clone(), why));
            }
        }
        for (t, c, rt, rc, why) in pairs {
            let (Some(c), Some(p)) = (ctx.column(schema, &t, &c), ctx.column(schema, &rt, &rc)) else {
                continue;
            };
            let k = (key(&c.schema, &c.table, &c.name), key(&p.schema, &p.table, &p.name));
            let rev = (k.1.clone(), k.0.clone());
            if seen.contains(&rev) || !seen.insert(k) {
                continue;
            }
            if let Some(x) = pair_finding(ctx, c, p, "join_mismatch", why) {
                out.push(x);
            }
        }
    }
    out
}

fn mb3_note(ctx: &Ctx, base: String, from: &str, target: &str) -> String {
    match modern_alternative(ctx.s, target) {
        Some(alt) => format!("{base} {}", t!("core.charset.modern", alt = alt, from = from)),
        None => base,
    }
}

/// 体检:按严重程度排好的问题清单 + 转换脚本。
pub fn check(s: &Snapshot) -> Report {
    let ctx = Ctx::new(s);
    let mut findings = pair_findings(&ctx);
    let convs = conversions(&ctx);
    let conv_of: HashMap<(String, String), &Conversion> =
        convs.iter().map(|c| ((c.table.schema.clone(), c.table.name.to_lowercase()), c)).collect();

    // 库
    let mut schema_sqls = vec![];
    for sc in &s.schemas {
        if !is_utf8mb3(&charset_of(&sc.collation)) {
            continue;
        }
        let target = upgrade(s, &sc.collation);
        let sql = schema_sql(&sc.name, &target);
        findings.push(Finding {
            severity: Severity::Medium,
            issue: "utf8mb3_schema",
            object: sc.name.clone(),
            current: sc.collation.clone(),
            suggested: target.clone(),
            note: mb3_note(&ctx, t!("core.charset.note_schema").to_string(), &target, &target),
            fix: Some(format!("{}{sql}", comment(&t!("core.charset.script_schema")))),
        });
        schema_sqls.push(sql);
    }

    // 表、索引、列
    for c in &convs {
        let t = c.table;
        let name = ctx.name(&[&t.schema, &t.name]);
        let fix = conversion_sql(&ctx, c);
        for i in &c.index_issues {
            findings.push(Finding {
                severity: Severity::High,
                issue: "index_too_long",
                object: format!("{name}.{}", i.index),
                current: format!("{} B > {} B", i.bytes, i.limit),
                suggested: if i.row_format_fix { "ROW_FORMAT=DYNAMIC".into() } else { format!("≤ {}", i.max_chars) },
                note: index_text(i),
                fix: i.row_format_fix.then(|| format!("ALTER TABLE {} ROW_FORMAT=DYNAMIC;\n", ctx.table_sql(&t.schema, &t.name))),
            });
        }
        let table_coll = t.collation.clone().unwrap_or_default();
        if is_utf8mb3(&charset_of(&table_coll)) {
            findings.push(Finding {
                severity: Severity::Medium,
                issue: "utf8mb3_table",
                object: name.clone(),
                current: table_coll.clone(),
                suggested: c.target.clone(),
                note: mb3_note(&ctx, t!("core.charset.note_table").to_string(), &c.target, &c.target),
                fix: Some(fix.clone()),
            });
        }
        for col in s.columns.iter().filter(|x| x.schema == t.schema && x.table.eq_ignore_ascii_case(&t.name)) {
            if !is_utf8mb3(&col.charset) {
                continue;
            }
            findings.push(Finding {
                severity: Severity::Medium,
                issue: "utf8mb3_column",
                object: format!("{name}.{}", col.name),
                current: col.collation.clone(),
                suggested: c.target.clone(),
                note: t!("core.charset.note_column", ty = col.column_type).to_string(),
                fix: Some(fix.clone()),
            });
        }
    }

    // 列与表的排序规则不一致(utf8mb3 列已单独列出;表是 utf8mb3 时与转换后的规则比)
    for col in &s.columns {
        if is_utf8mb3(&col.charset) {
            continue;
        }
        let Some(t) = ctx.table(&col.schema, &col.table) else { continue };
        let Some(table_coll) = t.collation.as_deref() else { continue };
        let table_target = upgrade(s, table_coll);
        if col.collation == table_target || conv_of.contains_key(&(t.schema.clone(), t.name.to_lowercase())) {
            // 要转换的表:CONVERT 会把这列一起改掉,脚本注释里已说明
            continue;
        }
        findings.push(Finding {
            severity: Severity::Low,
            issue: "column_collation",
            object: ctx.name(&[&col.schema, &col.table, &col.name]),
            current: col.collation.clone(),
            suggested: table_target.clone(),
            note: t!("core.charset.note_column_collation", coll = table_target).to_string(),
            fix: Some(format!("{}{}", comment(&t!("core.charset.fix_check")), column_fix(&ctx, col, &table_target))),
        });
    }

    // 表与库的排序规则不一致(两边都按升级后的规则比,避免和 utf8mb3 重复)
    let schema_coll: HashMap<&str, &str> = s.schemas.iter().map(|x| (x.name.as_str(), x.collation.as_str())).collect();
    for t in &s.tables {
        let (Some(tc), Some(sc)) = (t.collation.as_deref(), schema_coll.get(t.schema.as_str())) else { continue };
        let (tc, sc) = (upgrade(s, tc), upgrade(s, sc));
        if tc == sc {
            continue;
        }
        findings.push(Finding {
            severity: Severity::Low,
            issue: "table_collation",
            object: ctx.name(&[&t.schema, &t.name]),
            current: tc,
            suggested: sc.clone(),
            note: t!("core.charset.note_table_collation", coll = sc).to_string(),
            fix: None,
        });
    }

    let rank = |i: &str| ISSUES.iter().position(|x| *x == i).unwrap_or(ISSUES.len());
    findings.sort_by(|a, b| (a.severity, rank(a.issue), &a.object).cmp(&(b.severity, rank(b.issue), &b.object)));

    if let Some(n) = s.truncated_columns {
        findings.insert(0, truncated(t!("core.charset.columns_truncated", n = n).to_string()));
    }
    if findings.len() > MAX_FINDINGS {
        let hidden = findings.len() - (MAX_FINDINGS - 1);
        findings.truncate(MAX_FINDINGS - 1);
        findings.push(truncated(t!("core.charset.findings_truncated", n = hidden).to_string()));
    }

    let script = if schema_sqls.is_empty() && convs.is_empty() {
        None
    } else {
        let mut out = comment(&t!("core.charset.script_header", server = s.server.label()));
        if !convs.is_empty() {
            out += &comment(&t!("core.charset.script_copy"));
        }
        for sql in &schema_sqls {
            out += "\n";
            out += sql;
        }
        for c in &convs {
            out += "\n";
            out += &conversion_sql(&ctx, c);
        }
        Some(out)
    };
    Report { findings, script }
}

fn truncated(note: String) -> Finding {
    Finding {
        severity: Severity::Low,
        issue: "truncated",
        object: String::new(),
        current: String::new(),
        suggested: String::new(),
        note,
        fix: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::alter::{self, Algorithm, Risk, TableMeta};

    fn col(schema: &str, table: &str, name: &str, ty: &str, coll: &str) -> Column {
        let max_chars = ty
            .split_once('(')
            .and_then(|(_, r)| r.trim_end_matches(')').parse().ok())
            .or_else(|| ty.contains("text").then_some(65535));
        Column {
            schema: schema.into(),
            table: table.into(),
            name: name.into(),
            column_type: ty.into(),
            max_chars,
            charset: charset_of(coll),
            collation: coll.into(),
            nullable: true,
            ..Default::default()
        }
    }

    fn table(name: &str, coll: &str, row_format: &str) -> Table {
        Table {
            schema: "app".into(),
            name: name.into(),
            engine: Some("InnoDB".into()),
            row_format: Some(row_format.into()),
            collation: Some(coll.into()),
        }
    }

    fn idx(table: &str, index: &str, column: &str, unique: bool) -> IndexPart {
        IndexPart {
            schema: "app".into(),
            table: table.into(),
            index: index.into(),
            column: column.into(),
            unique,
            kind: "BTREE".into(),
            ..Default::default()
        }
    }

    fn mysql8() -> Vec<String> {
        ["utf8mb4_general_ci", "utf8mb4_bin", "utf8mb4_unicode_ci", "utf8mb4_0900_ai_ci", "utf8mb4_unicode_520_ci"]
            .map(String::from)
            .to_vec()
    }

    fn snap(server: Server) -> Snapshot {
        Snapshot {
            server,
            multi_schema: false,
            schemas: vec![Schema { name: "app".into(), collation: "utf8mb4_0900_ai_ci".into() }],
            tables: vec![],
            columns: vec![],
            indexes: vec![],
            fks: vec![],
            utf8mb4_collations: mysql8(),
            utf8mb4_default: Some("utf8mb4_0900_ai_ci".into()),
            large_prefix: None,
            truncated_columns: None,
        }
    }

    #[test]
    fn maps_utf8mb3_collations_to_utf8mb4() {
        let all = mysql8();
        let d = Some("utf8mb4_0900_ai_ci");
        assert_eq!(utf8mb4_collation("utf8mb3_general_ci", &all, d), "utf8mb4_general_ci");
        assert_eq!(utf8mb4_collation("utf8_general_ci", &all, d), "utf8mb4_general_ci");
        assert_eq!(utf8mb4_collation("UTF8_BIN", &all, d), "utf8mb4_bin");
        assert_eq!(utf8mb4_collation("utf8mb3_unicode_520_ci", &all, d), "utf8mb4_unicode_520_ci");
        assert_eq!(utf8mb4_collation("utf8mb3_general_mysql500_ci", &all, d), "utf8mb4_general_ci");
        // 服务器上没有对应名字:用默认规则
        assert_eq!(utf8mb4_collation("utf8mb3_tolower_ci", &all, d), "utf8mb4_0900_ai_ci");
        // 取不到可用列表:直接按名字映射(MariaDB 的 uca1400 / nopad)
        assert_eq!(utf8mb4_collation("utf8mb3_uca1400_ai_ci", &[], None), "utf8mb4_uca1400_ai_ci");
        assert_eq!(utf8mb4_collation("utf8mb3_general_nopad_ci", &[], None), "utf8mb4_general_nopad_ci");
        // 不是 utf8mb3:原样
        assert_eq!(utf8mb4_collation("latin1_swedish_ci", &all, d), "latin1_swedish_ci");
        assert_eq!(charset_of("utf8mb4_0900_ai_ci"), "utf8mb4");
        assert!(is_utf8mb3("utf8") && is_utf8mb3("UTF8MB3") && !is_utf8mb3("utf8mb4"));
    }

    #[test]
    fn suggests_0900_only_on_mysql8_and_uca1400_on_new_mariadb() {
        let s = snap(Server::mysql(8, 0, 36));
        assert_eq!(modern_alternative(&s, "utf8mb4_general_ci"), Some("utf8mb4_0900_ai_ci"));
        assert_eq!(modern_alternative(&s, "utf8mb4_bin"), None);
        let mut s57 = snap(Server::mysql(5, 7, 44));
        s57.utf8mb4_collations.retain(|c| !c.contains("0900"));
        assert_eq!(modern_alternative(&s57, "utf8mb4_general_ci"), None);
        let mut m = snap(Server::mariadb(11, 4, 2));
        m.utf8mb4_collations.push("utf8mb4_uca1400_ai_ci".into());
        assert_eq!(modern_alternative(&m, "utf8mb4_unicode_ci"), Some("utf8mb4_uca1400_ai_ci"));
        let mut m10 = snap(Server::mariadb(10, 6, 0));
        m10.utf8mb4_collations.push("utf8mb4_uca1400_ai_ci".into());
        assert_eq!(modern_alternative(&m10, "utf8mb4_unicode_ci"), None);
    }

    /// 一个典型的老库:utf8mb3 的表(COMPACT 行格式、VARCHAR(255) 上有索引)、库默认 utf8mb3、
    /// 外键两侧不一致、按命名推断的 JOIN 列不一致、一列排序规则与表不同。
    fn legacy() -> Snapshot {
        let mut s = snap(Server::mysql(8, 4, 3));
        s.schemas = vec![Schema { name: "app".into(), collation: "utf8mb3_general_ci".into() }];
        s.tables = vec![
            table("users", "utf8mb4_0900_ai_ci", "Dynamic"),
            table("orders", "utf8mb3_general_ci", "Compact"),
            table("we`ird", "utf8mb3_bin", "Dynamic"),
            table("tags", "utf8mb4_0900_ai_ci", "Dynamic"),
        ];
        let mut code = col("app", "orders", "user_code", "varchar(32)", "utf8mb3_general_ci");
        code.nullable = false;
        s.columns = vec![
            col("app", "users", "code", "varchar(32)", "utf8mb4_0900_ai_ci"),
            col("app", "users", "id", "varchar(36)", "utf8mb4_0900_ai_ci"),
            col("app", "users", "nick", "varchar(40)", "utf8mb4_bin"),
            code,
            col("app", "orders", "email", "varchar(255)", "utf8mb3_general_ci"),
            col("app", "orders", "note", "varchar(20)", "latin1_swedish_ci"),
            col("app", "we`ird", "x", "text", "utf8mb3_bin"),
            col("app", "tags", "user_id", "varchar(36)", "utf8mb4_general_ci"),
        ];
        s.indexes = vec![
            idx("users", "PRIMARY", "id", true),
            idx("users", "uk_code", "code", true),
            idx("orders", "idx_email", "email", false),
        ];
        s.fks = vec![Fk {
            constraint: "fk_orders_user".into(),
            schema: "app".into(),
            table: "orders".into(),
            column: "user_code".into(),
            ref_schema: "app".into(),
            ref_table: "users".into(),
            ref_column: "code".into(),
        }];
        s
    }

    #[test]
    fn finds_every_kind_of_problem_sorted_by_severity() {
        let r = check(&legacy());
        let kinds: Vec<(&str, &str)> = r.findings.iter().map(|f| (f.severity.code(), f.issue)).collect();
        // 严重程度不降
        let sev: Vec<Severity> = r.findings.iter().map(|f| f.severity).collect();
        assert!(sev.windows(2).all(|w| w[0] <= w[1]), "{kinds:?}");
        let find = |issue: &str, object: &str| {
            r.findings
                .iter()
                .find(|f| f.issue == issue && f.object == object)
                .unwrap_or_else(|| panic!("缺 {issue} {object}: {kinds:?}"))
        };
        let fk = find("fk_mismatch", "orders.user_code → users.code");
        assert_eq!(fk.current, "utf8mb3_general_ci ↔ utf8mb4_0900_ai_ci");
        assert_eq!(fk.suggested, "utf8mb4_0900_ai_ci");
        let fix = fk.fix.as_deref().unwrap();
        assert!(fix.contains(
            "ALTER TABLE `app`.`orders` MODIFY `user_code` varchar(32) CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci NOT NULL;"
        ), "{fix}");
        // 推断:tags.user_id → users.id(命名),只有排序规则不同
        let j = find("join_mismatch", "tags.user_id → users.id");
        assert_eq!(j.suggested, "utf8mb4_0900_ai_ci");
        // 外键已覆盖的列对不再按「同名唯一键」重复
        assert!(!r.findings.iter().any(|f| f.issue == "join_mismatch" && f.object.starts_with("orders.user_code")));
        // COMPACT 行格式上 VARCHAR(255) 的索引:1020 字节 > 767
        let ix = find("index_too_long", "orders.idx_email");
        assert_eq!(ix.current, "1020 B > 767 B");
        assert_eq!(ix.suggested, "ROW_FORMAT=DYNAMIC");
        assert_eq!(find("utf8mb3_schema", "app").suggested, "utf8mb4_general_ci");
        assert_eq!(find("utf8mb3_table", "orders").suggested, "utf8mb4_general_ci");
        assert_eq!(find("utf8mb3_table", "we`ird").suggested, "utf8mb4_bin");
        assert_eq!(find("utf8mb3_column", "orders.email").current, "utf8mb3_general_ci");
        // users.nick 是 _bin,和表不同:低优先级提示;要转换的表(orders.note)不重复报
        assert_eq!(find("column_collation", "users.nick").suggested, "utf8mb4_0900_ai_ci");
        assert!(!r.findings.iter().any(|f| f.issue == "column_collation" && f.object == "orders.note"));
        // 表与库(按升级后的规则比):users 是 0900,库会变成 general_ci
        assert_eq!(find("table_collation", "users").suggested, "utf8mb4_general_ci");
        assert_eq!(r.findings[0].severity, Severity::High);
        assert_eq!(r.findings.last().unwrap().severity, Severity::Low);
    }

    #[test]
    fn script_escapes_identifiers_and_warns() {
        let r = check(&legacy());
        let script = r.script.unwrap();
        assert!(script.contains("ALTER DATABASE `app` CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci;"), "{script}");
        assert!(script.contains("ALTER TABLE `app`.`orders` CONVERT TO CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci;"));
        assert!(script.contains("ALTER TABLE `app`.`we``ird` CONVERT TO CHARACTER SET utf8mb4 COLLATE utf8mb4_bin;"));
        // 索引超长的警告在转换语句前,并给出改行格式的写法(注释掉)
        let warn = script.find("idx_email").unwrap();
        let conv = script.find("ALTER TABLE `app`.`orders` CONVERT").unwrap();
        assert!(warn < conv);
        assert!(script.contains("-- ALTER TABLE `app`.`orders` ROW_FORMAT=DYNAMIC;"));
        // 会被一起转换的非 utf8mb3 列、外键说明
        assert!(script.contains("note (latin1_swedish_ci)"));
        // 没有 utf8mb3 的表不在脚本里
        assert!(!script.contains("`users` CONVERT") && !script.contains("`tags` CONVERT"));
        // 所有非注释行都是完整语句
        for line in script.lines().filter(|l| !l.trim().is_empty() && !l.starts_with("--")) {
            assert!(line.ends_with(';'), "{line}");
        }
    }

    #[test]
    fn generated_convert_goes_through_alter_risk_as_copy() {
        let r = check(&legacy());
        let script = r.script.unwrap();
        let stmt = script.lines().find(|l| l.contains("`orders` CONVERT")).unwrap();
        let p = alter::parse(stmt).expect("能解析");
        assert_eq!(p.schema.as_deref(), Some("app"));
        assert_eq!(p.table, "orders");
        let meta = TableMeta { engine: Some("InnoDB".into()), ..Default::default() };
        let a = alter::assess(&p, &Server::mysql(8, 4, 3), Some(&meta));
        assert_eq!(a.algorithm, Some(Algorithm::Copy));
        assert_eq!(a.risk, Risk::High);
        // 带注释的整段(编辑器里一条语句前面有注释)也能识别
        let with_comment = format!("-- {}\n{stmt}", "note");
        assert!(alter::parse(&with_comment).is_some());
    }

    #[test]
    fn index_limits_by_engine_and_row_format() {
        let mut s = snap(Server::mysql(8, 0, 36));
        s.tables = vec![table("a", "utf8mb3_general_ci", "Dynamic"), table("b", "utf8mb3_general_ci", "Redundant")];
        s.columns = vec![
            col("app", "a", "v", "varchar(255)", "utf8mb3_general_ci"),
            col("app", "a", "w", "varchar(600)", "utf8mb3_general_ci"),
            col("app", "a", "x", "varchar(500)", "utf8mb3_general_ci"),
            col("app", "b", "v", "varchar(255)", "utf8mb3_general_ci"),
            col("app", "b", "p", "varchar(255)", "utf8mb3_general_ci"),
        ];
        let mut prefix = idx("b", "idx_p", "p", false);
        prefix.sub_part = Some(100);
        s.indexes = vec![
            idx("a", "idx_v", "v", false),
            // 组合索引:单列都不超 3072,合计 (600 + 500) * 4 = 4400 > 3072
            idx("a", "idx_wx", "w", false),
            idx("a", "idx_wx", "x", false),
            idx("b", "idx_v", "v", false),
            prefix,
        ];
        let ctx = Ctx::new(&s);
        let a = index_issues(&ctx, &s.tables[0]);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!((a[0].index.as_str(), a[0].bytes, a[0].limit, a[0].row_format_fix), ("idx_wx", 4400, 3072, false));
        let b = index_issues(&ctx, &s.tables[1]);
        assert_eq!(b.len(), 1, "{b:?}");
        assert_eq!((b[0].index.as_str(), b[0].bytes, b[0].max_chars), ("idx_v", 1020, 191));
        // 5.7 关了 innodb_large_prefix:DYNAMIC 也按 767 算,改行格式解决不了
        s.large_prefix = Some(false);
        let ctx = Ctx::new(&s);
        let a = index_issues(&ctx, &s.tables[0]);
        assert!(a.iter().any(|i| i.index == "idx_v" && i.limit == 767 && !i.row_format_fix), "{a:?}");
    }

    #[test]
    fn modify_column_rebuilds_definition_and_handles_defaults() {
        let my = Server::mysql(8, 0, 36);
        let mut c = col("app", "t", "na`me", "varchar(20)", "utf8mb3_general_ci");
        c.nullable = false;
        c.default = Some("it's".into());
        c.comment = "名字".into();
        assert_eq!(
            modify_column(&my, &c, "utf8mb4_general_ci", "`app`.`t`").unwrap(),
            "ALTER TABLE `app`.`t` MODIFY `na``me` varchar(20) CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci NOT NULL DEFAULT 'it''s' COMMENT '名字';\n"
        );
        // MariaDB 10.2.7+:默认值已是表达式,NULL 表示没有默认值
        let maria = Server::mariadb(10, 11, 6);
        c.default = Some("'x'".into());
        assert!(modify_column(&maria, &c, "utf8mb4_unicode_ci", "`t`").unwrap().contains(" DEFAULT 'x' "));
        c.default = Some("NULL".into());
        assert!(!modify_column(&maria, &c, "utf8mb4_unicode_ci", "`t`").unwrap().contains("DEFAULT"));
        // 生成列、表达式默认值等拼不全定义
        c.extra = "VIRTUAL GENERATED".into();
        assert!(modify_column(&my, &c, "utf8mb4_general_ci", "`t`").is_none());
    }

    #[test]
    fn clean_schema_has_no_findings_or_script() {
        let mut s = snap(Server::mysql(8, 4, 3));
        s.tables = vec![table("users", "utf8mb4_0900_ai_ci", "Dynamic")];
        s.columns = vec![col("app", "users", "name", "varchar(255)", "utf8mb4_0900_ai_ci")];
        let r = check(&s);
        assert!(r.findings.is_empty(), "{:?}", r.findings);
        assert!(r.script.is_none());
    }

    #[test]
    fn mariadb_and_old_mysql_names_and_multi_schema_objects() {
        let mut s = snap(Server::mariadb(10, 11, 6));
        s.multi_schema = true;
        s.utf8mb4_collations = vec!["utf8mb4_general_ci".into(), "utf8mb4_unicode_ci".into(), "utf8mb4_bin".into()];
        s.utf8mb4_default = Some("utf8mb4_general_ci".into());
        s.schemas = vec![Schema { name: "shop".into(), collation: "utf8mb4_general_ci".into() }];
        s.tables = vec![Table { schema: "shop".into(), ..table("t", "utf8mb3_unicode_ci", "Dynamic") }];
        s.columns = vec![col("shop", "t", "v", "varchar(10)", "utf8mb3_unicode_ci")];
        let r = check(&s);
        let t = r.findings.iter().find(|f| f.issue == "utf8mb3_table").unwrap();
        assert_eq!(t.object, "shop.t");
        assert_eq!(t.suggested, "utf8mb4_unicode_ci");
        // 10.11 没有 uca1400:不提示新规则
        assert!(!t.note.contains("uca1400"));
        let c = r.findings.iter().find(|f| f.issue == "utf8mb3_column").unwrap();
        assert_eq!(c.object, "shop.t.v");
        // 5.7 叫 utf8_general_ci
        let mut s = snap(Server::mysql(5, 7, 44));
        s.utf8mb4_collations = vec!["utf8mb4_general_ci".into(), "utf8mb4_bin".into()];
        s.tables = vec![table("t", "utf8_general_ci", "Dynamic")];
        let r = check(&s);
        assert!(r.script.unwrap().contains("CONVERT TO CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci;"));
    }

    #[test]
    fn caps_findings() {
        let mut s = snap(Server::mysql(8, 4, 3));
        s.tables = vec![table("t", "utf8mb4_0900_ai_ci", "Dynamic")];
        s.columns = (0..MAX_FINDINGS + 10).map(|i| col("app", "t", &format!("c{i}"), "varchar(5)", "utf8mb3_general_ci")).collect();
        s.truncated_columns = Some(20000);
        let r = check(&s);
        assert_eq!(r.findings.len(), MAX_FINDINGS);
        assert_eq!(r.findings[0].issue, "truncated");
        assert_eq!(r.findings.last().unwrap().issue, "truncated");
        // 脚本里只有一条表转换
        assert_eq!(r.script.unwrap().matches("CONVERT TO").count(), 1);
    }
}
