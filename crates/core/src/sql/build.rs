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
}
