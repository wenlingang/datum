//! 写语句的撤销:执行前抓取会被改动的原始行(快照),据此生成反向 SQL(纯函数)。
//!
//! - DELETE:用 INSERT 把原始行插回去(全部列)
//! - UPDATE:按主键把 SET 里赋值过的列改回原值;要求表有主键,且不改主键本身
//!
//! 表名照语句原文(带 schema 与引号);值一律写成字符串字面量,由数据库按列类型转换。

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

fn value(v: &Option<String>, d: Dialect) -> String {
    v.as_deref().map_or_else(|| "NULL".to_string(), |v| literal(v, d))
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
                    let vals = r.iter().map(|v| value(v, d)).collect::<Vec<_>>().join(", ");
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
                        .map(|(i, c)| format!("{} = {}", ident(c, d), value(&r[*i], d)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let cond = key_idx
                        .iter()
                        .map(|(i, c)| format!("{} = {}", ident(c, d), value(&r[*i], d)))
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
