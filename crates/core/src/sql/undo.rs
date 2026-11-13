//! 写语句的撤销:执行前抓取会被改动的原始行(快照),据此生成反向 SQL(纯函数)。
//!
//! - DELETE:用 INSERT 把原始行插回去(全部列)
//! - UPDATE:按主键把 SET 里赋值过的列改回原值;要求表有主键,且不改主键本身
//!
//! 表名照语句原文(带 schema 与引号);值一般写成字符串字面量,由数据库按列类型转换。
//! 二进制值按文本取回会失真(MySQL 的 BLOB、SQLite 的 BLOB 都按 UTF-8 有损转换),所以抓快照时
//! 让数据库自己写好字面量(SQLite 的 `quote()`、MySQL 的 `X'十六进制'`),这些列在 `literal` 里标出,原样使用。

use super::build::{ident, literal};
use super::impact::WriteOp;
use super::Dialect;

/// 抓快照的行数上限:超出就不提供撤销(撤销语句也会太大)。
pub const MAX_ROWS: usize = 10_000;

/// 一条写语句执行前的快照。
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// 表名原文
    pub table: String,
    pub op: WriteOp,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// 主键列(UPDATE 撤销用)
    pub primary_key: Vec<String>,
    /// 各列的值是否已是数据库写好的 SQL 字面量(见模块说明);空表示都不是
    pub literal: Vec<bool>,
}

/// 数据库写好的字面量是否规整:数字、NULL、'…'(内部单引号成对)、X'十六进制'。
/// 不规整(理论上不会)就当普通文本转义,不原样拼进语句。
fn plain_literal(v: &str) -> bool {
    let upper = v.to_ascii_uppercase();
    if upper == "NULL" {
        return true;
    }
    if let Some(hex) = upper.strip_prefix("X'").and_then(|r| r.strip_suffix('\'')) {
        return hex.len() % 2 == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    if let Some(body) = v.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        return !body.replace("''", "").contains('\'');
    }
    !v.is_empty() && v.parse::<f64>().is_ok() && v.bytes().all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b))
}

/// 为什么不能撤销。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// 不是能改写的单表 UPDATE / DELETE(INSERT、多表、CTE、DDL…)
    Statement,
    /// TRUNCATE / DROP TABLE
    WholeTable,
    TooManyRows,
    /// UPDATE 的表没有主键
    NoPrimaryKey,
    /// UPDATE 改了主键
    ChangesPrimaryKey,
    /// UPDATE 的 SET 解析不了(如元组赋值)
    UnknownColumns,
}

fn value(s: &Snapshot, row: &[Option<String>], i: usize, d: Dialect) -> String {
    match row[i].as_deref() {
        None => "NULL".to_string(),
        Some(v) if s.literal.get(i).copied().unwrap_or(false) && plain_literal(v) => v.to_string(),
        Some(v) => literal(v, d),
    }
}

/// MySQL 里按二进制存的列类型(值按文本取回会失真)。
fn mysql_binary(data_type: &str) -> bool {
    let t = data_type.to_ascii_lowercase();
    t.contains("binary") || t.contains("blob") || t.starts_with("bit")
}

/// 包一层快照查询,让数据库把值写成字面量:SQLite 全部列用 `quote()`(类型随值,BLOB 也无损);
/// MySQL 只处理二进制列(`X'十六进制'`);PG 的 bytea 本来就以 `\x…` 文本往返,不用处理。
/// 返回 (查询, 各列是否为字面量);列名与快照原样一致。
pub fn binary_safe_snapshot(
    snapshot_sql: &str,
    columns: &[crate::pg::browse::ColumnInfo],
    d: Dialect,
) -> (String, Vec<bool>) {
    let wrap = |exprs: Vec<String>| format!("SELECT {} FROM (\n{}\n) AS datum_snap", exprs.join(", "), snapshot_sql.trim());
    match d {
        Dialect::Sqlite if !columns.is_empty() => {
            let exprs = columns.iter().map(|c| format!("quote({0}) AS {0}", ident(&c.name, d))).collect();
            (wrap(exprs), vec![true; columns.len()])
        }
        Dialect::Mysql if columns.iter().any(|c| mysql_binary(&c.data_type)) => {
            let exprs = columns
                .iter()
                .map(|c| {
                    let q = ident(&c.name, d);
                    if mysql_binary(&c.data_type) {
                        format!("IF({q} IS NULL, NULL, CONCAT('X''', HEX({q}), '''')) AS {q}")
                    } else {
                        q
                    }
                })
                .collect();
            (wrap(exprs), columns.iter().map(|c| mysql_binary(&c.data_type)).collect())
        }
        _ => (snapshot_sql.to_string(), vec![]),
    }
}

fn index_of(columns: &[String], name: &str) -> Option<usize> {
    columns
        .iter()
        .position(|c| c == name)
        .or_else(|| columns.iter().position(|c| c.eq_ignore_ascii_case(name)))
}

/// 快照 → 反向语句(按行一条);做不到返回原因。
pub fn statements(s: &Snapshot, d: Dialect) -> Result<Vec<String>, Unsupported> {
    match &s.op {
        WriteOp::WholeTable => Err(Unsupported::WholeTable),
        WriteOp::Delete => {
            let cols = s.columns.iter().map(|c| ident(c, d)).collect::<Vec<_>>().join(", ");
            Ok(s.rows
                .iter()
                .map(|r| {
                    let vals = (0..r.len()).map(|i| value(s, r, i, d)).collect::<Vec<_>>().join(", ");
                    format!("INSERT INTO {} ({cols}) VALUES ({vals})", s.table)
                })
                .collect())
        }
        WriteOp::Update { set_columns } => {
            let set_columns = set_columns.as_ref().ok_or(Unsupported::UnknownColumns)?;
            if s.primary_key.is_empty() {
                return Err(Unsupported::NoPrimaryKey);
            }
            if set_columns.iter().any(|c| s.primary_key.iter().any(|k| k.eq_ignore_ascii_case(c))) {
                return Err(Unsupported::ChangesPrimaryKey);
            }
            let set_idx: Vec<(usize, &String)> = set_columns
                .iter()
                .map(|c| index_of(&s.columns, c).map(|i| (i, &s.columns[i])))
                .collect::<Option<_>>()
                .ok_or(Unsupported::UnknownColumns)?;
            let key_idx: Vec<(usize, &String)> = s
                .primary_key
                .iter()
                .map(|k| index_of(&s.columns, k).map(|i| (i, &s.columns[i])))
                .collect::<Option<_>>()
                .ok_or(Unsupported::NoPrimaryKey)?;
            Ok(s.rows
                .iter()
                .map(|r| {
                    let set = set_idx
                        .iter()
                        .map(|(i, c)| format!("{} = {}", ident(c, d), value(s, r, *i, d)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let cond = key_idx
                        .iter()
                        .map(|(i, c)| format!("{} = {}", ident(c, d), value(s, r, *i, d)))
                        .collect::<Vec<_>>()
                        .join(" AND ");
                    format!("UPDATE {} SET {set} WHERE {cond}", s.table)
                })
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Dialect::*;

    fn snap(op: WriteOp, pk: &[&str]) -> Snapshot {
        Snapshot {
            table: "public.\"Orders\"".into(),
            op,
            columns: vec!["id".into(), "status".into(), "note".into()],
            rows: vec![
                vec![Some("1".into()), Some("paid".into()), None],
                vec![Some("2".into()), Some("it's".into()), Some("x".into())],
            ],
            primary_key: pk.iter().map(|s| s.to_string()).collect(),
            literal: vec![],
        }
    }

    #[test]
    fn literal_columns_are_used_verbatim_when_well_formed() {
        let mut s = snap(WriteOp::Delete, &[]);
        s.literal = vec![true, true, true];
        s.rows = vec![vec![Some("1".into()), Some("X'00FF'".into()), Some("'it''s'".into())]];
        assert_eq!(
            statements(&s, Sqlite).unwrap()[0],
            "INSERT INTO public.\"Orders\" (\"id\", \"status\", \"note\") VALUES (1, X'00FF', 'it''s')"
        );
        // 不规整的「字面量」当普通文本转义
        s.rows = vec![vec![Some("1); DROP TABLE x; --".into()), Some("X'0G'".into()), Some("'a' || 'b'".into())]];
        let out = &statements(&s, Sqlite).unwrap()[0];
        assert!(out.contains("'1); DROP TABLE x; --'"), "{out}");
        assert!(out.contains("'X''0G'''"), "{out}");
        assert!(out.contains("'''a'' || ''b'''"), "{out}");
        for ok in ["NULL", "-1.5e3", "X''", "''", "'a''b'"] {
            assert!(plain_literal(ok), "{ok}");
        }
        for bad in ["1 OR 1", "'a' 'b'", "X'ABC'", "0x00", ""] {
            assert!(!plain_literal(bad), "{bad}");
        }
    }

    #[test]
    fn delete_becomes_inserts_of_the_original_rows() {
        let s = statements(&snap(WriteOp::Delete, &[]), Pg).unwrap();
        assert_eq!(
            s,
            vec![
                "INSERT INTO public.\"Orders\" (\"id\", \"status\", \"note\") VALUES ('1', 'paid', NULL)",
                "INSERT INTO public.\"Orders\" (\"id\", \"status\", \"note\") VALUES ('2', 'it''s', 'x')",
            ]
        );
        // MySQL 的标识符与反斜杠转义
        let s = statements(&snap(WriteOp::Delete, &[]), Mysql).unwrap();
        assert!(s[0].contains("(`id`, `status`, `note`)"), "{}", s[0]);
    }

    #[test]
    fn update_restores_assigned_columns_by_primary_key() {
        let op = WriteOp::Update { set_columns: Some(vec!["STATUS".into()]) };
        let s = statements(&snap(op, &["id"]), Sqlite).unwrap();
        assert_eq!(
            s,
            vec![
                "UPDATE public.\"Orders\" SET \"status\" = 'paid' WHERE \"id\" = '1'",
                "UPDATE public.\"Orders\" SET \"status\" = 'it''s' WHERE \"id\" = '2'",
            ]
        );
    }

    #[test]
    fn what_cannot_be_undone() {
        let upd = |cols: Option<Vec<&str>>| WriteOp::Update { set_columns: cols.map(|c| c.into_iter().map(String::from).collect()) };
        assert_eq!(statements(&snap(upd(Some(vec!["status"])), &[]), Pg), Err(Unsupported::NoPrimaryKey));
        assert_eq!(statements(&snap(upd(Some(vec!["id"])), &["id"]), Pg), Err(Unsupported::ChangesPrimaryKey));
        assert_eq!(statements(&snap(upd(None), &["id"]), Pg), Err(Unsupported::UnknownColumns));
        assert_eq!(statements(&snap(upd(Some(vec!["missing"])), &["id"]), Pg), Err(Unsupported::UnknownColumns));
        assert_eq!(statements(&snap(WriteOp::WholeTable, &["id"]), Pg), Err(Unsupported::WholeTable));
    }
}
