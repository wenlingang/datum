//! 编辑器补全的纯逻辑:给定文本、光标(字节偏移)与 schema,算出候选项与替换范围。
//! UI 层把结果转换成编辑器的补全菜单项。

use crate::schema::ColumnsByTable;
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

    // 表名位置:FROM/JOIN/INTO/UPDATE 后立即弹出
    if EAGER_TABLE.is_match(tail) {
        let items = filter_prefix(table_items(tables), prefix);
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
}
