//! SQLite 改表结构。
//!
//! SQLite 的 ALTER TABLE 只能改表名、改列名、加列、删列(3.35+);改类型 / 非空 / 默认值 / 主键 / 外键
//! 都得按官方文档「Making Other Kinds Of Table Schema Changes」的流程重建表:
//! 关外键 → 开事务 → 建新表(临时名)→ 复制数据 → 删旧表 → 改名 → 重建索引 / 触发器 / 受影响的视图 →
//! 外键检查 → 提交 → 恢复外键。
//!
//! 分三层:
//! - `snapshot`:从库里读出表的现状(原始建表语句、列、外键、相关对象、连接上的两个 PRAGMA);
//! - `generate`:纯函数,现状 + 用户要的新结构 → 脚本;能用简单 ALTER 完成的不走重建;
//! - `apply`:在一个事务里执行脚本,失败整体回滚,外键检查不过也回滚并列出违规行。
//!
//! 结构对比(`schema_diff`)里 SQLite 需要重建的表也用 [`generate`] 生成脚本。

use crate::error::{AppError, AppResult, ErrorKind};
use crate::sql::build::ident as quote_ident;
use crate::sql::Dialect;
use rusqlite::Connection;
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

fn ident(s: &str) -> String {
    quote_ident(s, Dialect::Sqlite)
}

fn lc(s: &str) -> String {
    s.to_lowercase()
}

// ------------------------------------------------------------ 数据结构

/// 一列(现状或目标)。`orig` 是它在原表里的列名,新加的列为 None——改名、删列、调顺序都靠它对应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSpec {
    #[serde(default)]
    pub orig: Option<String>,
    pub name: String,
    #[serde(default)]
    pub data_type: String,
    #[serde(default)]
    pub not_null: bool,
    /// 默认值表达式,原样写进 DEFAULT(字符串要自带引号)
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub pk: bool,
    /// 生成列(只读,不参与数据复制)
    #[serde(default)]
    pub generated: bool,
}

/// 外键。目标结构里的列名用改名之后的新名字。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FkSpec {
    pub columns: Vec<String>,
    pub ref_table: String,
    /// 空表示引用对方的主键
    #[serde(default)]
    pub ref_columns: Vec<String>,
    #[serde(default = "no_action")]
    pub on_delete: String,
    #[serde(default = "no_action")]
    pub on_update: String,
}

fn no_action() -> String {
    "NO ACTION".into()
}

/// 用户要的新结构:列的顺序即新表的列顺序;原表里有、这里没有的列就是要删的。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlterPlan {
    pub table: String,
    pub new_name: String,
    pub columns: Vec<ColumnSpec>,
    #[serde(default)]
    pub fks: Vec<FkSpec>,
}

/// sqlite_master 里的索引 / 触发器 / 视图(原始 SQL)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaObject {
    /// index / trigger / view
    pub kind: String,
    pub name: String,
    /// 所属表(视图为自身名)
    pub table: String,
    pub sql: String,
}

/// 别的表经外键引用了本表的哪些列(空 = 引用主键)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefBy {
    pub table: String,
    pub columns: Vec<String>,
}

/// 表的现状。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableSnapshot {
    pub table: String,
    pub create_sql: String,
    pub columns: Vec<ColumnSpec>,
    pub fks: Vec<FkSpec>,
    /// 库里全部索引 / 触发器 / 视图(按建立顺序)
    pub objects: Vec<SchemaObject>,
    /// 库里全部对象名(挑临时表名、检查改名冲突)
    pub names: Vec<String>,
    pub referenced_by: Vec<RefBy>,
    pub foreign_keys_on: bool,
    pub legacy_alter_table: bool,
}

/// noop:没有变化;simple:用 ALTER TABLE 直接改;rebuild:重建表。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AlterMode {
    Noop,
    Simple,
    Rebuild,
}

/// 生成的脚本。`sql` 给人看(预览、复制);执行用结构化的 `statements` / `fk_check`,
/// 不再从文本里切语句——触发器体里本来就有分号。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlterScript {
    pub mode: AlterMode,
    pub sql: String,
    /// BEGIN 与 COMMIT 之间依次执行的语句(每条都是单条语句)
    pub statements: Vec<String>,
    /// 提交前逐个 `PRAGMA foreign_key_check` 的表
    pub fk_check: Vec<String>,
    /// 执行前要关掉外键约束(重建表)
    pub foreign_keys_off: bool,
    /// 提示(数据会丢、需人工检查的触发器 / 视图、新 NOT NULL 列可能复制失败等)
    pub warnings: Vec<String>,
}

/// 外键检查不过的一行(对应 `PRAGMA foreign_key_check` 的一行)。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FkViolation {
    pub table: String,
    pub rowid: Option<i64>,
    pub parent: String,
    pub fkid: i64,
}

/// 执行结果:`applied` 为 false 时整体已回滚,`violations` 列出外键检查不过的行。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyOutcome {
    pub applied: bool,
    pub violations: Vec<FkViolation>,
}

// ------------------------------------------------------------ 词法

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Space,
    Comment,
    Word,
    Quoted,
    Str,
    Num,
    Punct,
}

#[derive(Debug, Clone)]
struct Tok<'a> {
    kind: Kind,
    text: &'a str,
}

impl Tok<'_> {
    fn sig(&self) -> bool {
        !matches!(self.kind, Kind::Space | Kind::Comment)
    }
    fn is(&self, p: &str) -> bool {
        self.kind == Kind::Punct && self.text == p
    }
    fn upper(&self) -> String {
        if self.kind == Kind::Word { self.text.to_ascii_uppercase() } else { String::new() }
    }
    /// 标识符(裸词或引号标识符)的名字。
    fn name(&self) -> Option<String> {
        match self.kind {
            Kind::Word => Some(self.text.to_string()),
            Kind::Quoted => {
                let t = self.text;
                let inner = &t[1..t.len() - 1];
                Some(match &t[..1] {
                    "\"" => inner.replace("\"\"", "\""),
                    "`" => inner.replace("``", "`"),
                    _ => inner.to_string(),
                })
            }
            _ => None,
        }
    }
}

/// 按 SQLite 的词法切 token;字符串 / 引号标识符 / 块注释没闭合时返回 None。
fn lex(s: &str) -> Option<Vec<Tok<'_>>> {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    // 找到成对的结束符(支持重复转义,如 '' 与 "")
    let quoted = |start: usize, close: u8, doubled: bool| -> Option<usize> {
        let mut j = start + 1;
        while j < b.len() {
            if b[j] == close {
                if doubled && b.get(j + 1) == Some(&close) {
                    j += 2;
                    continue;
                }
                return Some(j + 1);
            }
            j += 1;
        }
        None
    };
    while i < b.len() {
        let c = b[i];
        let (kind, end) = if c.is_ascii_whitespace() {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            (Kind::Space, j)
        } else if c == b'-' && b.get(i + 1) == Some(&b'-') {
            let j = s[i..].find('\n').map(|n| i + n).unwrap_or(b.len());
            (Kind::Comment, j)
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            (Kind::Comment, s[i + 2..].find("*/").map(|n| i + 2 + n + 2)?)
        } else if c == b'\'' {
            (Kind::Str, quoted(i, b'\'', true)?)
        } else if c == b'"' {
            (Kind::Quoted, quoted(i, b'"', true)?)
        } else if c == b'`' {
            (Kind::Quoted, quoted(i, b'`', true)?)
        } else if c == b'[' {
            (Kind::Quoted, quoted(i, b']', false)?)
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'.' || b[j] == b'_') {
                j += 1;
            }
            (Kind::Num, j)
        } else if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'$' || b[j] >= 0x80) {
                j += 1;
            }
            (Kind::Word, j)
        } else {
            // 单个字符(按 UTF-8 边界)
            let n = s[i..].chars().next().map(char::len_utf8).unwrap_or(1);
            (Kind::Punct, i + n)
        };
        out.push(Tok { kind, text: &s[i..end] });
        i = end;
    }
    Some(out)
}

/// token 拼回文本:注释换成空格(拼成一行时 `--` 注释会吞掉后文),空白折叠成一个空格;
/// `rename` 里的标识符(按小写匹配)换成新名字。
fn render(toks: &[Tok], rename: &HashMap<String, String>) -> String {
    let mut s = String::new();
    for t in toks {
        if t.sig() {
            match t.name().and_then(|n| rename.get(&lc(&n))) {
                Some(new) => s.push_str(&ident(new)),
                None => s.push_str(t.text),
            }
        } else if !s.ends_with(' ') {
            s.push(' ');
        }
    }
    s.trim().to_string()
}

/// 文本里是否把 `names`(小写)之一当标识符用(字符串、注释里的不算)。
fn mentions(sql: &str, names: &HashSet<String>) -> bool {
    if names.is_empty() {
        return false;
    }
    match lex(sql) {
        Some(toks) => toks.iter().any(|t| t.name().is_some_and(|n| names.contains(&lc(&n)))),
        // 拆不开的按「提到了」处理,宁可多提示
        None => true,
    }
}

/// 改名:只改标识符 token,字符串与注释不动;对象自己的名字(CREATE INDEX / TRIGGER / VIEW 后的那个)不改。
fn rename_in_object(sql: &str, rename: &HashMap<String, String>) -> String {
    let Some(toks) = lex(sql) else { return sql.to_string() };
    // 找到对象名的位置:第一个 INDEX / TRIGGER / VIEW 关键字之后,跳过 IF NOT EXISTS 与 schema 前缀
    let sig: Vec<usize> = (0..toks.len()).filter(|&i| toks[i].sig()).collect();
    let mut skip = HashSet::new();
    if let Some(p) = sig.iter().position(|&i| matches!(toks[i].upper().as_str(), "INDEX" | "TRIGGER" | "VIEW")) {
        let mut q = p + 1;
        if sig.get(q).is_some_and(|&i| toks[i].upper() == "IF") {
            q += 3;
        }
        if let Some(&i) = sig.get(q) {
            skip.insert(i);
            if sig.get(q + 1).is_some_and(|&j| toks[j].is(".")) {
                if let Some(&k) = sig.get(q + 2) {
                    skip.insert(k);
                }
            }
        }
    }
    let mut s = String::new();
    for (i, t) in toks.iter().enumerate() {
        match t.name().filter(|_| !skip.contains(&i)).and_then(|n| rename.get(&lc(&n))) {
            Some(new) => s.push_str(&ident(new)),
            None => s.push_str(t.text),
        }
    }
    s
}

// ------------------------------------------------------------ 解析 CREATE TABLE

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seg {
    Primary,
    NotNull,
    Null,
    Default,
    Unique,
    Check,
    Collate,
    References,
    Generated,
    /// CONSTRAINT 名字之后还没遇到约束关键字
    Named,
}

#[derive(Debug, Clone)]
struct ParsedCol {
    name: String,
    type_text: String,
    segs: Vec<(Seg, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsKind {
    Primary,
    Foreign,
    Other,
}

#[derive(Debug, Clone, Default)]
struct ParsedTable {
    cols: Vec<ParsedCol>,
    constraints: Vec<(ConsKind, String)>,
    /// `)` 之后的表选项(WITHOUT ROWID、STRICT)
    options: String,
}

fn seg_kind(w: &str) -> Option<Seg> {
    Some(match w {
        "PRIMARY" => Seg::Primary,
        "NOT" => Seg::NotNull,
        "NULL" => Seg::Null,
        "DEFAULT" => Seg::Default,
        "UNIQUE" => Seg::Unique,
        "CHECK" => Seg::Check,
        "COLLATE" => Seg::Collate,
        "REFERENCES" => Seg::References,
        "GENERATED" | "AS" => Seg::Generated,
        "CONSTRAINT" => Seg::Named,
        _ => return None,
    })
}

/// 一个列定义(名字之后的部分):类型 + 按约束关键字切开的各段。
fn parse_col(src: &str, toks: &[Tok]) -> Option<ParsedCol> {
    let sig: Vec<&Tok> = toks.iter().filter(|t| t.sig()).collect();
    let name = sig.first()?.name()?;
    let rest = &sig[1..];
    let mut type_toks: Vec<&Tok> = vec![];
    let mut segs: Vec<(Seg, Vec<&Tok>)> = vec![];
    let mut depth = 0i32;
    // DEFAULT 之后还没吃到值
    let mut default_pending = false;
    for (i, t) in rest.iter().enumerate() {
        let w = t.upper();
        let prev = if i > 0 { rest[i - 1].upper() } else { String::new() };
        let next = rest.get(i + 1).map(|x| x.upper()).unwrap_or_default();
        if depth == 0 && default_pending {
            segs.last_mut()?.1.push(t);
            if t.is("(") {
                depth += 1;
            } else if !(t.is("+") || t.is("-")) {
                default_pending = false;
            }
            continue;
        }
        let start = if depth == 0 { seg_kind(&w) } else { None };
        let start = match (start, segs.last().map(|s| s.0)) {
            (None, _) => None,
            (Some(_), Some(Seg::Named)) if segs.last().is_some_and(|s| s.1.len() == 1) => None, // 约束名
            (Some(Seg::Null), _) if prev == "NOT" || prev == "SET" => None,
            (Some(Seg::NotNull), _) if next == "DEFERRABLE" => None,
            (Some(Seg::Default), _) if prev == "SET" => None,
            (Some(Seg::Generated), Some(Seg::Generated)) => None,
            (Some(k), Some(Seg::Named)) => {
                // CONSTRAINT 名字 + 约束:同一段,定下类型
                let last = segs.last_mut()?;
                last.0 = k;
                last.1.push(t);
                default_pending = k == Seg::Default;
                continue;
            }
            (s, _) => s,
        };
        if t.is("(") {
            depth += 1;
        } else if t.is(")") {
            depth -= 1;
            // DEFAULT (表达式) 的右括号:默认值到此结束
            if depth == 0 && default_pending {
                default_pending = false;
                segs.last_mut()?.1.push(t);
                continue;
            }
        }
        match start {
            Some(k) => {
                segs.push((k, vec![t]));
                default_pending = k == Seg::Default;
            }
            None => match segs.last_mut() {
                Some(s) => s.1.push(t),
                None => type_toks.push(t),
            },
        }
    }
    // 取回原文里这一段(保留原来的空白),再去掉注释、折叠空白
    let off = |t: &Tok| t.text.as_ptr() as usize - src.as_ptr() as usize;
    let join = |v: &[&Tok]| match (v.first(), v.last()) {
        (Some(a), Some(b)) => rename_text(&src[off(a)..off(b) + b.text.len()], &HashMap::new()),
        _ => String::new(),
    };
    Some(ParsedCol {
        name,
        type_text: join(&type_toks),
        segs: segs.iter().map(|(k, v)| (*k, join(v))).collect(),
    })
}

/// 拆开原始建表语句。`CREATE TABLE … AS SELECT` 之类拆不开的返回 None。
fn parse_create(sql: &str) -> Option<ParsedTable> {
    let toks = lex(sql)?;
    let open = toks.iter().position(|t| t.is("("))?;
    if toks[..open].iter().any(|t| t.upper() == "AS") {
        return None;
    }
    let mut depth = 0;
    let mut close = None;
    for (i, t) in toks.iter().enumerate().skip(open) {
        if t.is("(") {
            depth += 1;
        } else if t.is(")") {
            depth -= 1;
            if depth == 0 {
                close = Some(i);
                break;
            }
        }
    }
    let close = close?;
    let mut items: Vec<&[Tok]> = vec![];
    let (mut start, mut depth) = (open + 1, 0);
    for i in open + 1..close {
        let t = &toks[i];
        if t.is("(") {
            depth += 1;
        } else if t.is(")") {
            depth -= 1;
        } else if t.is(",") && depth == 0 {
            items.push(&toks[start..i]);
            start = i + 1;
        }
    }
    items.push(&toks[start..close]);
    let mut out = ParsedTable { options: render(&toks[close + 1..], &HashMap::new()), ..Default::default() };
    for item in items {
        let first = item.iter().find(|t| t.sig())?;
        let w = first.upper();
        if ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"].contains(&w.as_str()) {
            let sig: Vec<String> = item.iter().filter(|t| t.sig()).map(|t| t.upper()).collect();
            let kind = if sig.iter().take(3).any(|w| w == "PRIMARY") {
                ConsKind::Primary
            } else if sig.iter().take(3).any(|w| w == "FOREIGN") {
                ConsKind::Foreign
            } else {
                ConsKind::Other
            };
            out.constraints.push((kind, render(item, &HashMap::new())));
        } else {
            out.cols.push(parse_col(sql, item)?);
        }
    }
    Some(out)
}

// ------------------------------------------------------------ 校验

fn invalid(msg: impl Into<String>) -> AppError {
    AppError::new(ErrorKind::Query, msg.into())
}

/// 默认值必须是单个表达式:能完整分词、括号配对、不含分号与注释、不换行——
/// 原样写进 DEFAULT,不能让它带出第二条语句(同 schema_diff 的注入检查思路)。
fn check_default(col: &str, v: &str) -> AppResult<()> {
    let bad = || invalid(t!("core.alter.bad_default", col = col, value = v));
    if v.trim().is_empty() || v.contains(['\n', '\r']) || v.chars().any(char::is_control) {
        return Err(bad());
    }
    let toks = lex(v).ok_or_else(bad)?;
    let mut depth = 0i32;
    for t in &toks {
        if t.kind == Kind::Comment || t.is(";") {
            return Err(bad());
        }
        if t.is("(") {
            depth += 1;
        } else if t.is(")") {
            depth -= 1;
            if depth < 0 {
                return Err(bad());
            }
        }
    }
    if depth != 0 {
        return Err(bad());
    }
    Ok(())
}

/// 类型名只允许词、数字、括号、逗号与正负号,如 `VARCHAR(20)`、`DECIMAL(10, 2)`、`UNSIGNED BIG INT`。
fn check_type(col: &str, v: &str) -> AppResult<()> {
    let bad = || invalid(t!("core.alter.bad_type", col = col, value = v));
    let toks = lex(v).ok_or_else(bad)?;
    let ok = toks.iter().all(|t| match t.kind {
        Kind::Space | Kind::Word | Kind::Num => !t.text.contains(['\n', '\r']),
        Kind::Punct => ["(", ")", ",", "+", "-"].contains(&t.text),
        _ => false,
    });
    if !ok {
        return Err(bad());
    }
    Ok(())
}

const FK_ACTIONS: [&str; 5] = ["NO ACTION", "CASCADE", "SET NULL", "SET DEFAULT", "RESTRICT"];

fn fk_action(a: &str) -> AppResult<String> {
    let up = a.trim().to_ascii_uppercase();
    let up = if up.is_empty() { "NO ACTION".to_string() } else { up };
    if FK_ACTIONS.contains(&up.as_str()) {
        Ok(up)
    } else {
        Err(invalid(t!("core.alter.bad_fk_action", action = a)))
    }
}

// ------------------------------------------------------------ 生成

/// 外键比较:列名小写、动作规范化;`rename` 把原列名映射到新列名(自引用的被引列也要跟着改)。
fn fk_key(f: &FkSpec, table: &str, new_name: &str, rename: &HashMap<String, String>) -> String {
    let map = |c: &String| rename.get(&lc(c)).map(|s| lc(s)).unwrap_or_else(|| lc(c));
    let self_ref = lc(&f.ref_table) == lc(table) || lc(&f.ref_table) == lc(new_name);
    let rt = if self_ref { "<self>".to_string() } else { lc(&f.ref_table) };
    let rc: Vec<String> = if self_ref { f.ref_columns.iter().map(map).collect() } else { f.ref_columns.iter().map(|c| lc(c)).collect() };
    let up = |a: &str| {
        let u = a.trim().to_ascii_uppercase();
        if u.is_empty() { "NO ACTION".into() } else { u }
    };
    format!(
        "{:?}|{rt}|{rc:?}|{}|{}",
        f.columns.iter().map(map).collect::<Vec<_>>(),
        up(&f.on_delete),
        up(&f.on_update)
    )
}

fn fk_sql(f: &FkSpec, table: &str, new_name: &str) -> AppResult<String> {
    let cols = |v: &[String]| v.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ");
    // 自引用的外键写原表名:重建时新表最后才改名为原名,之后再改成新名时会被 SQLite 一并改写
    let rt = if lc(&f.ref_table) == lc(new_name) { table } else { f.ref_table.as_str() };
    let mut s = format!("FOREIGN KEY ({}) REFERENCES {}", cols(&f.columns), ident(rt));
    if !f.ref_columns.is_empty() {
        s.push_str(&format!(" ({})", cols(&f.ref_columns)));
    }
    for (what, act) in [("DELETE", &f.on_delete), ("UPDATE", &f.on_update)] {
        let a = fk_action(act)?;
        if a != "NO ACTION" {
            s.push_str(&format!(" ON {what} {a}"));
        }
    }
    Ok(s)
}

fn same_type(a: &str, b: &str) -> bool {
    lc(a.trim()) == lc(b.trim())
}

fn same_default(a: &Option<String>, b: &Option<String>) -> bool {
    a.as_deref().map(str::trim) == b.as_deref().map(str::trim)
}

/// 不能用 ADD COLUMN 加的列:主键、UNIQUE、NOT NULL 却没有非 NULL 默认值、
/// 默认值是 CURRENT_* 或括号表达式(SQLite 文档列出的限制)。
fn addable(c: &ColumnSpec) -> bool {
    if c.pk || c.generated {
        return false;
    }
    match c.default.as_deref().map(str::trim) {
        None => !c.not_null,
        Some(d) => {
            let u = d.to_ascii_uppercase();
            !(u.starts_with("CURRENT_") || d.starts_with('(') || (c.not_null && u == "NULL"))
        }
    }
}

struct Ctx<'a> {
    snap: &'a TableSnapshot,
    plan: &'a AlterPlan,
    /// 原列名(小写)→ 新列名(只含改了名的)
    rename: HashMap<String, String>,
    /// 删掉的原列名(小写)
    dropped: HashSet<String>,
    /// 原列名(小写)→ 原列
    old: HashMap<String, &'a ColumnSpec>,
}

impl Ctx<'_> {
    fn old_pk(&self) -> Vec<String> {
        self.snap.columns.iter().filter(|c| c.pk).map(|c| lc(&c.name)).collect()
    }
    fn new_pk(&self) -> Vec<String> {
        self.plan.columns.iter().filter(|c| c.pk).map(|c| c.orig.as_deref().map(lc).unwrap_or_else(|| "\u{0}new".into())).collect()
    }
    fn pk_same(&self) -> bool {
        let mut a = self.old_pk();
        let mut b = self.new_pk();
        a.sort();
        b.sort();
        a == b
    }
    fn fks_same(&self) -> bool {
        let t = &self.snap.table;
        let n = &self.plan.new_name;
        let mut a: Vec<String> = self.snap.fks.iter().map(|f| fk_key(f, t, n, &self.rename)).collect();
        let mut b: Vec<String> = self.plan.fks.iter().map(|f| fk_key(f, t, n, &HashMap::new())).collect();
        a.sort();
        b.sort();
        a == b
    }
    fn col_same(&self, c: &ColumnSpec) -> bool {
        match c.orig.as_ref().and_then(|o| self.old.get(&lc(o))) {
            Some(o) => same_type(&o.data_type, &c.data_type) && o.not_null == c.not_null && same_default(&o.default, &c.default),
            None => false,
        }
    }
    /// 原表里除了本列定义之外的所有结构文本(其他列的约束、表约束、本表相关的对象)。
    fn others_mention(&self, parsed: Option<&ParsedTable>, col: &str) -> bool {
        let names: HashSet<String> = [lc(col)].into();
        if let Some(p) = parsed {
            let t = &self.snap.table;
            for pc in p.cols.iter().filter(|pc| lc(&pc.name) != lc(col)) {
                if pc.segs.iter().any(|(k, s)| if *k == Seg::References { fk_mentions(s, t, &names) } else { mentions(s, &names) }) {
                    return true;
                }
            }
            if p.constraints.iter().any(|(k, s)| if *k == ConsKind::Foreign { fk_mentions(s, t, &names) } else { mentions(s, &names) }) {
                return true;
            }
        }
        let t = lc(&self.snap.table);
        let tn: HashSet<String> = [t.clone()].into();
        self.snap.objects.iter().any(|o| (lc(&o.table) == t || mentions(&o.sql, &tn)) && mentions(&o.sql, &names))
            || self.snap.referenced_by.iter().any(|r| r.columns.iter().any(|c| lc(c) == lc(col)))
    }
}

/// 校验目标结构,得出改名 / 删列映射。
fn context<'a>(snap: &'a TableSnapshot, plan: &'a AlterPlan) -> AppResult<Ctx<'a>> {
    if plan.new_name.trim().is_empty() {
        return Err(invalid(t!("core.alter.empty_table_name")));
    }
    if plan.columns.is_empty() {
        return Err(invalid(t!("core.alter.no_columns")));
    }
    if lc(&plan.new_name) != lc(&snap.table) && snap.names.iter().any(|n| lc(n) == lc(&plan.new_name)) {
        return Err(invalid(t!("core.alter.name_taken", name = plan.new_name)));
    }
    let old: HashMap<String, &ColumnSpec> = snap.columns.iter().map(|c| (lc(&c.name), c)).collect();
    let mut seen = HashSet::new();
    let mut used = HashSet::new();
    let mut rename = HashMap::new();
    for c in &plan.columns {
        if c.name.trim().is_empty() {
            return Err(invalid(t!("core.alter.empty_column_name")));
        }
        if !seen.insert(lc(&c.name)) {
            return Err(invalid(t!("core.alter.dup_column", col = c.name)));
        }
        check_type(&c.name, &c.data_type)?;
        if let Some(d) = &c.default {
            check_default(&c.name, d)?;
        }
        if let Some(o) = &c.orig {
            if !old.contains_key(&lc(o)) || !used.insert(lc(o)) {
                return Err(invalid(t!("core.alter.unknown_column", col = o)));
            }
            if o != &c.name {
                rename.insert(lc(o), c.name.clone());
            }
        }
    }
    let dropped: HashSet<String> = old.keys().filter(|k| !used.contains(*k)).cloned().collect();
    for f in &plan.fks {
        if f.columns.is_empty() || f.ref_table.trim().is_empty() {
            return Err(invalid(t!("core.alter.bad_fk")));
        }
        if !f.ref_columns.is_empty() && f.ref_columns.len() != f.columns.len() {
            return Err(invalid(t!("core.alter.bad_fk")));
        }
        for c in &f.columns {
            if !seen.contains(&lc(c)) {
                return Err(invalid(t!("core.alter.fk_unknown_column", col = c)));
            }
        }
        fk_action(&f.on_delete)?;
        fk_action(&f.on_update)?;
    }
    Ok(Ctx { snap, plan, rename, dropped, old })
}

/// 能否只用 ALTER TABLE(改名 / 改列名 / 加列 / 删列)完成。
fn simple_ok(cx: &Ctx, parsed: Option<&ParsedTable>) -> bool {
    if !cx.pk_same() || !cx.fks_same() {
        return false;
    }
    // 保留的列:相对顺序不变、定义不变;新列只能加在最后且能用 ADD COLUMN 加
    let mut last_old: Option<usize> = None;
    let mut seen_new = false;
    for c in &cx.plan.columns {
        match &c.orig {
            Some(o) => {
                let ix = cx.snap.columns.iter().position(|x| lc(&x.name) == lc(o));
                if seen_new || ix < last_old || !cx.col_same(c) {
                    return false;
                }
                last_old = ix;
            }
            None => {
                if !addable(c) {
                    return false;
                }
                seen_new = true;
            }
        }
    }
    // 删列:DROP COLUMN 的限制(主键、UNIQUE、被索引 / 触发器 / 视图 / 约束 / 外键引用)
    for d in &cx.dropped {
        let col = cx.old[d];
        if col.pk || col.generated {
            return false;
        }
        let own_unique = parsed
            .and_then(|p| p.cols.iter().find(|pc| lc(&pc.name) == *d))
            .is_some_and(|pc| pc.segs.iter().any(|(k, _)| matches!(k, Seg::Unique | Seg::References | Seg::Primary)));
        if parsed.is_none() || own_unique || cx.others_mention(parsed, &col.name) {
            return false;
        }
        if cx.snap.fks.iter().any(|f| f.columns.iter().any(|c| lc(c) == *d)) {
            return false;
        }
    }
    true
}

fn column_def(c: &ColumnSpec) -> String {
    let mut s = ident(&c.name);
    if !c.data_type.trim().is_empty() {
        s.push(' ');
        s.push_str(c.data_type.trim());
    }
    if c.not_null {
        s.push_str(" NOT NULL");
    }
    if let Some(d) = &c.default {
        s.push_str(&format!(" DEFAULT {}", d.trim()));
    }
    s
}

fn script_text(header: &str, pre: &[String], body: &[String], checks: &[String], post: &[String]) -> String {
    let mut lines = vec![format!("-- {header}")];
    lines.extend(pre.iter().map(|s| format!("{s};")));
    lines.push("BEGIN;".into());
    lines.extend(body.iter().map(|s| format!("{};", s.trim_end_matches(';'))));
    lines.extend(checks.iter().cloned());
    lines.push("COMMIT;".into());
    lines.extend(post.iter().map(|s| format!("{s};")));
    lines.join("\n")
}

/// 现状 + 目标结构 → 脚本。目标结构不合法(重名、默认值不是单个表达式等)返回错误。
pub fn generate(snap: &TableSnapshot, plan: &AlterPlan) -> AppResult<AlterScript> {
    let cx = context(snap, plan)?;
    let parsed = parse_create(&snap.create_sql);
    let renamed_table = plan.new_name != snap.table;
    let mut warnings = vec![];
    for d in snap.columns.iter().filter(|c| cx.dropped.contains(&lc(&c.name))) {
        warnings.push(t!("core.alter.warn_drop", col = d.name).to_string());
    }

    let unchanged = !renamed_table
        && cx.rename.is_empty()
        && cx.dropped.is_empty()
        && plan.columns.len() == snap.columns.len()
        && plan.columns.iter().zip(&snap.columns).all(|(c, o)| c.orig.as_deref() == Some(o.name.as_str()) && cx.col_same(c))
        && cx.pk_same()
        && cx.fks_same();
    if unchanged {
        return Ok(AlterScript {
            mode: AlterMode::Noop,
            sql: format!("-- {}", t!("core.alter.noop")),
            statements: vec![],
            fk_check: vec![],
            foreign_keys_off: false,
            warnings: vec![],
        });
    }

    let tq = ident(&snap.table);
    if simple_ok(&cx, parsed.as_ref()) {
        let mut body = vec![];
        for c in &plan.columns {
            if let Some(o) = c.orig.as_ref().filter(|o| *o != &c.name) {
                body.push(format!("ALTER TABLE {tq} RENAME COLUMN {} TO {}", ident(o), ident(&c.name)));
            }
        }
        for d in snap.columns.iter().filter(|c| cx.dropped.contains(&lc(&c.name))) {
            body.push(format!("ALTER TABLE {tq} DROP COLUMN {}", ident(&d.name)));
        }
        for c in plan.columns.iter().filter(|c| c.orig.is_none()) {
            body.push(format!("ALTER TABLE {tq} ADD COLUMN {}", column_def(c)));
        }
        if renamed_table {
            body.push(format!("ALTER TABLE {tq} RENAME TO {}", ident(&plan.new_name)));
        }
        let sql = script_text(&t!("core.alter.header_simple", table = snap.table), &[], &body, &[], &[]);
        return Ok(AlterScript {
            mode: AlterMode::Simple,
            sql,
            statements: body,
            fk_check: vec![],
            foreign_keys_off: false,
            warnings,
        });
    }

    rebuild(&cx, parsed.as_ref(), warnings)
}

fn rebuild(cx: &Ctx, parsed: Option<&ParsedTable>, mut warnings: Vec<String>) -> AppResult<AlterScript> {
    let (snap, plan) = (cx.snap, cx.plan);
    let table = &snap.table;
    let tq = ident(table);
    if parsed.is_none() {
        warnings.push(t!("core.alter.warn_unparsed").to_string());
    }
    // 临时表名不能与库里任何对象重名
    let taken: HashSet<String> = snap.names.iter().map(|n| lc(n)).collect();
    let mut tmp = format!("_datum_new_{table}");
    let mut n = 2;
    while taken.contains(&lc(&tmp)) {
        tmp = format!("_datum_new_{table}_{n}");
        n += 1;
    }

    let pk_same = cx.pk_same();
    let fks_same = cx.fks_same();
    let pcol = |orig: &str| parsed.and_then(|p| p.cols.iter().find(|pc| lc(&pc.name) == lc(orig)));
    let new_pk: Vec<&ColumnSpec> = plan.columns.iter().filter(|c| c.pk).collect();
    let old_pk_count = snap.columns.iter().filter(|c| c.pk).count();
    // 主键写在列上还是表约束里:没变就沿用原写法(原表约束里的 PRIMARY KEY 保留),变了按新主键的列数决定
    let table_level_pk_kept = pk_same && parsed.is_some_and(|p| p.constraints.iter().any(|(k, _)| *k == ConsKind::Primary));
    let column_pk = !table_level_pk_kept && new_pk.len() == 1;

    let mut defs = vec![];
    for c in &plan.columns {
        let mut s = ident(&c.name);
        let p = c.orig.as_deref().and_then(pcol);
        let ty = match p {
            Some(p) if same_type(&p.type_text, &c.data_type) => p.type_text.clone(),
            _ => c.data_type.trim().to_string(),
        };
        if !ty.is_empty() {
            s.push(' ');
            s.push_str(&ty);
        }
        let seg = |k: Seg| p.and_then(|p| p.segs.iter().find(|(x, _)| *x == k).map(|(_, t)| t.clone()));
        if c.pk && column_pk {
            // 原来就是唯一的列主键:沿用原写法(保留 AUTOINCREMENT、排序、冲突子句)
            match seg(Seg::Primary).filter(|_| old_pk_count == 1 && c.orig.as_ref().is_some_and(|o| cx.old[&lc(o)].pk)) {
                Some(t) => s.push_str(&format!(" {}", rename_text(&t, &cx.rename))),
                None => s.push_str(" PRIMARY KEY"),
            }
        }
        if c.not_null {
            s.push(' ');
            s.push_str(&seg(Seg::NotNull).unwrap_or_else(|| "NOT NULL".into()));
        }
        if let Some(d) = &c.default {
            s.push_str(&format!(" DEFAULT {}", d.trim()));
        }
        // 其余约束原样保留(列改名跟着改);外键变了时列上的 REFERENCES 统一改到表约束里重写
        if let Some(p) = p {
            for (k, text) in &p.segs {
                let keep = match k {
                    Seg::Unique | Seg::Check | Seg::Collate | Seg::Generated => true,
                    Seg::References => fks_same,
                    _ => false,
                };
                if keep {
                    s.push(' ');
                    s.push_str(&match k {
                        Seg::References => rename_fk(text, table, &cx.rename),
                        Seg::Collate => text.clone(),
                        _ => rename_text(text, &cx.rename),
                    });
                }
            }
        }
        defs.push(s);
    }
    if !column_pk && !table_level_pk_kept && !new_pk.is_empty() {
        defs.push(format!("PRIMARY KEY ({})", new_pk.iter().map(|c| ident(&c.name)).collect::<Vec<_>>().join(", ")));
    }
    if let Some(p) = parsed {
        for (k, text) in &p.constraints {
            let keep = match k {
                ConsKind::Primary => table_level_pk_kept,
                ConsKind::Foreign => fks_same,
                ConsKind::Other => true,
            };
            if !keep {
                continue;
            }
            let fk = *k == ConsKind::Foreign;
            if if fk { fk_mentions(text, table, &cx.dropped) } else { mentions(text, &cx.dropped) } {
                warnings.push(t!("core.alter.warn_constraint_dropped", def = text).to_string());
                continue;
            }
            defs.push(if fk { rename_fk(text, table, &cx.rename) } else { rename_text(text, &cx.rename) });
        }
    }
    if !fks_same {
        for f in &plan.fks {
            defs.push(fk_sql(f, table, &plan.new_name)?);
        }
    }
    let options = parsed.map(|p| p.options.clone()).unwrap_or_default();
    let create = format!(
        "CREATE TABLE {} (\n  {}\n){}",
        ident(&tmp),
        defs.join(",\n  "),
        if options.is_empty() { String::new() } else { format!(" {options}") }
    );

    // 数据复制:只复制原表里有的列(生成列不能写入)
    let copy: Vec<&ColumnSpec> = plan.columns.iter().filter(|c| c.orig.is_some() && !c.generated).collect();
    for c in plan.columns.iter().filter(|c| c.orig.is_none() && c.not_null && c.default.is_none() && !c.generated) {
        warnings.push(t!("core.alter.warn_not_null_no_default", col = c.name).to_string());
    }
    for c in &copy {
        if let Some(o) = c.orig.as_ref().map(|o| cx.old[&lc(o)]) {
            if c.not_null && !o.not_null {
                warnings.push(t!("core.alter.warn_now_not_null", col = c.name).to_string());
            }
        }
    }

    let mut body = vec!["PRAGMA legacy_alter_table = ON".to_string(), create];
    if !copy.is_empty() {
        body.push(format!(
            "INSERT INTO {} ({}) SELECT {} FROM {tq}",
            ident(&tmp),
            copy.iter().map(|c| ident(&c.name)).collect::<Vec<_>>().join(", "),
            copy.iter().map(|c| ident(c.orig.as_deref().unwrap_or_default())).collect::<Vec<_>>().join(", ")
        ));
    }

    // 受列改名 / 删列影响的视图:先删,改完再按原定义(跟着改名)重建
    let tl = lc(table);
    let tn: HashSet<String> = [tl.clone()].into();
    let touched: HashSet<String> = cx.rename.keys().cloned().chain(cx.dropped.iter().cloned()).collect();
    let views: Vec<&SchemaObject> = snap
        .objects
        .iter()
        .filter(|o| o.kind == "view" && mentions(&o.sql, &tn) && mentions(&o.sql, &touched))
        .collect();
    for v in &views {
        body.push(format!("DROP VIEW {}", ident(&v.name)));
    }
    body.push(format!("DROP TABLE {tq}"));
    body.push(format!("ALTER TABLE {} RENAME TO {tq}", ident(&tmp)));

    // 本表的索引与触发器(DROP TABLE 时一起删了):按原 SQL 重建,列改名跟着改;
    // 引用了删掉的列的建不出来,注释掉并提示
    let mut commented = vec![];
    for o in snap.objects.iter().filter(|o| (o.kind == "index" || o.kind == "trigger") && lc(&o.table) == tl) {
        if mentions(&o.sql, &cx.dropped) {
            warnings.push(t!("core.alter.warn_object_dropped", kind = o.kind, name = o.name).to_string());
            commented.push(o.sql.clone());
            continue;
        }
        if o.kind == "trigger" && mentions(&o.sql, &cx.rename.keys().cloned().collect()) {
            warnings.push(t!("core.alter.warn_check_renamed", kind = o.kind, name = o.name).to_string());
        }
        body.push(rename_in_object(&o.sql, &cx.rename));
    }
    for v in &views {
        if mentions(&v.sql, &cx.dropped) {
            warnings.push(t!("core.alter.warn_view_dropped_col", name = v.name).to_string());
        } else {
            warnings.push(t!("core.alter.warn_check_renamed", kind = "view", name = v.name).to_string());
        }
        body.push(rename_in_object(&v.sql, &cx.rename));
    }
    // 别的表的触发器、外键里提到了改名 / 删掉的列:不动它们,提示人工检查
    for o in snap.objects.iter().filter(|o| o.kind == "trigger" && lc(&o.table) != tl) {
        if mentions(&o.sql, &tn) && mentions(&o.sql, &touched) {
            warnings.push(t!("core.alter.warn_check_other", kind = o.kind, name = o.name).to_string());
        }
    }
    for r in &snap.referenced_by {
        if r.columns.iter().any(|c| touched.contains(&lc(c))) {
            warnings.push(t!("core.alter.warn_child_fk", table = r.table).to_string());
        }
    }

    body.push(format!("PRAGMA legacy_alter_table = {}", if snap.legacy_alter_table { "ON" } else { "OFF" }));
    let final_name = plan.new_name.clone();
    if final_name != *table {
        // 关掉 legacy 后再改名:SQLite 会把视图、触发器、别的表外键里对本表的引用一并改过来
        body.push(format!("ALTER TABLE {tq} RENAME TO {}", ident(&final_name)));
    }

    let mut fk_check = vec![final_name.clone()];
    for r in &snap.referenced_by {
        if !fk_check.iter().any(|x| lc(x) == lc(&r.table)) {
            fk_check.push(r.table.clone());
        }
    }
    let checks: Vec<String> = fk_check.iter().map(|t| format!("PRAGMA foreign_key_check({});", ident(t))).collect();
    let mut post = vec![];
    if snap.foreign_keys_on {
        post.push("PRAGMA foreign_keys = ON".to_string());
    }
    let mut sql = script_text(
        &t!("core.alter.header_rebuild", table = table),
        &["PRAGMA foreign_keys = OFF".to_string()],
        &body,
        &checks,
        &post,
    );
    if !commented.is_empty() {
        sql.push_str(&format!("\n-- {}", t!("core.alter.commented_objects")));
        for c in &commented {
            for line in c.lines() {
                sql.push_str(&format!("\n-- {line}"));
            }
        }
    }
    Ok(AlterScript { mode: AlterMode::Rebuild, sql, statements: body, fk_check, foreign_keys_off: true, warnings })
}

fn rename_text(text: &str, rename: &HashMap<String, String>) -> String {
    match lex(text) {
        Some(toks) => render(&toks, rename),
        None => text.to_string(),
    }
}

/// 外键子句(列上的 REFERENCES … 或表约束 FOREIGN KEY (…) REFERENCES …)按「本表部分 / 被引部分」拆开:
/// 被引的列属于别的表,同名也不能跟着本表改名;只有自引用(被引表就是本表)时才一起改。
fn split_fk(text: &str, table: &str) -> Option<(String, String, bool)> {
    let toks = lex(text)?;
    let at = toks.iter().position(|t| t.upper() == "REFERENCES")?;
    let target = toks[at + 1..].iter().find(|t| t.sig()).and_then(|t| t.name());
    let self_ref = target.is_some_and(|n| lc(&n) == lc(table));
    let local: String = toks[..at].iter().map(|t| t.text).collect();
    let parent: String = toks[at..].iter().map(|t| t.text).collect();
    Some((local, parent, self_ref))
}

/// 外键子句改名:本表部分照改;被引部分仅自引用时改(跳过紧跟 REFERENCES 的表名)。
fn rename_fk(text: &str, table: &str, rename: &HashMap<String, String>) -> String {
    let Some(toks) = lex(text) else { return text.to_string() };
    let Some(at) = toks.iter().position(|t| t.upper() == "REFERENCES") else { return render(&toks, rename) };
    let name_at = (at + 1..toks.len()).find(|&i| toks[i].sig());
    let self_ref = name_at.and_then(|i| toks[i].name()).is_some_and(|n| lc(&n) == lc(table));
    let mut s = String::new();
    for (i, t) in toks.iter().enumerate() {
        if !t.sig() {
            if !s.ends_with(' ') {
                s.push(' ');
            }
            continue;
        }
        let local = i < at || (self_ref && Some(i) != name_at);
        match t.name().filter(|_| local).and_then(|n| rename.get(&lc(&n))) {
            Some(new) => s.push_str(&ident(new)),
            None => s.push_str(t.text),
        }
    }
    s.trim().to_string()
}

/// 外键子句里本表那部分是否提到了这些列。
fn fk_mentions(text: &str, table: &str, names: &HashSet<String>) -> bool {
    match split_fk(text, table) {
        Some((local, parent, self_ref)) => mentions(&local, names) || (self_ref && mentions(&parent, names)),
        None => mentions(text, names),
    }
}

// ------------------------------------------------------------ 读现状与执行

fn q_err(e: rusqlite::Error) -> AppError {
    AppError::new(ErrorKind::Query, t!("core.err.query_failed")).with_detail(e.to_string())
}

fn pragma_on(conn: &Connection, name: &str) -> AppResult<bool> {
    conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get::<_, i64>(0)).map(|v| v != 0).map_err(q_err)
}

/// 读出表的现状。表不存在时报错。
pub fn snapshot(conn: &Connection, table: &str) -> AppResult<TableSnapshot> {
    let (name, create_sql): (String, String) = conn
        .query_row(
            "SELECT name, sql FROM sqlite_master WHERE type = 'table' AND name = ?1 COLLATE NOCASE",
            [table],
            |r| Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())),
        )
        .map_err(|_| AppError::new(ErrorKind::NotFound, t!("core.alter.no_table", table = table)))?;

    let mut st = conn
        .prepare("SELECT name, type, \"notnull\", dflt_value, pk, hidden FROM pragma_table_xinfo(?1) ORDER BY cid")
        .map_err(q_err)?;
    let mut columns = vec![];
    let rows = st
        .query_map([&name], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, i64>(2)? != 0,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })
        .map_err(q_err)?;
    for row in rows {
        let (n, ty, nn, def, pk, hidden) = row.map_err(q_err)?;
        // hidden = 1 是虚表的隐藏列;2 / 3 是生成列
        if hidden == 1 {
            continue;
        }
        columns.push(ColumnSpec {
            orig: Some(n.clone()),
            name: n,
            data_type: ty,
            not_null: nn,
            default: def,
            pk: pk > 0,
            generated: hidden >= 2,
        });
    }

    let mut st = conn
        .prepare("SELECT id, \"table\", \"from\", \"to\", on_update, on_delete FROM pragma_foreign_key_list(?1) ORDER BY id, seq")
        .map_err(q_err)?;
    let mut fks: Vec<(i64, FkSpec)> = vec![];
    let rows = st
        .query_map([&name], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .map_err(q_err)?;
    for row in rows {
        let (id, rt, from, to, upd, del) = row.map_err(q_err)?;
        if fks.last().map(|f| f.0) != Some(id) {
            fks.push((id, FkSpec { columns: vec![], ref_table: rt, ref_columns: vec![], on_delete: del, on_update: upd }));
        }
        let f = &mut fks.last_mut().expect("just pushed").1;
        f.columns.push(from);
        if let Some(to) = to {
            f.ref_columns.push(to);
        }
    }

    let mut st = conn
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master WHERE type IN ('index', 'trigger', 'view') AND sql IS NOT NULL ORDER BY rowid")
        .map_err(q_err)?;
    let objects = st
        .query_map([], |r| Ok(SchemaObject { kind: r.get(0)?, name: r.get(1)?, table: r.get(2)?, sql: r.get(3)? }))
        .map_err(q_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(q_err)?;
    let mut st = conn.prepare("SELECT name FROM sqlite_master").map_err(q_err)?;
    let names = st.query_map([], |r| r.get(0)).map_err(q_err)?.collect::<Result<Vec<String>, _>>().map_err(q_err)?;

    let mut st = conn
        .prepare(
            "SELECT m.name, f.id, f.\"to\" FROM sqlite_master m JOIN pragma_foreign_key_list(m.name) f \
             WHERE m.type = 'table' AND f.\"table\" = ?1 COLLATE NOCASE AND m.name <> ?1 ORDER BY m.name, f.id, f.seq",
        )
        .map_err(q_err)?;
    let mut referenced_by: Vec<RefBy> = vec![];
    let rows = st
        .query_map([&name], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(2)?)))
        .map_err(q_err)?;
    for row in rows {
        let (t, to) = row.map_err(q_err)?;
        if referenced_by.last().map(|r| &r.table) != Some(&t) {
            referenced_by.push(RefBy { table: t, columns: vec![] });
        }
        if let Some(to) = to {
            referenced_by.last_mut().expect("just pushed").columns.push(to);
        }
    }

    Ok(TableSnapshot {
        table: name,
        create_sql,
        columns,
        fks: fks.into_iter().map(|(_, f)| f).collect(),
        objects,
        names,
        referenced_by,
        foreign_keys_on: pragma_on(conn, "foreign_keys")?,
        legacy_alter_table: pragma_on(conn, "legacy_alter_table")?,
    })
}

/// 执行一条语句:只接受单条语句——rusqlite 默认会静默丢掉第一条之后的内容,这里按 SQLite 实际
/// 编译掉的文本(`sqlite3_sql`)核对,后面只许剩空白、注释与分号;PRAGMA 之类返回行的照样执行。
fn exec_one(conn: &Connection, sql: &str) -> rusqlite::Result<()> {
    let mut st = conn.prepare(sql)?;
    let used = st.expanded_sql().unwrap_or_default();
    let rest = sql.trim_start().strip_prefix(used.as_str()).unwrap_or(sql);
    let clean = lex(rest).is_some_and(|toks| toks.iter().all(|t| !t.sig() || t.is(";")));
    if !clean {
        return Err(rusqlite::Error::MultipleStatement);
    }
    let mut rows = st.raw_query();
    while rows.next()?.is_some() {}
    Ok(())
}

/// 在一个事务里执行脚本:任一条失败整体回滚;外键检查有违规也回滚并返回违规行。
/// 重建表前若外键约束开着就先关掉(PRAGMA foreign_keys 在事务里不生效,只能放在 BEGIN 之前),
/// 结束后无论成败都恢复外键与 legacy_alter_table 原来的设置。
pub fn apply(conn: &Connection, script: &AlterScript) -> AppResult<ApplyOutcome> {
    if script.statements.is_empty() {
        return Ok(ApplyOutcome { applied: false, violations: vec![] });
    }
    let fk_was_on = pragma_on(conn, "foreign_keys")?;
    let legacy_was = pragma_on(conn, "legacy_alter_table")?;
    let restore = |conn: &Connection| {
        let _ = conn.execute_batch(if legacy_was { "PRAGMA legacy_alter_table = ON" } else { "PRAGMA legacy_alter_table = OFF" });
        if script.foreign_keys_off && fk_was_on {
            let _ = conn.execute_batch("PRAGMA foreign_keys = ON");
        }
    };
    if script.foreign_keys_off && fk_was_on {
        conn.execute_batch("PRAGMA foreign_keys = OFF").map_err(q_err)?;
    }
    if let Err(e) = conn.execute_batch("BEGIN") {
        restore(conn);
        return Err(AppError::new(ErrorKind::Query, t!("core.err.begin_tx_failed")).with_detail(e.to_string()));
    }
    let run = || -> AppResult<Vec<FkViolation>> {
        for (i, s) in script.statements.iter().enumerate() {
            exec_one(conn, s).map_err(|e| {
                AppError::new(ErrorKind::Query, t!("core.err.stmt_failed_all_rolled_back", n = i + 1)).with_detail(format!("{e}\n{s}"))
            })?;
        }
        let mut out = vec![];
        for table in &script.fk_check {
            let mut st = conn.prepare(&format!("PRAGMA foreign_key_check({})", ident(table))).map_err(q_err)?;
            let rows = st
                .query_map([], |r| {
                    Ok(FkViolation {
                        table: r.get(0)?,
                        rowid: r.get(1)?,
                        parent: r.get(2)?,
                        fkid: r.get(3)?,
                    })
                })
                .map_err(q_err)?;
            for r in rows {
                out.push(r.map_err(q_err)?);
            }
        }
        Ok(out)
    };
    let result = run();
    let outcome = match result {
        Ok(v) if v.is_empty() => match conn.execute_batch("COMMIT") {
            Ok(()) => Ok(ApplyOutcome { applied: true, violations: vec![] }),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(AppError::new(ErrorKind::Query, t!("core.err.commit_tx_failed")).with_detail(e.to_string()))
            }
        },
        Ok(v) => {
            let _ = conn.execute_batch("ROLLBACK");
            Ok(ApplyOutcome { applied: false, violations: v })
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    };
    restore(conn);
    outcome
}

/// 读现状 → 生成 → 核对与预览一致 → 执行,全程持有同一个连接。
/// `expected_sql` 是用户预览过的脚本:期间表结构被别处改了,生成的脚本就会不同,拒绝执行、请重新预览。
pub fn preview_and_apply(conn: &Connection, plan: &AlterPlan, expected_sql: &str) -> AppResult<ApplyOutcome> {
    let snap = snapshot(conn, &plan.table)?;
    let script = generate(&snap, plan)?;
    if script.sql != expected_sql {
        return Err(AppError::new(ErrorKind::Query, t!("core.alter.stale")));
    }
    apply(conn, &script)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexer_handles_quotes_and_comments() {
        let toks = lex("a \"b\"\"c\" 'x;y' -- c;\n[d e] `f`").unwrap();
        let names: Vec<String> = toks.iter().filter_map(|t| t.name()).collect();
        assert_eq!(names, vec!["a", "b\"c", "d e", "f"]);
        assert!(lex("'open").is_none());
        assert!(lex("/* open").is_none());
    }

    #[test]
    fn parses_column_segments() {
        let p = parse_create(
            "CREATE TABLE t (\n  id INTEGER PRIMARY KEY AUTOINCREMENT, -- 主键\n  \
             name VARCHAR(20) NOT NULL DEFAULT 'a,b' COLLATE NOCASE,\n  \
             n INT DEFAULT NULL CHECK (n > 0),\n  \
             p INT CONSTRAINT fk_p REFERENCES other(id) ON DELETE SET NULL NOT DEFERRABLE,\n  \
             g INT GENERATED ALWAYS AS (n * 2) STORED,\n  \
             UNIQUE (name, n), CONSTRAINT c CHECK (n < 10)\n) WITHOUT ROWID",
        )
        .unwrap();
        assert_eq!(p.cols.len(), 5);
        assert_eq!(p.cols[0].segs, vec![(Seg::Primary, "PRIMARY KEY AUTOINCREMENT".to_string())]);
        assert_eq!(p.cols[1].type_text, "VARCHAR(20)");
        let kinds: Vec<Seg> = p.cols[1].segs.iter().map(|s| s.0).collect();
        assert_eq!(kinds, vec![Seg::NotNull, Seg::Default, Seg::Collate]);
        assert_eq!(p.cols[1].segs[1].1, "DEFAULT 'a,b'");
        assert_eq!(p.cols[2].segs.iter().map(|s| s.0).collect::<Vec<_>>(), vec![Seg::Default, Seg::Check]);
        assert_eq!(p.cols[3].segs.len(), 1);
        assert_eq!(p.cols[3].segs[0].0, Seg::References);
        assert_eq!(p.cols[4].segs.iter().map(|s| s.0).collect::<Vec<_>>(), vec![Seg::Generated]);
        assert_eq!(p.constraints.len(), 2);
        assert_eq!(p.options, "WITHOUT ROWID");
        assert!(parse_create("CREATE TABLE t AS SELECT 1").is_none());
    }

    #[test]
    fn default_and_type_validation() {
        for ok in ["0", "-1", "'x;y'", "CURRENT_TIMESTAMP", "(datetime('now'))", "'it''s'"] {
            assert!(check_default("c", ok).is_ok(), "{ok}");
        }
        for bad in ["1; DROP TABLE t", "1 -- x", "(1", "1)", "'open", "1\nDROP TABLE t", "/* x */ 1", ""] {
            assert!(check_default("c", bad).is_err(), "{bad}");
        }
        assert!(check_type("c", "DECIMAL(10, 2)").is_ok());
        assert!(check_type("c", "").is_ok());
        assert!(check_type("c", "INT; DROP TABLE t").is_err());
        assert!(check_type("c", "TEXT 'x'").is_err());
    }

    #[test]
    fn rename_skips_object_name_and_strings() {
        let m: HashMap<String, String> = [("a".to_string(), "b".to_string())].into();
        assert_eq!(
            rename_in_object("CREATE INDEX a ON t (a, 'a') WHERE \"A\" > 0", &m),
            "CREATE INDEX a ON t (\"b\", 'a') WHERE \"b\" > 0"
        );
        assert_eq!(rename_in_object("CREATE INDEX IF NOT EXISTS main.a ON t(a)", &m), "CREATE INDEX IF NOT EXISTS main.a ON t(\"b\")");
    }
}
