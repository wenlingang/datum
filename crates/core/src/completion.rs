//! 编辑器补全的纯逻辑:给定文本、光标(字节偏移)与 schema,算出候选项与替换范围。
//! UI 层把结果转换成编辑器的补全菜单项。

use crate::schema::{ColumnsByTable, ForeignKey};
use crate::sql::stmt::{current_statement, split_statements};
use regex::Regex;
use rust_i18n::t;
use std::ops::Range;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Keyword,
    Table,
    Column,
    Snippet,
    Command,
    Key,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub detail: String,
    pub kind: ItemKind,
    /// 实际插入的文本(缺省同 label)
    pub insert: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completions {
    /// 被替换的字节范围(光标前已输入的词)
    pub replace: Range<usize>,
    pub items: Vec<Item>,
}

const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "FULL",
    "CROSS",
    "ON",
    "USING",
    "GROUP",
    "BY",
    "ORDER",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "FETCH",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "AS",
    "DISTINCT",
    "ALL",
    "ANY",
    "SOME",
    "CREATE",
    "TABLE",
    "ALTER",
    "DROP",
    "TRUNCATE",
    "VIEW",
    "INDEX",
    "UNIQUE",
    "PRIMARY",
    "KEY",
    "FOREIGN",
    "REFERENCES",
    "CONSTRAINT",
    "DEFAULT",
    "CHECK",
    "LIKE",
    "ILIKE",
    "IN",
    "BETWEEN",
    "IS",
    "EXISTS",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "ASC",
    "DESC",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "WITH",
    "RETURNING",
    "CAST",
    "COALESCE",
    "NULLS",
    "FIRST",
    "LAST",
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "NOW",
    "EXPLAIN",
    "ANALYZE",
    "SHOW",
    "DATABASE",
    "SCHEMA",
    "COLUMN",
    "ADD",
    "RENAME",
    "TO",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "TRUE",
    "FALSE",
    "INTEGER",
    "BIGINT",
    "VARCHAR",
    "TEXT",
    "BOOLEAN",
    "TIMESTAMP",
    "DATE",
    "NUMERIC",
    "SERIAL",
    "AUTO_INCREMENT",
    "COMMENT",
    "PRAGMA",
    "IF",
    "REPLACE",
];

/// 输入边界字符时会被自动转成大写的关键字(与原版编辑器一致的较小集合)。
const UPPER_ON_BOUNDARY: &[&str] = &[
    "select",
    "from",
    "where",
    "insert",
    "into",
    "values",
    "update",
    "set",
    "delete",
    "join",
    "left",
    "right",
    "inner",
    "outer",
    "full",
    "cross",
    "on",
    "using",
    "group",
    "by",
    "order",
    "having",
    "limit",
    "offset",
    "fetch",
    "and",
    "or",
    "not",
    "null",
    "as",
    "distinct",
    "all",
    "any",
    "some",
    "create",
    "table",
    "alter",
    "drop",
    "truncate",
    "view",
    "index",
    "unique",
    "primary",
    "key",
    "foreign",
    "references",
    "constraint",
    "default",
    "check",
    "like",
    "ilike",
    "in",
    "between",
    "is",
    "exists",
    "union",
    "intersect",
    "except",
    "asc",
    "desc",
    "case",
    "when",
    "then",
    "else",
    "end",
    "with",
    "returning",
    "cast",
    "coalesce",
    "nulls",
    "first",
    "last",
];

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 光标前的标识符前缀起点。
fn word_start(text: &str, offset: usize) -> usize {
    let bytes = text.as_bytes();
    let mut start = offset;
    while start > 0 && is_word_byte(bytes[start - 1]) {
        start -= 1;
    }
    start
}

/// 输入边界字符(空白 , ; ( ))后,若紧邻前面的词是 SQL 关键字且非全大写,返回要替换的范围与大写文本。
/// `offset` 为边界字符之后的光标位置。
pub fn uppercase_keyword_before(text: &str, offset: usize) -> Option<(Range<usize>, String)> {
    let bytes = text.as_bytes();
    if offset == 0 || offset > bytes.len() {
        return None;
    }
    let boundary = bytes[offset - 1];
    if !(boundary.is_ascii_whitespace() || matches!(boundary, b',' | b';' | b'(' | b')')) {
        return None;
    }
    let end = offset - 1;
    let start = word_start(text, end);
    if start == end {
        return None;
    }
    // 处于字符串或行注释中时不改写
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_before = &text[line_start..start];
    if line_before.contains("--") || line_before.matches('\'').count() % 2 == 1 {
        return None;
    }
    let word = &text[start..end];
    let upper = word.to_ascii_uppercase();
    (UPPER_ON_BOUNDARY.contains(&word.to_ascii_lowercase().as_str()) && word != upper)
        .then_some((start..end, upper))
}

static EAGER_TABLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(from|join|into|update|table)\s+$").unwrap());
static EAGER_COLUMN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(select|where|and|or|on|set|having|by)\s+$").unwrap());
static TABLE_REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(?:from|join|update|into)\s+(?:\w+\.)?(?:"([^"]+)"|`([^`]+)`|(\w+))(?:\s+(?:as\s+)?(\w+))?"#).unwrap()
});
static AFTER_KEYWORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^a-zA-Z])([a-zA-Z]+)\s+$").unwrap());
static AFTER_TABLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^a-zA-Z])([a-zA-Z]+)\s+\w+\s+$").unwrap());

const NOT_ALIAS: &[&str] = &[
    "where",
    "join",
    "left",
    "right",
    "inner",
    "outer",
    "on",
    "group",
    "order",
    "limit",
    "set",
    "values",
    "using",
    "cross",
    "full",
    "natural",
    "having",
    "union",
    "select",
    "offset",
    "returning",
];

/// 解析语句中被引用的表及其别名:返回 (表名(对齐到已知大小写), 别名)。
fn table_refs(stmt: &str, columns: &ColumnsByTable) -> Vec<(String, Option<String>)> {
    let mut refs: Vec<(String, Option<String>)> = Vec::new();
    for caps in TABLE_REF.captures_iter(stmt) {
        let raw = caps
            .get(1)
            .or(caps.get(2))
            .or(caps.get(3))
            .unwrap()
            .as_str();
        let known = if columns.contains_key(raw) {
            Some(raw.to_string())
        } else {
            columns
                .keys()
                .find(|k| k.eq_ignore_ascii_case(raw))
                .cloned()
        };
        let Some(table) = known else { continue };
        let alias = caps
            .get(4)
            .map(|m| m.as_str().to_string())
            .filter(|a| !NOT_ALIAS.contains(&a.to_ascii_lowercase().as_str()));
        if !refs.iter().any(|(t, a)| *t == table && *a == alias) {
            refs.push((table, alias));
        }
    }
    refs
}

fn column_items(columns: &ColumnsByTable, tables: &[String], tag_source: bool) -> Vec<Item> {
    tables
        .iter()
        .flat_map(|t| {
            columns.get(t).into_iter().flatten().map(move |c| Item {
                label: c.name.clone(),
                detail: if tag_source {
                    format!("{} · {t}", c.data_type)
                } else {
                    c.data_type.clone()
                },
                kind: ItemKind::Column,
                insert: c.name.clone(),
            })
        })
        .collect()
}

fn table_items(tables: &[String]) -> Vec<Item> {
    tables
        .iter()
        .map(|t| Item {
            label: t.clone(),
            detail: t!("core.completion.table").to_string(),
            kind: ItemKind::Table,
            insert: t.clone(),
        })
        .collect()
}

fn snippet(label: &str, detail: &str, insert: &str) -> Item {
    Item {
        label: label.into(),
        detail: detail.into(),
        kind: ItemKind::Snippet,
        insert: insert.into(),
    }
}

fn filter_prefix(items: Vec<Item>, prefix: &str) -> Vec<Item> {
    if prefix.is_empty() {
        return items;
    }
    let p = prefix.to_lowercase();
    let mut starts: Vec<Item> = Vec::new();
    let mut contains: Vec<Item> = Vec::new();
    for it in items {
        let l = it.label.to_lowercase();
        if l == p {
            continue; // 已完整输入,不再建议自身
        }
        if l.starts_with(&p) {
            starts.push(it);
        } else if it.kind != ItemKind::Keyword && l.contains(&p) {
            contains.push(it);
        }
    }
    starts.extend(contains);
    starts
}

/// SQL 补全。`explicit` 为用户主动请求(不依赖前缀)。
pub fn sql_completions(
    text: &str,
    offset: usize,
    tables: &[String],
    columns: &ColumnsByTable,
    explicit: bool,
) -> Option<Completions> {
    sql_completions_with(text, offset, tables, columns, &[], explicit)
}

/// SQL 补全(带外键):`fks` 为物理外键 + 推断外键,用于 JOIN 的 `ON` 条件与关联表排序。
pub fn sql_completions_with(
    text: &str,
    offset: usize,
    tables: &[String],
    columns: &ColumnsByTable,
    fks: &[ForeignKey],
    explicit: bool,
) -> Option<Completions> {
    let offset = offset.min(text.len());
    let start = word_start(text, offset);
    let prefix = &text[start..offset];
    let before = &text[..start];

    // 字符串或行注释里不补全
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let line_before = &text[line_start..start];
    if line_before.contains("--") || line_before.matches('\'').count() % 2 == 1 {
        return None;
    }

    let stmt_start = split_statements(text)
        .into_iter()
        .find(|r| offset <= r.end)
        .map_or(0, |r| r.start);
    let stmt = current_statement(text, offset);
    let stmt_before = &text[stmt_start.min(start)..start];

    // 「表.」或「别名.」→ 该表的列
    if before.ends_with('.') {
        let qual_end = before.len() - 1;
        let qual_start = word_start(text, qual_end);
        let qual = &text[qual_start..qual_end];
        let qual = qual.trim_matches(|c| c == '"' || c == '`');
        let refs = table_refs(&stmt, columns);
        let table = refs
            .iter()
            .find(|(_, a)| a.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(qual)))
            .map(|(t, _)| t.clone())
            .or_else(|| {
                columns
                    .keys()
                    .find(|k| k.eq_ignore_ascii_case(qual))
                    .cloned()
            });
        let items = table
            .map(|t| column_items(columns, &[t], false))
            .unwrap_or_default();
        let items = filter_prefix(items, prefix);
        return (!items.is_empty()).then_some(Completions {
            replace: start..offset,
            items,
        });
    }

    let tail = &stmt_before[stmt_before.len().saturating_sub(40)..];
    let tail = tail.trim_start_matches(|c: char| !c.is_ascii());

    // JOIN 表名之后:按外键给出 ON 条件
    if !fks.is_empty() {
        if let Some(items) = join_on_items(stmt_before, tail, prefix, columns, fks) {
            let items = filter_prefix(items, prefix);
            if !items.is_empty() {
                return Some(Completions { replace: start..offset, items });
            }
        }
    }

    // 表名位置:FROM/JOIN/INTO/UPDATE 后立即弹出;JOIN 后与已引用表有外键关联的表排前面
    if EAGER_TABLE.is_match(tail) {
        let mut items = table_items(tables);
        if !fks.is_empty() && JOIN_TABLE.is_match(tail) {
            let left: Vec<String> = table_refs(stmt_before, columns).into_iter().map(|(t, _)| t).collect();
            items = rank_related(items, &left, fks);
        }
        let items = filter_prefix(items, prefix);
        return (!items.is_empty()).then_some(Completions {
            replace: start..offset,
            items,
        });
    }

    let mut items: Vec<Item> = Vec::new();

    // 子句模板:刚打完 SELECT / INSERT,或 UPDATE 表名之后
    if prefix.is_empty() {
        if let Some(caps) = AFTER_KEYWORD.captures(tail) {
            match caps[1].to_ascii_lowercase().as_str() {
                "select" => {
                    items.push(snippet(
                        "* FROM",
                        &t!("core.completion.select_all"),
                        "* FROM ",
                    ));
                }
                "insert" => items.push(snippet(
                    "INTO … VALUES",
                    &t!("core.completion.insert_row"),
                    "INTO ",
                )),
                _ => {}
            }
        }
        if let Some(caps) = AFTER_TABLE.captures(tail) {
            if caps[1].eq_ignore_ascii_case("update") {
                items.push(snippet(
                    "SET … WHERE",
                    &t!("core.completion.update_rows"),
                    "SET ",
                ));
            }
        }
    }

    // 作用域内表的裸列名
    let refs = table_refs(&stmt, columns);
    let ref_tables: Vec<String> =
        refs.iter()
            .map(|(t, _)| t.clone())
            .fold(Vec::new(), |mut acc, t| {
                if !acc.contains(&t) {
                    acc.push(t);
                }
                acc
            });
    let column_position = EAGER_COLUMN.is_match(tail);
    if !ref_tables.is_empty() && (column_position || !prefix.is_empty() || explicit) {
        items.extend(column_items(columns, &ref_tables, ref_tables.len() > 1));
    }

    if prefix.is_empty() && !explicit && !column_position && items.is_empty() {
        return None;
    }
    if !prefix.is_empty() || explicit {
        items.extend(table_items(tables));
        items.extend(KEYWORDS.iter().map(|k| Item {
            label: k.to_string(),
            detail: t!("core.completion.keyword").to_string(),
            kind: ItemKind::Keyword,
            insert: k.to_string(),
        }));
    }
    let items = filter_prefix(items, prefix);
    (!items.is_empty()).then_some(Completions {
        replace: start..offset,
        items,
    })
}

static JOIN_TABLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bjoin\s+$").unwrap());
/// `JOIN 表 [AS] [别名] ` 或 `… ON `:组 1/2/3 为表名(不同引号),组 4 为别名,组 5 为 ON。
static JOINED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\bjoin\s+(?:\w+\.)?(?:"([^"]+)"|`([^`]+)`|(\w+))(?:\s+(?:as\s+)?(\w+))?\s+(on\s+)?$"#).unwrap()
});

/// 与左侧已引用表有外键关联的表排到前面,详情标出关联到谁。
fn rank_related(items: Vec<Item>, left: &[String], fks: &[ForeignKey]) -> Vec<Item> {
    let related = |t: &str| -> Option<String> {
        fks.iter().find_map(|f| {
            if f.table == t && left.contains(&f.ref_table) {
                Some(f.ref_table.clone())
            } else if f.ref_table == t && left.contains(&f.table) {
                Some(f.table.clone())
            } else {
                None
            }
        })
    };
    let (mut hit, rest): (Vec<Item>, Vec<Item>) = items.into_iter().partition(|i| related(&i.label).is_some());
    for i in &mut hit {
        if let Some(other) = related(&i.label) {
            i.detail = t!("core.completion.related_to", table = other).to_string();
        }
    }
    hit.extend(rest);
    hit
}

/// 刚写完 `JOIN 表 [别名] `(或再加 `ON `)时,按外键生成连接条件。复合外键用 AND 拼成一条。
/// 左侧的表来自光标前的语句;表名 / 别名按用户写法引用。
fn join_on_items(
    stmt_before: &str,
    tail: &str,
    prefix: &str,
    columns: &ColumnsByTable,
    fks: &[ForeignKey],
) -> Option<Vec<Item>> {
    let caps = JOINED.captures(tail)?;
    let raw = caps.get(1).or(caps.get(2)).or(caps.get(3))?.as_str();
    let after_on = caps.get(5).is_some();
    let alias = caps
        .get(4)
        .map(|m| m.as_str().to_string())
        .filter(|a| !NOT_ALIAS.contains(&a.to_ascii_lowercase().as_str()));
    // 「JOIN t 」后面跟着的别名没写完时不打扰;前缀只可能是 ON 的开头
    if !after_on && !prefix.is_empty() && !"on".starts_with(&prefix.to_ascii_lowercase()) {
        return None;
    }
    let joined = columns.keys().find(|k| k.as_str() == raw || k.eq_ignore_ascii_case(raw))?.clone();
    let joined_name = alias.clone().unwrap_or_else(|| raw.to_string());
    // 左侧:JOIN 之前出现的表(排除这次 JOIN 自己)
    let join_at = stmt_before.len() - tail.len() + caps.get(0)?.start();
    let left = table_refs(&stmt_before[..join_at], columns);
    if left.is_empty() {
        return None;
    }
    let name_of = |table: &str| -> Option<String> {
        left.iter().find(|(t, _)| t == table).map(|(t, a)| a.clone().unwrap_or_else(|| t.clone()))
    };

    // (约束, 方向) → 条件列表;按出现顺序保持稳定
    let mut groups: Vec<(String, bool, Vec<String>, String)> = vec![];
    for f in fks {
        if f.ref_column.is_empty() {
            continue;
        }
        let other = |t: &str| name_of(t).filter(|o| *o != joined_name);
        let cond = if f.table == joined {
            other(&f.ref_table).map(|o| format!("{joined_name}.{} = {o}.{}", f.column, f.ref_column))
        } else {
            None
        }
        .or_else(|| {
            (f.ref_table == joined)
                .then(|| other(&f.table))
                .flatten()
                .map(|o| format!("{joined_name}.{} = {o}.{}", f.ref_column, f.column))
        });
        let Some(cond) = cond else { continue };
        let key = format!("{}|{}|{}", f.table, f.ref_table, f.constraint);
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.2.push(cond),
            None => groups.push((key, f.inferred, vec![cond], f.constraint.clone())),
        }
    }
    if groups.is_empty() {
        return None;
    }
    // 物理外键在前
    groups.sort_by_key(|g| g.1);
    let items = groups
        .into_iter()
        .map(|(_, inferred, conds, constraint)| {
            let body = conds.join(" AND ");
            let text = if after_on { body } else { format!("ON {body}") };
            let detail = if inferred {
                t!("core.completion.fk_inferred").to_string()
            } else {
                t!("core.completion.fk", name = constraint).to_string()
            };
            Item { label: text.clone(), detail, kind: ItemKind::Snippet, insert: text }
        })
        .collect();
    Some(items)
}

/// 光标处于某列的取值位置(`col = '…`、`col IN ('a', '…`):补全该列的实际取值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueContext {
    pub table: String,
    pub column: String,
    /// 被替换的字节范围:正在输入的值(含开头的引号)
    pub replace: Range<usize>,
    /// 已输入的部分(不含引号),用于过滤
    pub prefix: String,
    /// 正在引号里
    pub quoted: bool,
}

static VALUE_POS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?:(\w+)\.)?(\w+|"[^"]+"|`[^`]+`)\s*(?:=|<>|!=|\bin\s*\((?:\s*(?:'(?:[^']|'')*'|[\w.-]+)\s*,)*)\s*('(?:[^']|'')*|[\w.-]*)$"#,
    )
    .unwrap()
});

/// 判断光标是否处在某列的取值位置;列需能对应到语句里引用的表(别名 / 表名限定,或在唯一的表里)。
pub fn value_context(text: &str, offset: usize, columns: &ColumnsByTable) -> Option<ValueContext> {
    let offset = offset.min(text.len());
    let stmt_start = split_statements(text)
        .into_iter()
        .find(|r| offset <= r.end)
        .map_or(0, |r| r.start);
    let before = &text[stmt_start.min(offset)..offset];
    let line = &before[before.rfind('\n').map_or(0, |i| i + 1)..];
    if line.contains("--") {
        return None;
    }
    let caps = VALUE_POS.captures(before)?;
    let token = caps.get(3)?;
    let quoted = token.as_str().starts_with('\'');
    // 已闭合的引号(`'a'` 之后)不在取值位置
    if quoted && token.as_str().len() > 1 && token.as_str().replace("''", "").ends_with('\'') {
        return None;
    }
    let col_raw = caps.get(2)?.as_str().trim_matches(|c| c == '"' || c == '`');
    if NOT_ALIAS.contains(&col_raw.to_ascii_lowercase().as_str()) {
        return None;
    }
    let stmt = current_statement(text, offset);
    let refs = table_refs(&stmt, columns);
    let has_col = |t: &str| columns.get(t).and_then(|cs| cs.iter().find(|c| c.name.eq_ignore_ascii_case(col_raw)).map(|c| c.name.clone()));
    let (table, column) = match caps.get(1) {
        Some(q) => {
            let q = q.as_str();
            let t = refs
                .iter()
                .find(|(t, a)| a.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(q)) || t.eq_ignore_ascii_case(q))
                .map(|(t, _)| t.clone())?;
            let c = has_col(&t)?;
            (t, c)
        }
        None => refs.iter().find_map(|(t, _)| has_col(t).map(|c| (t.clone(), c)))?,
    };
    let start = stmt_start + token.start();
    let prefix = token.as_str().trim_start_matches('\'').replace("''", "'");
    Some(ValueContext { table, column, replace: start..offset, prefix, quoted })
}

/// 常用 Redis 命令:(名称, 签名, 首参是否为 key)。
const REDIS_COMMANDS: &[(&str, &str, bool)] = &[
    ("GET", "key", true),
    ("SET", "key value [EX seconds] [NX|XX]", true),
    ("DEL", "key [key ...]", true),
    ("UNLINK", "key [key ...]", true),
    ("EXISTS", "key [key ...]", true),
    ("EXPIRE", "key seconds", true),
    ("TTL", "key", true),
    ("PTTL", "key", true),
    ("PERSIST", "key", true),
    ("TYPE", "key", true),
    ("RENAME", "key newkey", true),
    ("INCR", "key", true),
    ("DECR", "key", true),
    ("INCRBY", "key increment", true),
    ("DECRBY", "key decrement", true),
    ("APPEND", "key value", true),
    ("STRLEN", "key", true),
    ("SETNX", "key value", true),
    ("SETEX", "key seconds value", true),
    ("GETRANGE", "key start end", true),
    ("MGET", "key [key ...]", true),
    ("HGET", "key field", true),
    ("HSET", "key field value [field value ...]", true),
    ("HGETALL", "key", true),
    ("HDEL", "key field [field ...]", true),
    ("HEXISTS", "key field", true),
    ("HKEYS", "key", true),
    ("HVALS", "key", true),
    ("HLEN", "key", true),
    ("HMGET", "key field [field ...]", true),
    ("HINCRBY", "key field increment", true),
    ("HSCAN", "key cursor [MATCH pattern] [COUNT count]", true),
    ("LPUSH", "key element [element ...]", true),
    ("RPUSH", "key element [element ...]", true),
    ("LPOP", "key [count]", true),
    ("RPOP", "key [count]", true),
    ("LRANGE", "key start stop", true),
    ("LLEN", "key", true),
    ("LINDEX", "key index", true),
    ("LSET", "key index element", true),
    ("LREM", "key count element", true),
    ("LTRIM", "key start stop", true),
    ("SADD", "key member [member ...]", true),
    ("SREM", "key member [member ...]", true),
    ("SMEMBERS", "key", true),
    ("SISMEMBER", "key member", true),
    ("SCARD", "key", true),
    ("SPOP", "key [count]", true),
    ("SRANDMEMBER", "key [count]", true),
    ("SUNION", "key [key ...]", true),
    ("SINTER", "key [key ...]", true),
    ("SDIFF", "key [key ...]", true),
    ("SSCAN", "key cursor [MATCH pattern] [COUNT count]", true),
    ("ZADD", "key score member [score member ...]", true),
    ("ZREM", "key member [member ...]", true),
    ("ZRANGE", "key start stop [WITHSCORES]", true),
    ("ZREVRANGE", "key start stop [WITHSCORES]", true),
    ("ZRANGEBYSCORE", "key min max [WITHSCORES]", true),
    ("ZSCORE", "key member", true),
    ("ZCARD", "key", true),
    ("ZINCRBY", "key increment member", true),
    ("ZRANK", "key member", true),
    ("ZREVRANK", "key member", true),
    ("ZSCAN", "key cursor [MATCH pattern] [COUNT count]", true),
    ("KEYS", "pattern", false),
    ("SCAN", "cursor [MATCH pattern] [COUNT count]", false),
    ("RANDOMKEY", "", false),
    ("DBSIZE", "", false),
    ("INFO", "[section]", false),
    ("PING", "[message]", false),
    ("ECHO", "message", false),
    ("SELECT", "index", false),
];

/// Redis 命令补全:一行一条命令。行首 → 命令名;取 key 的命令的第一个参数位 → 已扫描到的 key 名。
pub fn redis_completions(
    text: &str,
    offset: usize,
    keys: &[String],
    explicit: bool,
) -> Option<Completions> {
    let offset = offset.min(text.len());
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let before = &text[line_start..offset];
    let token_start = before.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    let token = &before[token_start..];
    let leading = &before[..token_start];
    let replace = line_start + token_start..offset;

    if leading.trim().is_empty() {
        if token.is_empty() && !explicit {
            return None;
        }
        let up = token.to_ascii_uppercase();
        let items: Vec<Item> = REDIS_COMMANDS
            .iter()
            .filter(|(n, _, _)| n.starts_with(&up) && *n != up)
            .map(|(n, args, _)| Item {
                label: n.to_string(),
                detail: args.to_string(),
                kind: ItemKind::Command,
                insert: format!("{n} "),
            })
            .collect();
        return (!items.is_empty()).then_some(Completions { replace, items });
    }
    let parts: Vec<&str> = leading.split_whitespace().collect();
    let takes_key = parts.len() == 1
        && REDIS_COMMANDS
            .iter()
            .any(|(n, _, k)| *k && n.eq_ignore_ascii_case(parts[0]));
    if !takes_key || keys.is_empty() {
        return None;
    }
    let items: Vec<Item> = keys
        .iter()
        .filter(|k| k.starts_with(token) && k.as_str() != token)
        .take(200)
        .map(|k| Item {
            label: k.clone(),
            detail: "key".into(),
            kind: ItemKind::Key,
            insert: k.clone(),
        })
        .collect();
    (!items.is_empty()).then_some(Completions { replace, items })
}

/// mongosh 的补全:`db.` 之后补集合名,`db.<集合>.` 之后补方法名。
pub fn mongo_completions(
    text: &str,
    offset: usize,
    collections: &[String],
    explicit: bool,
) -> Option<Completions> {
    const METHODS: [&str; 13] = [
        "find",
        "findOne",
        "countDocuments",
        "distinct",
        "aggregate",
        "getIndexes",
        "insertOne",
        "insertMany",
        "updateOne",
        "updateMany",
        "replaceOne",
        "deleteOne",
        "deleteMany",
    ];
    let before = text.get(..offset.min(text.len()))?;
    let word_start = before
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '-'))
        .map_or(0, |i| i + 1);
    let word = &before[word_start..];
    let head = &before[..word_start];
    let lower = word.to_lowercase();
    let at_db = head.ends_with("db.")
        && head[..head.len() - 3]
            .chars()
            .last()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.'));
    let items: Vec<Item> = if at_db {
        collections
            .iter()
            .filter(|c| c.to_lowercase().starts_with(&lower))
            .map(|c| {
                let full = crate::mongo::parse::coll_ref(c);
                Item {
                    label: c.clone(),
                    detail: String::new(),
                    kind: ItemKind::Table,
                    insert: full.strip_prefix("db.").unwrap_or(&full).to_string(),
                }
            })
            .collect()
    } else if head.ends_with('.') && head.trim_end_matches('.').contains("db.") {
        METHODS
            .iter()
            .filter(|m| m.to_lowercase().starts_with(&lower))
            .map(|m| Item {
                label: m.to_string(),
                detail: String::new(),
                kind: ItemKind::Command,
                insert: m.to_string(),
            })
            .collect()
    } else {
        return None;
    };
    if items.is_empty() && !explicit {
        return None;
    }
    Some(Completions {
        replace: word_start..offset,
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ColumnBrief;

    fn schema() -> (Vec<String>, ColumnsByTable) {
        let mut cols = ColumnsByTable::new();
        cols.insert(
            "users".into(),
            vec![
                ColumnBrief {
                    name: "id".into(),
                    data_type: "int".into(),
                },
                ColumnBrief {
                    name: "email".into(),
                    data_type: "text".into(),
                },
            ],
        );
        cols.insert(
            "orders".into(),
            vec![ColumnBrief {
                name: "user_id".into(),
                data_type: "int".into(),
            }],
        );
        (vec!["users".into(), "orders".into()], cols)
    }

    fn labels(c: &Option<Completions>) -> Vec<String> {
        c.as_ref()
            .map(|c| c.items.iter().map(|i| i.label.clone()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn tables_after_from() {
        let (t, c) = schema();
        let text = "select * from ";
        assert_eq!(
            labels(&sql_completions(text, text.len(), &t, &c, false)),
            vec!["users", "orders"]
        );
    }

    #[test]
    fn alias_dot_gives_columns() {
        let (t, c) = schema();
        let text = "select u. from users u";
        let got = sql_completions(text, 9, &t, &c, false);
        assert_eq!(labels(&got), vec!["id", "email"]);
        assert_eq!(got.unwrap().replace, 9..9);
    }

    #[test]
    fn bare_columns_in_where() {
        let (t, c) = schema();
        let text = "select * from users where em";
        let got = sql_completions(text, text.len(), &t, &c, false).unwrap();
        assert_eq!(got.items[0].label, "email");
        assert_eq!(got.replace, text.len() - 2..text.len());
    }

    #[test]
    fn snippet_after_select() {
        let (t, c) = schema();
        let text = "SELECT ";
        assert_eq!(
            labels(&sql_completions(text, text.len(), &t, &c, false)),
            vec!["* FROM"]
        );
    }

    #[test]
    fn no_completion_inside_string() {
        let (t, c) = schema();
        let text = "select 'us";
        assert!(sql_completions(text, text.len(), &t, &c, false).is_none());
    }

    #[test]
    fn keyword_uppercase_on_boundary() {
        assert_eq!(
            uppercase_keyword_before("select ", 7),
            Some((0..6, "SELECT".into()))
        );
        assert_eq!(uppercase_keyword_before("SELECT ", 7), None);
        assert_eq!(uppercase_keyword_before("'select ", 8), None);
        assert_eq!(uppercase_keyword_before("users ", 6), None);
    }

    #[test]
    fn redis_commands_and_keys() {
        let keys = vec!["user:1".to_string(), "user:2".to_string()];
        assert_eq!(
            labels(&redis_completions("hg", 2, &keys, false)),
            vec!["HGET", "HGETALL"]
        );
        let got = redis_completions("GET us", 6, &keys, false).unwrap();
        assert_eq!(got.items.len(), 2);
        assert_eq!(got.replace, 4..6);
        assert!(redis_completions("KEYS us", 7, &keys, false).is_none());
    }

    #[test]
    fn mongo_completes_collections_after_db_dot() {
        let colls = vec![
            "orders".to_string(),
            "order-items".to_string(),
            "users".to_string(),
        ];
        let text = "db.or";
        let r = mongo_completions(text, text.len(), &colls, false).unwrap();
        assert_eq!(r.replace, 3..5);
        let labels: Vec<_> = r.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["orders", "order-items"]);
        assert_eq!(r.items[1].insert, "getCollection(\"order-items\")");
        assert!(mongo_completions("x = 1", 5, &colls, false).is_none());
        let text = "db.orders.fi";
        let r = mongo_completions(text, text.len(), &colls, false).unwrap();
        assert!(r.items.iter().any(|i| i.label == "find"));
    }

    fn fk(table: &str, column: &str, ref_table: &str, ref_column: &str, constraint: &str, inferred: bool) -> ForeignKey {
        ForeignKey {
            constraint: constraint.into(),
            table: table.into(),
            column: column.into(),
            ref_table: ref_table.into(),
            ref_column: ref_column.into(),
            inferred,
        }
    }

    fn shop() -> (Vec<String>, ColumnsByTable, Vec<ForeignKey>) {
        let mut cols = ColumnsByTable::new();
        let c = |n: &str, t: &str| ColumnBrief { name: n.into(), data_type: t.into() };
        cols.insert("users".into(), vec![c("id", "int"), c("status", "text")]);
        cols.insert("orders".into(), vec![c("id", "int"), c("user_id", "int"), c("status", "text")]);
        cols.insert("items".into(), vec![c("order_id", "int"), c("order_rev", "int"), c("sku", "text")]);
        cols.insert("logs".into(), vec![c("msg", "text")]);
        let fks = vec![
            fk("orders", "user_id", "users", "id", "orders_user_fk", false),
            // 复合外键
            fk("items", "order_id", "orders", "id", "items_order_fk", false),
            fk("items", "order_rev", "orders", "rev", "items_order_fk", false),
            fk("logs", "user_id", "users", "id", "inferred_logs_user_id", true),
        ];
        (vec!["users".into(), "orders".into(), "items".into(), "logs".into()], cols, fks)
    }

    #[test]
    fn join_on_from_foreign_keys_with_aliases() {
        let (t, c, f) = shop();
        let text = "SELECT * FROM users u JOIN orders o ";
        let got = sql_completions_with(text, text.len(), &t, &c, &f, false).unwrap();
        assert_eq!(labels(&Some(got.clone())), vec!["ON o.user_id = u.id"]);
        assert_eq!(got.items[0].detail, "Foreign key orders_user_fk");
        // 已写了 ON:只补条件
        let text = "SELECT * FROM users u JOIN orders o ON ";
        let got = sql_completions_with(text, text.len(), &t, &c, &f, false);
        assert_eq!(labels(&got)[0], "o.user_id = u.id");
        // 正在敲 ON 的开头也给
        let text = "SELECT * FROM users u JOIN orders o o";
        assert_eq!(labels(&sql_completions_with(text, text.len(), &t, &c, &f, false))[0], "ON o.user_id = u.id");
    }

    #[test]
    fn join_on_reverse_direction_composite_and_no_alias() {
        let (t, c, f) = shop();
        // 被引用方后 JOIN;没有别名时用表名
        let text = "select * from items join orders ";
        let got = labels(&sql_completions_with(text, text.len(), &t, &c, &f, false));
        assert_eq!(got, vec!["ON orders.id = items.order_id AND orders.rev = items.order_rev"]);
        // 与左侧没有关联的表:不给 ON 条件
        let text = "select * from logs l join orders o ";
        assert!(!labels(&sql_completions_with(text, text.len(), &t, &c, &f, false)).iter().any(|l| l.starts_with("ON")));
        // 不传外键时行为不变
        let text = "SELECT * FROM users u JOIN orders o ";
        assert!(!labels(&sql_completions(text, text.len(), &t, &c, false)).iter().any(|l| l.starts_with("ON")));
    }

    #[test]
    fn join_table_position_ranks_related_tables_first() {
        let (t, c, f) = shop();
        let text = "select * from users u join ";
        let got = sql_completions_with(text, text.len(), &t, &c, &f, false).unwrap();
        let l: Vec<&str> = got.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(&l[..2], &["orders", "logs"]);
        assert_eq!(got.items[0].detail, "Table · related to users");
    }

    #[test]
    fn value_context_positions() {
        let (_, c, _) = shop();
        let text = "select * from orders o where o.status = 'pa";
        let v = value_context(text, text.len(), &c).unwrap();
        assert_eq!((v.table.as_str(), v.column.as_str(), v.prefix.as_str(), v.quoted), ("orders", "status", "pa", true));
        assert_eq!(&text[v.replace.clone()], "'pa");
        // 裸列名 + 刚打完 = 与空格
        let text = "SELECT * FROM users WHERE status = ";
        let v = value_context(text, text.len(), &c).unwrap();
        assert_eq!((v.column.as_str(), v.prefix.as_str(), v.quoted, v.replace.len()), ("status", "", false, 0));
        // IN 列表的后续项
        let text = "select * from users where status in ('active', '";
        assert_eq!(value_context(text, text.len(), &c).unwrap().prefix, "");
        // 引号已闭合、未知列、注释里:不算
        assert!(value_context("select * from users where status = 'a' ", 39, &c).is_none());
        assert!(value_context("select * from users where nope = '", 34, &c).is_none());
        assert!(value_context("select 1 -- where status = '", 28, &c).is_none());
    }
}
