//! 由表结构或结果行合成 SQL 文本(建表语句、INSERT 模板、逐行 INSERT)。

use super::Dialect;
use crate::pg::browse::TableDetail;
use regex::Regex;
use std::sync::LazyLock;

/// 标识符引号:pg/sqlite 用双引号,mysql 用反引号。
pub fn ident(s: &str, d: Dialect) -> String {
    match d {
        Dialect::Mysql => format!("`{}`", s.replace('`', "``")),
        _ => format!("\"{}\"", s.replace('"', "\"\"")),
    }
}

/// 字符串字面量:mysql 还需转义反斜杠(默认开启反斜杠转义)。
pub fn literal(v: &str, d: Dialect) -> String {
    let escaped = match d {
        Dialect::Mysql => v.replace('\\', "\\\\").replace('\'', "''"),
        _ => v.replace('\'', "''"),
    };
    format!("'{escaped}'")
}

fn split_cols(cols: &str) -> impl Iterator<Item = &str> {
    cols.split(',').map(str::trim).filter(|c| !c.is_empty())
}

/// 由表详情合成 CREATE TABLE(PG / SQLite;MySQL 用 SHOW CREATE TABLE)。
pub fn build_create_table(table: &str, detail: &TableDetail) -> String {
    let q = |s: &str| ident(s, Dialect::Pg);
    let mut lines: Vec<String> = detail
        .columns
        .iter()
        .map(|c| {
            let mut line = format!("  {} {}", q(&c.name), c.data_type);
            if c.not_null {
                line.push_str(" NOT NULL");
            }
            if let Some(def) = c.default.as_deref().filter(|d| !d.is_empty()) {
                line.push_str(&format!(" DEFAULT {def}"));
            }
            line
        })
        .collect();
    if let Some(pk) = detail.indexes.iter().find(|i| i.is_primary) {
        let cols = split_cols(&pk.columns)
            .map(q)
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("  PRIMARY KEY ({cols})"));
    }
    let mut ddl = format!("CREATE TABLE {} (\n{}\n);", q(table), lines.join(",\n"));
    for idx in detail.indexes.iter().filter(|i| !i.is_primary) {
        let cols = split_cols(&idx.columns)
            .map(q)
            .collect::<Vec<_>>()
            .join(", ");
        ddl.push_str(&format!(
            "\n\nCREATE {}INDEX {} ON {} ({cols});",
            if idx.is_unique { "UNIQUE " } else { "" },
            q(&idx.name),
            q(table)
        ));
    }
    ddl
}

/// 由表详情合成 INSERT 模板,值用 `<列名>` 占位,便于替换。
pub fn build_insert(table: &str, detail: &TableDetail, d: Dialect) -> String {
    let cols = detail
        .columns
        .iter()
        .map(|c| ident(&c.name, d))
        .collect::<Vec<_>>()
        .join(", ");
    let vals = detail
        .columns
        .iter()
        .map(|c| format!("<{}>", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    format!("INSERT INTO {} ({cols})\nVALUES ({vals});", ident(table, d))
}

/// 由结果某一行的真实值合成 INSERT(NULL 原样,其余按方言转义)。
pub fn build_insert_row(
    table: &str,
    columns: &[String],
    values: &[Option<String>],
    d: Dialect,
) -> String {
    let cols = columns
        .iter()
        .map(|c| ident(c, d))
        .collect::<Vec<_>>()
        .join(", ");
    let vals = values
        .iter()
        .map(|v| {
            v.as_deref()
                .map_or_else(|| "NULL".to_string(), |v| literal(v, d))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("INSERT INTO {} ({cols}) VALUES ({vals});", ident(table, d))
}

/// 从任意 SQL 文本里猜测主表名:取 FROM / UPDATE / INTO 后的第一个标识符,
/// 去掉引号和 schema 前缀。找不到时返回 None,交给调用方兜底。
pub fn guess_table_name(sql: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)\b(?:from|update|into)\s+(?:\w+\.)?(?:"([^"]+)"|`([^`]+)`|(\w+))"#)
            .unwrap()
    });
    let caps = RE.captures(sql)?;
    caps.get(1)
        .or(caps.get(2))
        .or(caps.get(3))
        .map(|m| m.as_str().to_string())
}

/// 分页浏览一张表的 SQL(可带排序)。
pub fn page_sql(
    table: &str,
    page: usize,
    page_size: usize,
    d: Dialect,
    sort: Option<(&str, bool)>,
) -> String {
    let order = sort
        .map(|(col, desc)| {
            format!(
                " ORDER BY {} {}",
                ident(col, d),
                if desc { "DESC" } else { "ASC" }
            )
        })
        .unwrap_or_default();
    format!(
        "SELECT * FROM {}{order} LIMIT {page_size} OFFSET {}",
        ident(table, d),
        page * page_size
    )
}

/// 列类型大类:决定条件怎么比较。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColClass {
    Number,
    Temporal,
    Other,
}

fn col_class(data_type: Option<&str>) -> ColClass {
    let t = data_type.unwrap_or("").to_ascii_lowercase();
    if t.contains("time") || t.contains("date") {
        return ColClass::Temporal;
    }
    let numeric = ["int", "serial", "numeric", "decimal", "real", "double", "float", "money"];
    if numeric.iter().any(|k| t.contains(k)) && !t.contains("interval") && !t.contains("point") {
        ColClass::Number
    } else {
        ColClass::Other
    }
}

fn is_number(v: &str) -> bool {
    let v = v.trim();
    !v.is_empty() && v.parse::<f64>().is_ok() && !v.contains(['e', 'E', 'n', 'N', 'i', 'I'])
}

/// 列转成文本(各方言写法不同),用于文本比较与 LIKE。
fn as_text(col: &str, d: Dialect) -> String {
    match d {
        Dialect::Pg => format!("CAST({col} AS TEXT)"),
        Dialect::Mysql => format!("CAST({col} AS CHAR)"),
        Dialect::Sqlite => format!("CAST({col} AS TEXT)"),
    }
}

/// LIKE 模式:转义 `\ % _`,统一用 `ESCAPE '\'`。
fn like_pattern(v: &str, prefix: bool, suffix: bool) -> String {
    let escaped = v.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    format!("{}{escaped}{}", if prefix { "%" } else { "" }, if suffix { "%" } else { "" })
}

fn like(col: &str, v: &str, prefix: bool, suffix: bool, d: Dialect) -> String {
    let pat = literal(&like_pattern(v, prefix, suffix), d);
    let esc = match d {
        Dialect::Mysql => "'\\\\'",
        _ => "'\\'",
    };
    match d {
        // ILIKE:与界面上的筛选一样忽略大小写
        Dialect::Pg => format!("{} ILIKE {pat} ESCAPE {esc}", as_text(col, d)),
        _ => format!("LOWER({}) LIKE LOWER({pat}) ESCAPE {esc}", as_text(col, d)),
    }
}

/// 一条筛选条件 → SQL 谓词。数值列配数值时按数值比较;日期时间列用字面量比较(可走索引);
/// 其余按文本比较,等于 / 不等于忽略大小写(与界面上的客户端筛选一致)。
fn predicate(f: &crate::grid::FilterRow, d: Dialect, data_type: Option<&str>) -> String {
    use crate::grid::FilterOp::*;
    let col = ident(&f.col, d);
    let v = f.value.trim();
    let class = col_class(data_type);
    let numeric = class == ColClass::Number && is_number(v);
    let cmp = |op: &str| -> String {
        if numeric {
            format!("{col} {op} {v}")
        } else if class == ColClass::Temporal {
            format!("{col} {op} {}", literal(v, d))
        } else {
            format!("{} {op} {}", as_text(&col, d), literal(v, d))
        }
    };
    let eq = |negate: bool| -> String {
        if numeric {
            format!("{col} {} {v}", if negate { "<>" } else { "=" })
        } else {
            let e = format!("LOWER({}) {} LOWER({})", as_text(&col, d), if negate { "<>" } else { "=" }, literal(v, d));
            // 与客户端一致:NULL 不等于任何具体值
            if negate { format!("({col} IS NULL OR {e})") } else { e }
        }
    };
    match f.op {
        Contains => like(&col, v, true, true, d),
        Starts => like(&col, v, false, true, d),
        Ends => like(&col, v, true, false, d),
        Eq => eq(false),
        Ne => eq(true),
        Gt => cmp(">"),
        Lt => cmp("<"),
        Gte => cmp(">="),
        Lte => cmp("<="),
        Empty => format!("({col} IS NULL OR {} = '')", as_text(&col, d)),
        NotEmpty => format!("({col} IS NOT NULL AND {} <> '')", as_text(&col, d)),
    }
}

/// JSON 字段条件 → PG 谓词(只支持 PG;其他方言返回 None,该条件不下推)。
///
/// - 等于 / 不等于、jsonb 列、路径全是键、值与字段类型对得上:用包含查询 `col @> '{"a":{"b":3}}'`。
///   能用上 jsonb 的 GIN 索引(默认 `jsonb_ops` 与 `jsonb_path_ops` 都支持 `@>`),也天然按类型比较
///   (数字比数值、字符串区分大小写、`3` 与 `"3"` 不相等),不会因为某行该字段不是数字而转换报错;
/// - 其余(大小比较、包含文字、路径里有数组下标、json 类型列):取出字段比较。数值比较写成
///   `CASE WHEN jsonb_typeof(…) = 'number' THEN (…->>…)::numeric END > 3`,CASE 保证先判断类型再转换,
///   别的行里同名字段是文本时只是不命中,而不是整条查询报错;
/// - 不等于一律写成「等于 IS NOT TRUE」:字段缺失 / 为 NULL 的行也算不等于(与客户端筛选一致)。
///
/// 路径里的键都经 [`literal`] 转义成字符串字面量、下标是整数,包含查询的文档整体再作为字面量,键名无法注入。
fn json_predicate(f: &crate::grid::FilterRow, d: Dialect, data_type: Option<&str>) -> Option<String> {
    use crate::grid::FilterOp::*;
    use crate::json::{self, Kind};
    if d != Dialect::Pg {
        return None;
    }
    let t = data_type.unwrap_or("").to_ascii_lowercase();
    // json 列没有 @>,类型函数也不同;不是 JSON 类型的列(如存 JSON 文本的 text)先转成 jsonb
    let (col, is_jsonb) = if t.contains("jsonb") {
        (ident(&f.col, d), true)
    } else if t.contains("json") {
        (ident(&f.col, d), false)
    } else {
        (format!("CAST({} AS jsonb)", ident(&f.col, d)), true)
    };
    let typeof_fn = if is_jsonb { "jsonb_typeof" } else { "json_typeof" };
    let field = format!("({})", json::pg_path(&col, &f.path, false));
    let text = format!("({})", json::pg_path(&col, &f.path, true));
    let v = f.value.trim();
    let typed = f.json_type.and_then(|k| json::typed_value(k, &f.value));
    let numeric = matches!(f.json_type, None | Some(Kind::Number)) && json::typed_value(Kind::Number, v).is_some();

    let equal = || -> String {
        let doc = typed.as_ref().filter(|_| is_jsonb).and_then(|val| json::containment_doc(&f.path, val));
        match (&typed, f.json_type, doc) {
            (_, _, Some(doc)) => format!("{col} @> {}::jsonb", literal(&doc, d)),
            (Some(val), Some(Kind::Number), _) => {
                format!("CASE WHEN {typeof_fn}({field}) = 'number' THEN {text}::numeric END = {}", json::to_text(val, false))
            }
            (Some(_), Some(Kind::Null), _) => format!("{typeof_fn}({field}) = 'null'"),
            (Some(val), Some(kind), _) => {
                let ty = if kind == Kind::Bool { "boolean" } else { "string" };
                let lit = literal(&val.as_text().unwrap_or_default(), d);
                format!("{typeof_fn}({field}) = '{ty}' AND {text} = {lit}")
            }
            // 手填的条件(或值与类型对不上):与普通列一样按文本、忽略大小写
            _ => format!("LOWER({text}) = LOWER({})", literal(v, d)),
        }
    };
    let cmp = |op: &str| -> String {
        if numeric {
            format!("CASE WHEN {typeof_fn}({field}) = 'number' THEN {text}::numeric END {op} {v}")
        } else {
            format!("{text} {op} {}", literal(v, d))
        }
    };
    Some(match f.op {
        Contains => like(&text, v, true, true, d),
        Starts => like(&text, v, false, true, d),
        Ends => like(&text, v, true, false, d),
        Eq => equal(),
        Ne => format!("({}) IS NOT TRUE", equal()),
        Gt => cmp(">"),
        Lt => cmp("<"),
        Gte => cmp(">="),
        Lte => cmp("<="),
        Empty => format!("({text} IS NULL OR {text} = '')"),
        NotEmpty => format!("({text} IS NOT NULL AND {text} <> '')"),
    })
}

/// 把筛选条件拼成 WHERE 子句的内容(不含 `WHERE`);没有有效条件为 None。
/// `types` 给出列的类型(列名 → 类型名),决定数值 / 日期 / 文本的比较方式。
pub fn where_clause(
    filters: &[crate::grid::FilterRow],
    conj: crate::grid::Conj,
    d: Dialect,
    types: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let parts: Vec<String> = filters
        .iter()
        .filter(|f| f.is_active())
        .filter_map(|f| {
            let ty = types.get(&f.col).map(String::as_str);
            if f.path.is_empty() {
                Some(predicate(f, d, ty))
            } else {
                json_predicate(f, d, ty)
            }
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    let joiner = match conj {
        crate::grid::Conj::And => " AND ",
        crate::grid::Conj::Or => " OR ",
    };
    Some(parts.iter().map(|p| format!("({p})")).collect::<Vec<_>>().join(joiner))
}

/// 带筛选的分页 SQL。
pub fn page_sql_where(
    table: &str,
    page: usize,
    page_size: usize,
    d: Dialect,
    sort: Option<(&str, bool)>,
    where_sql: Option<&str>,
) -> String {
    let base = page_sql(table, page, page_size, d, sort);
    match where_sql {
        None => base,
        Some(w) => {
            let from = format!("SELECT * FROM {}", ident(table, d));
            base.replacen(&from, &format!("{from} WHERE {w}"), 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pg::browse::{ColumnInfo, IndexInfo};

    fn detail() -> TableDetail {
        TableDetail {
            columns: vec![
                ColumnInfo {
                    name: "id".into(),
                    data_type: "integer".into(),
                    default: None,
                    comment: None,
                    not_null: true,
                    is_pk: true,
                },
                ColumnInfo {
                    name: "name".into(),
                    data_type: "text".into(),
                    default: Some("''".into()),
                    comment: None,
                    not_null: false,
                    is_pk: false,
                },
            ],
            indexes: vec![
                IndexInfo {
                    name: "users_pkey".into(),
                    columns: "id".into(),
                    is_primary: true,
                    is_unique: true,
                },
                IndexInfo {
                    name: "idx_name".into(),
                    columns: "name".into(),
                    is_primary: false,
                    is_unique: true,
                },
            ],
        }
    }

    #[test]
    fn create_table_includes_pk_and_indexes() {
        let ddl = build_create_table("users", &detail());
        assert!(ddl.contains("\"id\" integer NOT NULL"));
        assert!(ddl.contains("\"name\" text DEFAULT ''"));
        assert!(ddl.contains("PRIMARY KEY (\"id\")"));
        assert!(ddl.contains("CREATE UNIQUE INDEX \"idx_name\" ON \"users\" (\"name\");"));
    }

    #[test]
    fn insert_template_and_row() {
        assert_eq!(
            build_insert("users", &detail(), Dialect::Mysql),
            "INSERT INTO `users` (`id`, `name`)\nVALUES (<id>, <name>);"
        );
        let row = build_insert_row(
            "t",
            &["a".into(), "b".into()],
            &[Some("it's \\ ok".into()), None],
            Dialect::Mysql,
        );
        assert_eq!(
            row,
            "INSERT INTO `t` (`a`, `b`) VALUES ('it''s \\\\ ok', NULL);"
        );
    }

    #[test]
    fn guesses_table_names() {
        assert_eq!(
            guess_table_name("select * from public.\"Users\" u").as_deref(),
            Some("Users")
        );
        assert_eq!(
            guess_table_name("UPDATE `orders` SET x=1").as_deref(),
            Some("orders")
        );
        assert_eq!(guess_table_name("select 1"), None);
    }

    #[test]
    fn page_sql_with_sort() {
        assert_eq!(
            page_sql("t", 2, 100, Dialect::Pg, Some(("name", true))),
            "SELECT * FROM \"t\" ORDER BY \"name\" DESC LIMIT 100 OFFSET 200"
        );
    }

    fn f(col: &str, op: crate::grid::FilterOp, value: &str) -> crate::grid::FilterRow {
        crate::grid::FilterRow { id: 1, col: col.into(), op, value: value.into(), path: vec![], json_type: None }
    }

    fn jf(
        col: &str,
        path: &[&str],
        ty: Option<crate::json::Kind>,
        op: crate::grid::FilterOp,
        value: &str,
    ) -> crate::grid::FilterRow {
        use crate::json::PathSeg;
        let path = path
            .iter()
            .map(|p| p.parse::<u64>().map_or_else(|_| PathSeg::Key(p.to_string()), PathSeg::Index))
            .collect();
        crate::grid::FilterRow { id: 1, col: col.into(), op, value: value.into(), path, json_type: ty }
    }

    #[test]
    fn json_path_where_clause() {
        use crate::grid::{Conj, FilterOp::*};
        use crate::json::Kind;
        let types: std::collections::HashMap<String, String> =
            [("doc", "jsonb"), ("raw", "json"), ("txt", "text")].into_iter().map(|(a, b)| (a.into(), b.into())).collect();
        let w = |f: crate::grid::FilterRow| where_clause(&[f], Conj::And, Dialect::Pg, &types).unwrap();
        // jsonb 的等于:包含查询(可走 GIN 索引),按类型比较
        assert_eq!(w(jf("doc", &["a", "n"], Some(Kind::Number), Eq, "3")), r#"("doc" @> '{"a":{"n":3}}'::jsonb)"#);
        assert_eq!(w(jf("doc", &["a", "s"], Some(Kind::String), Eq, "Paid")), r#"("doc" @> '{"a":{"s":"Paid"}}'::jsonb)"#);
        assert_eq!(w(jf("doc", &["ok"], Some(Kind::Bool), Ne, "true")), r#"(("doc" @> '{"ok":true}'::jsonb) IS NOT TRUE)"#);
        // 键名里的引号:JSON 转义后整体再按 SQL 字面量转义
        assert_eq!(
            w(jf("doc", &["it's \"x\""], Some(Kind::String), Eq, "o'k")),
            r#"("doc" @> '{"it''s \"x\"":"o''k"}'::jsonb)"#
        );
        // 大小比较:先判断类型再转数值
        assert_eq!(
            w(jf("doc", &["a", "n"], Some(Kind::Number), Gte, "2.5")),
            r#"(CASE WHEN jsonb_typeof(("doc"->'a'->'n')) = 'number' THEN ("doc"->'a'->>'n')::numeric END >= 2.5)"#
        );
        // 路径里有下标:不能用包含查询,取出字段比较
        assert_eq!(
            w(jf("doc", &["items", "0", "sku"], Some(Kind::Number), Eq, "7")),
            r#"(CASE WHEN jsonb_typeof(("doc"->'items'->0->'sku')) = 'number' THEN ("doc"->'items'->0->>'sku')::numeric END = 7)"#
        );
        // json 列:没有 @>,类型函数用 json_typeof
        assert_eq!(
            w(jf("raw", &["s"], Some(Kind::String), Eq, "a")),
            r#"(json_typeof(("raw"->'s')) = 'string' AND ("raw"->>'s') = 'a')"#
        );
        assert_eq!(w(jf("raw", &["z"], Some(Kind::Null), Eq, "null")), r#"(json_typeof(("raw"->'z')) = 'null')"#);
        // 包含文字 / 为空:比较 ->> 取出的文本
        assert_eq!(
            w(jf("doc", &["a", "s"], Some(Kind::String), Contains, "50%")),
            r#"(CAST(("doc"->'a'->>'s') AS TEXT) ILIKE '%50\%%' ESCAPE '\')"#
        );
        assert_eq!(w(jf("doc", &["a"], None, Empty, "")), r#"((("doc"->>'a') IS NULL OR ("doc"->>'a') = ''))"#);
        // 数字类型配非数字的值:退回文本比较,不生成非法 SQL
        assert_eq!(w(jf("doc", &["a", "n"], Some(Kind::Number), Gt, "1; DROP")), r#"(("doc"->'a'->>'n') > '1; DROP')"#);
        assert_eq!(
            w(jf("doc", &["a", "n"], Some(Kind::Number), Eq, "x")),
            r#"(LOWER(("doc"->'a'->>'n')) = LOWER('x'))"#
        );
        // 不是 JSON 类型的列:先转 jsonb
        assert_eq!(w(jf("txt", &["k"], Some(Kind::Number), Eq, "1")), r#"(CAST("txt" AS jsonb) @> '{"k":1}'::jsonb)"#);
        // 注入:键名里的 '); DROP … 留在字面量里
        let s = w(jf("doc", &["x'); DROP TABLE t; --"], None, Gt, "a"));
        assert_eq!(s, r#"(("doc"->>'x''); DROP TABLE t; --') > 'a')"#);
        // 非 PG 不下推 JSON 路径条件
        assert!(where_clause(&[jf("doc", &["a"], None, Eq, "1")], Conj::And, Dialect::Mysql, &types).is_none());
    }

    fn types() -> std::collections::HashMap<String, String> {
        [("amount", "numeric(10,2)"), ("id", "integer"), ("created_at", "timestamp with time zone"), ("status", "text")]
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn where_clause_per_type_and_dialect() {
        use crate::grid::{Conj, FilterOp::*};
        let w = where_clause(
            &[f("amount", Gt, "100"), f("created_at", Gte, "2026-11-05"), f("status", Eq, "Failed")],
            Conj::And,
            Dialect::Pg,
            &types(),
        )
        .unwrap();
        assert_eq!(
            w,
            "(\"amount\" > 100) AND (\"created_at\" >= '2026-11-05') AND (LOWER(CAST(\"status\" AS TEXT)) = LOWER('Failed'))"
        );
        // 数值列配非数值:退回文本比较,不会生成 `amount > abc` 这种非法 SQL
        let w = where_clause(&[f("amount", Gt, "abc")], Conj::And, Dialect::Pg, &types()).unwrap();
        assert_eq!(w, "(CAST(\"amount\" AS TEXT) > 'abc')");
        // LIKE 转义 % _,MySQL 用反引号与 CHAR
        let w = where_clause(&[f("status", Contains, "50%_off")], Conj::Or, Dialect::Mysql, &types()).unwrap();
        assert_eq!(w, "(LOWER(CAST(`status` AS CHAR)) LIKE LOWER('%50\\\\%\\\\_off%') ESCAPE '\\\\')");
        let w = where_clause(&[f("status", Starts, "pa"), f("id", Ne, "3")], Conj::Or, Dialect::Pg, &types()).unwrap();
        assert_eq!(w, "(CAST(\"status\" AS TEXT) ILIKE 'pa%' ESCAPE '\\') OR (\"id\" <> 3)");
        // 引号注入被转义
        let w = where_clause(&[f("status", Eq, "x' OR '1'='1")], Conj::And, Dialect::Sqlite, &types()).unwrap();
        assert!(w.contains("'x'' OR ''1''=''1'"));
        // 空 / 非空、无有效条件
        let w = where_clause(&[f("status", Empty, "")], Conj::And, Dialect::Sqlite, &types()).unwrap();
        assert_eq!(w, "((\"status\" IS NULL OR CAST(\"status\" AS TEXT) = ''))");
        assert!(where_clause(&[f("", Eq, "x"), f("status", Eq, "")], Conj::And, Dialect::Pg, &types()).is_none());
    }

    #[test]
    fn page_sql_with_where_keeps_order_and_paging() {
        let s = page_sql_where("orders", 2, 100, Dialect::Pg, Some(("id", true)), Some("(\"id\" > 3)"));
        assert_eq!(s, "SELECT * FROM \"orders\" WHERE (\"id\" > 3) ORDER BY \"id\" DESC LIMIT 100 OFFSET 200");
    }
}
