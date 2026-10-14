//! 破坏性语句识别:执行前审查的检测层(纯函数,UI 无关)。
//!
//! `Danger` = 不可逆的数据/结构毁灭(DROP、TRUNCATE、无 WHERE 的 DELETE/UPDATE);
//! `Warning` = 结构性变更但通常可控(ALTER … DROP/RENAME、DROP INDEX 等)。

use regex::Regex;
use rust_i18n::t;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    Danger,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskFinding {
    /// 语句原文(trim 后),审查弹窗里展示
    pub sql: String,
    pub level: RiskLevel,
    /// 操作类型标签,如 "DROP TABLE"、"DELETE 无 WHERE"
    pub kind: String,
    /// 受影响对象名(取最后一段、已去引号);解析不出时为 None
    pub object: Option<String>,
}

/// 词法规则。三种数据库对字符串、注释的解析不同(反斜杠转义、`#` 注释、`$$` 字符串、
/// 可执行注释……),只按一种规则分析会被另一种的写法绕过:分析器以为在字符串/注释里,
/// 数据库却当代码执行。所以每种规则都完整分析一遍,结果取并集。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rules {
    /// 单引号 '' 转义,E'' 字符串支持反斜杠,`$tag$` 字符串,`--` 注释
    Pg,
    /// 单/双引号均为字符串且支持反斜杠转义,`#` 与 `-- `(后跟空白)为注释,`/*! */` 内容会执行
    MySql,
    /// 单引号 '' 转义,`--` 注释,`[...]` 也是标识符引号
    Sqlite,
    /// PG 关闭 standard_conforming_strings 时:普通字符串里的反斜杠也是转义
    PgLegacy,
}

const ALL_RULES: [Rules; 4] = [Rules::Pg, Rules::MySql, Rules::Sqlite, Rules::PgLegacy];

/// 一条语句:原文范围 + 去掉字符串内容与注释后的文本(供关键字匹配)。
struct Lexed {
    range: std::ops::Range<usize>,
    clean: String,
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 按给定规则把整段 SQL 切成语句(只在顶层 `;` 处切),同时生成去噪文本。
fn lex(doc: &str, rules: Rules) -> Vec<Lexed> {
    let chars: Vec<(usize, char)> = doc.char_indices().collect();
    let at = |k: usize| chars.get(k).map(|&(_, c)| c);
    let mut out = Vec::new();
    let mut clean = String::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut in_exec_comment = false;
    while i < chars.len() {
        let (pos, ch) = chars[i];
        let prev = if i > 0 { at(i - 1) } else { None };
        match ch {
            ';' => {
                out.push(Lexed {
                    range: start..pos,
                    clean: std::mem::take(&mut clean),
                });
                start = pos + 1;
                i += 1;
            }
            '\'' | '"' if ch == '\'' || rules == Rules::MySql => {
                // 字符串:内容丢弃。MySQL 与 PG 的 E'' 支持反斜杠转义
                let backslash = rules == Rules::MySql
                    || rules == Rules::PgLegacy
                    || (ch == '\''
                        && matches!(prev, Some('e' | 'E'))
                        && !(i >= 2 && at(i - 2).is_some_and(is_ident_char)));
                i += 1;
                while i < chars.len() {
                    let c = chars[i].1;
                    if backslash && c == '\\' {
                        i += 2;
                        continue;
                    }
                    if c == ch {
                        if at(i + 1) == Some(ch) {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                clean.push(' ');
            }
            '[' if rules == Rules::Sqlite => {
                // SQLite 的 [标识符]:到 ] 为止,内部的引号和 ; 都不算
                while i < chars.len() {
                    let c = chars[i].1;
                    clean.push(c);
                    i += 1;
                    if c == ']' {
                        break;
                    }
                }
            }
            '"' | '`' => {
                // 带引号的标识符:保留原文(对象名展示要用),内部的 ; 不切分
                clean.push(ch);
                i += 1;
                while i < chars.len() {
                    let c = chars[i].1;
                    clean.push(c);
                    i += 1;
                    if c == ch {
                        if at(i) == Some(ch) {
                            clean.push(ch);
                            i += 1;
                            continue;
                        }
                        break;
                    }
                }
            }
            '-' if at(i + 1) == Some('-')
                && (rules != Rules::MySql
                    || at(i + 2).is_none_or(|c| c.is_whitespace() || c.is_control())) =>
            {
                while i < chars.len() && chars[i].1 != '\n' {
                    i += 1;
                }
                clean.push(' ');
            }
            '#' if rules == Rules::MySql => {
                while i < chars.len() && chars[i].1 != '\n' {
                    i += 1;
                }
                clean.push(' ');
            }
            '*' if in_exec_comment && at(i + 1) == Some('/') => {
                in_exec_comment = false;
                i += 2;
                clean.push(' ');
            }
            '/' if at(i + 1) == Some('*') => {
                // 可执行注释:MySQL 的 `/*!`、MariaDB 的 `/*M!`,跳过标记与版本号,内容照常分析
                let mariadb = at(i + 2) == Some('M') && at(i + 3) == Some('!');
                if rules == Rules::MySql && (at(i + 2) == Some('!') || mariadb) {
                    i += if mariadb { 4 } else { 3 };
                    while at(i).is_some_and(|c| c.is_ascii_digit()) {
                        i += 1;
                    }
                    in_exec_comment = true;
                } else {
                    i += 2;
                    while i < chars.len() && !(chars[i].1 == '*' && at(i + 1) == Some('/')) {
                        i += 1;
                    }
                    i += 2;
                }
                clean.push(' ');
            }
            '$' if matches!(rules, Rules::Pg | Rules::PgLegacy)
                && !prev.is_some_and(is_ident_char) =>
            {
                // PG 美元引号 $tag$ … $tag$
                let mut k = i + 1;
                while at(k).is_some_and(is_ident_char) {
                    k += 1;
                }
                let tag_ok = at(k) == Some('$') && !at(i + 1).is_some_and(|c| c.is_ascii_digit());
                if tag_ok {
                    let tag: String = chars[i..=k].iter().map(|&(_, c)| c).collect();
                    let body_from = chars.get(k + 1).map_or(doc.len(), |&(p, _)| p);
                    match doc[body_from..].find(&tag) {
                        Some(off) => {
                            let end = body_from + off + tag.len();
                            while i < chars.len() && chars[i].0 < end {
                                i += 1;
                            }
                        }
                        None => i = chars.len(),
                    }
                    clean.push(' ');
                } else {
                    clean.push(ch);
                    i += 1;
                }
            }
            _ => {
                clean.push(ch);
                i += 1;
            }
        }
    }
    if start < doc.len() || !clean.trim().is_empty() {
        out.push(Lexed {
            range: start..doc.len(),
            clean,
        });
    }
    out
}

static SEGMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^(?:`([^`]+)`|"([^"]+)"|([\w$]+))"#).unwrap());

/// 从 `text` 开头读一个(可能带库名前缀的)标识符,返回最后一段的去引号形式。
fn read_object_name(text: &str) -> Option<String> {
    let mut s = text.trim_start();
    let mut last = None;
    while let Some(caps) = SEGMENT.captures(s) {
        let seg = caps
            .get(1)
            .or(caps.get(2))
            .or(caps.get(3))
            .unwrap()
            .as_str();
        last = Some(seg.to_string());
        s = &s[caps.get(0).unwrap().end()..];
        match s.strip_prefix('.') {
            Some(rest) => s = rest,
            None => break,
        }
    }
    last
}

fn object_after(text: &str, re: &Regex) -> Option<String> {
    re.find(text)
        .and_then(|m| read_object_name(&text[m.end()..]))
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap()
}

static DROP_WHAT: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)^drop\s+(?:materialized\s+)?(table|database|schema|view|index|trigger|sequence|function|procedure|type)\b",
    )
});
static DROP_ANY: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^drop\b"));
static TRUNCATE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^truncate\b"));
static TRUNCATE_OBJ: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^truncate\s+(?:table\s+)?(?:only\s+)?(?:if\s+exists\s+)?"));
static DELETE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^delete\b"));
static UPDATE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^update\b"));
static WHERE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bwhere\b"));
static DELETE_OBJ: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^delete\s+(?:low_priority\s+|quick\s+|ignore\s+)*from\s+(?:only\s+)?")
});
static UPDATE_OBJ: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^update\s+(?:only\s+|low_priority\s+|ignore\s+)*"));
static ALTER_TABLE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^alter\s+table\s+(?:if\s+exists\s+)?(?:only\s+)?"));
static DROP_WORD: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bdrop\b"));
static RENAME_WORD: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\brename\b"));
static WITH: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^with\b"));
static EXPLAIN_ANALYZE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^explain\b[^;]*\banalyze\b"));
static DML_WORD: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(insert|update|delete|merge)\b"));
static PROCEDURAL: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(do|call|exec|execute)\b"));

/// 分析单条语句(已去噪),命中返回 (level, kind, object)。
fn classify(clean: &str) -> Option<(RiskLevel, String, Option<String>)> {
    let s = clean.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(caps) = DROP_WHAT.captures(s) {
        let what_raw = caps.get(1).unwrap().as_str();
        let what = what_raw.to_ascii_uppercase();
        let danger = matches!(what.as_str(), "TABLE" | "DATABASE" | "SCHEMA" | "VIEW");
        let obj_re = re(&format!(
            r"(?i)^drop\s+(?:materialized\s+)?{what_raw}\s+(?:if\s+exists\s+)?(?:concurrently\s+)?"
        ));
        let level = if danger {
            RiskLevel::Danger
        } else {
            RiskLevel::Warning
        };
        return Some((level, format!("DROP {what}"), object_after(s, &obj_re)));
    }
    if DROP_ANY.is_match(s) {
        return Some((RiskLevel::Warning, "DROP".into(), None));
    }
    if TRUNCATE.is_match(s) {
        return Some((
            RiskLevel::Danger,
            "TRUNCATE".into(),
            object_after(s, &TRUNCATE_OBJ),
        ));
    }
    if DELETE.is_match(s) && !WHERE.is_match(s) {
        return Some((
            RiskLevel::Danger,
            t!("core.risk.delete_no_where").to_string(),
            object_after(s, &DELETE_OBJ),
        ));
    }
    if UPDATE.is_match(s) && !WHERE.is_match(s) {
        return Some((
            RiskLevel::Danger,
            t!("core.risk.update_no_where").to_string(),
            object_after(s, &UPDATE_OBJ),
        ));
    }
    // PG 数据修改 CTE:`WITH x AS (DELETE …) SELECT …` 开头不是 DELETE 也会改数据
    if WITH.is_match(s) && DML_WORD.is_match(s) {
        return Some((
            RiskLevel::Warning,
            t!("core.risk.with_dml").to_string(),
            None,
        ));
    }
    // EXPLAIN ANALYZE 会真正执行被分析的语句
    if EXPLAIN_ANALYZE.is_match(s) && DML_WORD.is_match(s) {
        return Some((
            RiskLevel::Warning,
            t!("core.risk.explain_analyze_write").to_string(),
            None,
        ));
    }
    // 匿名块 / 存储过程:内部做什么无法静态判断
    if let Some(caps) = PROCEDURAL.captures(s) {
        return Some((
            RiskLevel::Warning,
            t!("core.risk.procedural", kind = caps[1].to_ascii_uppercase()).to_string(),
            None,
        ));
    }
    if let Some(m) = ALTER_TABLE.find(s) {
        let object = read_object_name(&s[m.end()..]);
        if DROP_WORD.is_match(s) {
            return Some((RiskLevel::Warning, "ALTER TABLE … DROP".into(), object));
        }
        if RENAME_WORD.is_match(s) {
            return Some((RiskLevel::Warning, "ALTER TABLE … RENAME".into(), object));
        }
    }
    None
}

/// 分析一段 SQL(可多语句),返回全部破坏性命中(按出现顺序)。
/// 按三种数据库的词法规则各分析一遍取并集:任一规则下危险就提示。
pub fn analyze_sql(doc: &str) -> Vec<RiskFinding> {
    let mut found: Vec<(usize, RiskFinding)> = Vec::new();
    for rules in ALL_RULES {
        for l in lex(doc, rules) {
            let raw = doc[l.range.clone()].trim();
            if raw.is_empty() {
                continue;
            }
            if let Some((level, kind, object)) = classify(&l.clean) {
                let f = RiskFinding {
                    sql: raw.to_string(),
                    level,
                    kind,
                    object,
                };
                if !found
                    .iter()
                    .any(|(_, g)| g.sql == f.sql && g.kind == f.kind)
                {
                    found.push((l.range.start, f));
                }
            }
        }
    }
    found.sort_by_key(|(start, _)| *start);
    found.into_iter().map(|(_, f)| f).collect()
}

/// 是否能确定整段 SQL 都是只读语句。用于「免确认直接执行」的放行判断:
/// 黑名单(analyze_sql)漏判的写法很多,放行只认白名单——开头是只读语句,
/// 且全文(去掉字符串与注释后)不含任何写操作关键字;并且三种词法规则下都必须成立。
/// 宁可误判为非只读而多问一次。
pub fn is_read_only(doc: &str) -> bool {
    // 各数据库解析不一致的写法(反斜杠转义、# / -- / /* 注释、$$ 字符串、[ ] 与 " ` 引号标识符)
    // 是绕过检测的根源。免确认只给最朴素的语句:出现任何这类字符就要求确认,不再逐个推敲规则。
    // 单引号字符串仍然允许,但其中不能含上述字符(反斜杠会改变字符串在 MySQL 里的结束位置)。
    if has_dialect_sensitive_syntax(doc) {
        return false;
    }
    static READ_HEAD: LazyLock<Regex> = LazyLock::new(|| {
        re(r"(?i)^(select|show|explain|desc|describe|values|table|with|pragma)\b")
    });
    static WRITE_WORD: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"(?i)\b(insert|update|delete|merge|replace|upsert|drop|alter|truncate|create|",
            r"rename|grant|revoke|call|do|exec|execute|copy|load|lock|unlock|vacuum|reindex|",
            r"cluster|refresh|comment|set|reset|into|outfile|dumpfile|nextval|setval|",
            r"pg_terminate_backend|pg_cancel_backend|lo_unlink|dblink_exec|sleep|pg_sleep|",
            r"benchmark|kill|shutdown|flush|purge|handler|attach|detach)\b"
        ))
    });
    ALL_RULES.iter().all(|&rules| {
        let stmts: Vec<String> = lex(doc, rules)
            .into_iter()
            .map(|l| l.clean.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        !stmts.is_empty()
            && stmts.iter().all(|s| {
                let is_pragma = s.to_ascii_lowercase().starts_with("pragma");
                READ_HEAD.is_match(s)
                    && !WRITE_WORD.is_match(s)
                    && pragma_is_read(s)
                    // PRAGMA 的括号是参数,已由 pragma_is_read 按白名单把关
                    && (is_pragma || calls_only_safe_functions(s))
            })
    })
}

/// 是否含有在不同数据库里解析结果可能不同的写法。
fn has_dialect_sensitive_syntax(doc: &str) -> bool {
    doc.contains(['\\', '#', '$', '[', ']', '"', '`'])
        || doc.contains("--")
        || doc.contains("/*")
        || doc.contains("*/")
}

/// PRAGMA 只放行读取类;`PRAGMA journal_mode(WAL)` 这类括号写法也会改设置,不能只看 `=`。
fn pragma_is_read(s: &str) -> bool {
    static PRAGMA: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?is)^pragma\s+(?:[\w$]+\.)?([\w$]+)\s*(.*)$"));
    const READ_PRAGMAS: &[&str] = &[
        "table_info",
        "table_xinfo",
        "table_list",
        "index_list",
        "index_info",
        "index_xinfo",
        "foreign_key_list",
        "database_list",
        "collation_list",
        "function_list",
        "module_list",
        "pragma_list",
        "compile_options",
    ];
    match PRAGMA.captures(s) {
        Some(caps) => {
            READ_PRAGMAS.contains(&caps[1].to_ascii_lowercase().as_str()) && !caps[2].contains('=')
        }
        // 不是 PRAGMA 语句(或无法识别的 PRAGMA 写法)
        None => !s.to_ascii_lowercase().starts_with("pragma"),
    }
}

/// 函数调用也走白名单:SELECT 里调用的函数可能有副作用(set_config、pg_notify、
/// load_extension、GET_LOCK、用户自定义函数……),只放行常见的内置纯函数。
fn calls_only_safe_functions(s: &str) -> bool {
    // 函数名可以带 schema 前缀,也可以加引号("set_config" / `f`),都要识别出来
    static CALL: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r#"((?:(?:[\w$]+|"(?:[^"]|"")*"|`[^`]*`)\s*\.\s*)*)"#,
            r#"([\w$]+|"(?:[^"]|"")*"|`[^`]*`)\s*\("#
        ))
    });
    // 后面能跟括号、但不是函数调用的关键字与类型名
    const KEYWORDS: &[&str] = &[
        "in",
        "exists",
        "values",
        "as",
        "over",
        "filter",
        "within",
        "using",
        "on",
        "and",
        "or",
        "not",
        "select",
        "from",
        "where",
        "join",
        "any",
        "all",
        "some",
        "case",
        "when",
        "then",
        "else",
        "lateral",
        "partition",
        "by",
        "rows",
        "range",
        "between",
        "interval",
        "array",
        "row",
        "distinct",
        "union",
        "intersect",
        "except",
        "group",
        "order",
        "having",
        "limit",
        "offset",
        "is",
        "like",
        "ilike",
        "tablesample",
        "cube",
        "rollup",
        "grouping",
        "sets",
        "with",
        "recursive",
        "table",
        "explain",
        "key",
        "char",
        "varchar",
        "numeric",
        "decimal",
        "float",
        "double",
        "timestamp",
        "time",
        "precision",
    ];
    const SAFE_FUNCTIONS: &[&str] = &[
        // 聚合与窗口
        "count",
        "sum",
        "avg",
        "min",
        "max",
        "array_agg",
        "string_agg",
        "group_concat",
        "json_agg",
        "jsonb_agg",
        "json_object_agg",
        "jsonb_object_agg",
        "bool_and",
        "bool_or",
        "every",
        "stddev",
        "stddev_pop",
        "stddev_samp",
        "variance",
        "var_pop",
        "var_samp",
        "percentile_cont",
        "percentile_disc",
        "mode",
        "row_number",
        "rank",
        "dense_rank",
        "lag",
        "lead",
        "first_value",
        "last_value",
        "nth_value",
        "ntile",
        "cume_dist",
        "percent_rank",
        // 条件与数学
        "coalesce",
        "nullif",
        "greatest",
        "least",
        "ifnull",
        "isnull",
        "nvl",
        "if",
        "iif",
        "abs",
        "ceil",
        "ceiling",
        "floor",
        "round",
        "trunc",
        "truncate",
        "mod",
        "power",
        "pow",
        "sqrt",
        "exp",
        "ln",
        "log",
        "log10",
        "sign",
        "random",
        "rand",
        // 字符串
        "lower",
        "upper",
        "length",
        "char_length",
        "character_length",
        "octet_length",
        "substring",
        "substr",
        "trim",
        "ltrim",
        "rtrim",
        "btrim",
        "concat",
        "concat_ws",
        "left",
        "right",
        "lpad",
        "rpad",
        "position",
        "strpos",
        "instr",
        "locate",
        "split_part",
        "regexp_replace",
        "regexp_matches",
        "regexp_substr",
        "regexp_like",
        "reverse",
        "repeat",
        "initcap",
        "md5",
        "format",
        "quote_ident",
        "quote_literal",
        "hex",
        "unhex",
        "encode",
        "decode",
        "to_hex",
        "ascii",
        "chr",
        "starts_with",
        // 日期时间
        "now",
        "to_char",
        "to_date",
        "to_timestamp",
        "to_number",
        "date_trunc",
        "date_part",
        "extract",
        "age",
        "date_format",
        "date_add",
        "date_sub",
        "datediff",
        "timestampdiff",
        "year",
        "month",
        "day",
        "hour",
        "minute",
        "second",
        "dayofweek",
        "unix_timestamp",
        "from_unixtime",
        "strftime",
        "datetime",
        "date",
        "julianday",
        "make_date",
        "make_timestamp",
        "current_timestamp",
        "localtimestamp",
        // 类型与 JSON
        "cast",
        "convert",
        "typeof",
        "pg_typeof",
        "json_extract",
        "json_unquote",
        "json_object",
        "json_array",
        "json_build_object",
        "jsonb_build_object",
        "json_build_array",
        "jsonb_build_array",
        "to_json",
        "to_jsonb",
        "json_array_length",
        "jsonb_array_length",
        "json_array_elements",
        "jsonb_array_elements",
        "json_array_elements_text",
        "jsonb_array_elements_text",
        "json_each",
        "jsonb_each",
        "json_each_text",
        "jsonb_each_text",
        "json_object_keys",
        "jsonb_object_keys",
        "jsonb_typeof",
        "json_typeof",
        "jsonb_path_query",
        "json_valid",
        // 集合与元信息
        "unnest",
        "generate_series",
        "array_length",
        "cardinality",
        "array_to_string",
        "string_to_array",
        "version",
        "pg_size_pretty",
        "pg_total_relation_size",
        "pg_relation_size",
        "pg_table_size",
        "pg_indexes_size",
        "pg_database_size",
        "current_database",
        "current_schema",
        "database",
        "schema",
    ];
    CALL.captures_iter(s).all(|c| {
        let qualifier = c[1]
            .trim()
            .trim_end_matches('.')
            .trim()
            .to_ascii_lowercase();
        let name = &c[2];
        // 加引号的函数名可能是任何函数(PG "set_config"、MySQL `f`),一律不放行
        if name.starts_with('"') || name.starts_with('`') {
            return false;
        }
        // 带前缀时只认 PG 内置函数所在的 pg_catalog;其他 schema 里的同名函数可能是自定义的
        if !qualifier.is_empty() && qualifier != "pg_catalog" {
            return false;
        }
        let name = name.to_ascii_lowercase();
        KEYWORDS.contains(&name.as_str()) || SAFE_FUNCTIONS.contains(&name.as_str())
    })
}

/// Redis 命令的破坏性检测:FLUSHALL / FLUSHDB 整库清空。
pub fn analyze_redis(cmd: &str) -> Vec<RiskFinding> {
    static FLUSH: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(flushall|flushdb)\b"));
    let s = cmd.trim();
    match FLUSH.captures(s) {
        Some(caps) => vec![RiskFinding {
            sql: s.to_string(),
            level: RiskLevel::Danger,
            kind: caps[1].to_ascii_uppercase(),
            object: None,
        }],
        None => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(sql: &str) -> RiskFinding {
        let f = analyze_sql(sql);
        assert_eq!(f.len(), 1, "{sql}");
        f.into_iter().next().unwrap()
    }

    #[test]
    fn drop_table_is_danger_with_object() {
        let f = one("DROP TABLE IF EXISTS public.\"Users\"");
        assert_eq!(f.level, RiskLevel::Danger);
        assert_eq!(f.kind, "DROP TABLE");
        assert_eq!(f.object.as_deref(), Some("Users"));
    }

    #[test]
    fn drop_index_is_warning() {
        let f = one("drop index concurrently idx_a");
        assert_eq!(f.level, RiskLevel::Warning);
        assert_eq!(f.object.as_deref(), Some("idx_a"));
    }

    #[test]
    fn delete_and_update_without_where() {
        assert_eq!(
            one("delete from `orders`").object.as_deref(),
            Some("orders")
        );
        assert_eq!(one("UPDATE users SET a = 1").kind, "UPDATE without WHERE");
        assert!(analyze_sql("delete from t where id = 1").is_empty());
    }

    #[test]
    fn where_inside_string_or_comment_does_not_count() {
        assert_eq!(analyze_sql("delete from t -- where id = 1").len(), 1);
        assert_eq!(analyze_sql("update t set note = 'where'").len(), 1);
    }

    #[test]
    fn truncate_and_alter() {
        assert_eq!(one("TRUNCATE TABLE logs").object.as_deref(), Some("logs"));
        let f = one("alter table t drop column c");
        assert_eq!(f.kind, "ALTER TABLE … DROP");
        assert!(analyze_sql("alter table t add column c int").is_empty());
    }

    #[test]
    fn multiple_statements_reported_in_order() {
        let f = analyze_sql("select 1; drop table a; truncate b");
        assert_eq!(
            f.iter().map(|x| x.kind.as_str()).collect::<Vec<_>>(),
            vec!["DROP TABLE", "TRUNCATE"]
        );
    }

    /// 以前能绕过检测的写法
    #[test]
    fn previously_bypassed_forms_are_caught() {
        for sql in [
            "WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x",
            "# 说明\nDROP TABLE t",
            "/*!50000 DROP TABLE t */",
            "DO $$ BEGIN DELETE FROM t; END $$",
            "CALL purge_all()",
            "EXPLAIN ANALYZE DELETE FROM t WHERE id = 1",
        ] {
            // 不同词法规则可能把同一段切成不同语句,各报一次;至少要报出来
            assert!(!analyze_sql(sql).is_empty(), "{sql}");
        }
        // 普通注释仍被忽略
        assert!(analyze_sql("/* drop table t */ select 1").is_empty());
        // PG 的 # 是运算符:句中的 # 不能把后面的语句藏起来
        assert_eq!(analyze_sql("select 1 # 2; drop table t").len(), 1);
    }

    /// 各数据库词法差异造成的绕过:分析器以为在字符串/注释里,数据库却会执行。
    #[test]
    fn dialect_lexing_differentials_are_caught() {
        for sql in [
            // MySQL:\' 是转义,字符串在 \'' 处结束,后面的 DROP 会执行
            "SELECT 'a\\'' ; DROP TABLE t; -- '",
            // PG:$$…$$ 是字符串,其中的 ' 不开启字符串
            "SELECT $$'$$; DROP TABLE t; --'",
            "SELECT $tag$ ' $tag$; DROP TABLE t; --'",
            // MySQL:# 注释掉了单引号
            "SELECT 1 #'\n; DROP TABLE t; -- '",
            // 块注释里的引号
            "/* ' */; DROP TABLE t; -- '",
            // MySQL:--x 不是注释
            "SELECT 1 --'\n; DROP TABLE t; -- '",
            // PG 的 E'' 支持反斜杠
            "SELECT E'\\''; DROP TABLE t; --'",
            // SQLite 的 [标识符] 里的单引号
            "SELECT [']; DROP TABLE t; --'",
            // MariaDB 的可执行注释
            "/*M!100100 DROP TABLE t */",
            // PG 关闭 standard_conforming_strings 时普通字符串的反斜杠转义,配合 # 运算符
            "SELECT 'a\\'' # '\n; DROP TABLE t; -- '",
        ] {
            assert!(
                analyze_sql(sql).iter().any(|f| f.kind.starts_with("DROP")),
                "未识别: {sql}"
            );
            assert!(!is_read_only(sql), "误判为只读: {sql}");
        }
    }

    /// 合法的引号、注释写法不应误报。
    #[test]
    fn quoting_does_not_cause_false_positives() {
        for sql in [
            "SELECT 'it''s; drop table t' FROM a",
            "SELECT \"drop;table\" FROM a",
            "SELECT `x;drop table t` FROM a",
            "SELECT 1 -- ; drop table t",
            "SELECT /* ; drop table t */ 1",
        ] {
            assert!(analyze_sql(sql).is_empty(), "误报: {sql}");
        }
        assert!(is_read_only("SELECT 'it''s' FROM t; SELECT 2"));
    }

    #[test]
    fn read_only_whitelist() {
        for sql in [
            "SELECT count(*), max(id), lower(name) FROM t WHERE id IN (1, 2) AND EXISTS (SELECT 1)",
            "SELECT date_trunc('day', created_at) d, row_number() OVER (PARTITION BY a ORDER BY b) FROM t",
            "SELECT CAST(x AS varchar(10)), coalesce(a, 0) FROM t",
            "SELECT pg_catalog.lower(name) FROM t",
            "SELECT id, updated_at FROM t WHERE note = 'delete me'",
            "WITH a AS (SELECT 1) SELECT * FROM a",
            "EXPLAIN SELECT 1",
            "SHOW TABLES; DESC t",
            "PRAGMA table_info(t)",
        ] {
            assert!(is_read_only(sql), "{sql}");
        }
        for sql in [
            "",
            // 含方言敏感写法的一律要求确认
            "SELECT 1 -- note",
            "SELECT /* c */ 1",
            "SELECT 'a\\b'",
            "SELECT \"col\" FROM t",
            "SELECT `col` FROM t",
            "SELECT $$x$$",
            "SELECT [col] FROM t",
            "SELECT 1 # note",
            // 有副作用的函数与设置类 PRAGMA
            "SELECT set_config('search_path', 'x', false)",
            "SELECT pg_notify('ch', 'hi')",
            "SELECT my_cleanup_func()",
            "SELECT public.do_stuff(1)",
            "SELECT load_extension('/tmp/x')",
            "SELECT GET_LOCK('a', 10)",
            // 加引号或带 schema 前缀的函数名
            "SELECT \"set_config\"('search_path', 'x', false)",
            "SELECT `get_lock`('a', 10)",
            "SELECT myschema.lower(name) FROM t",
            "SELECT \"public\".\"count\"(1)",
            "SELECT set_config /* c */ ('a', 'b', false)",
            "PRAGMA journal_mode(WAL)",
            "PRAGMA foreign_keys",
            "DELETE FROM t WHERE id = 1",
            "WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x",
            "SELECT * INTO t2 FROM t",
            "SELECT * FROM t FOR UPDATE",
            "SELECT 1; UPDATE t SET a = 1 WHERE id = 1",
            "/*!50000 DROP TABLE t */ SELECT 1",
            "SELECT pg_terminate_backend(123)",
            "SELECT nextval('s')",
            "PRAGMA journal_mode = WAL",
            "EXPLAIN ANALYZE DELETE FROM t WHERE id = 1",
            "INSERT INTO t VALUES (1)",
        ] {
            assert!(!is_read_only(sql), "{sql}");
        }
    }

    #[test]
    fn redis_flush() {
        assert_eq!(analyze_redis("flushdb")[0].kind, "FLUSHDB");
        assert!(analyze_redis("get flushall").is_empty());
    }
}
