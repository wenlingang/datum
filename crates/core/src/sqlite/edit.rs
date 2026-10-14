use crate::error::AppResult;
pub use crate::pg::edit::{CellEdit, RowDelete};
use crate::sqlite::client::SqlitePool;

/// 批量提交编辑与删除:单事务,全成功或全回滚。返回受影响行数。
pub fn commit_edits(
    pool: &SqlitePool,
    id: &str,
    edits: &[CellEdit],
    deletes: &[RowDelete],
) -> AppResult<u64> {
    if edits.is_empty() && deletes.is_empty() {
        return Ok(0);
    }
    pool.with_txn(id, edits, deletes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_edits_is_noop() {
        let p = SqlitePool::default();
        p.connect("t", ":memory:").unwrap();
        assert_eq!(commit_edits(&p, "t", &[], &[]).unwrap(), 0);
    }

    #[test]
    fn deletes_a_row_in_txn() {
        let p = SqlitePool::default();
        p.connect("t", ":memory:").unwrap();
        p.query("t", "CREATE TABLE dtest(id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        p.query("t", "INSERT INTO dtest VALUES (1,'a'),(2,'b')")
            .unwrap();
        let dels = vec![RowDelete {
            table: "dtest".into(),
            pk_col: "id".into(),
            pk_value: "2".into(),
        }];
        assert_eq!(commit_edits(&p, "t", &[], &dels).unwrap(), 1);
        let r = p.query("t", "SELECT id FROM dtest").unwrap();
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][0].as_deref(), Some("1"));
    }

    #[test]
    fn bad_edit_rolls_back_deletes_too() {
        let p = SqlitePool::default();
        p.connect("t", ":memory:").unwrap();
        p.query("t", "CREATE TABLE dtest(id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        p.query("t", "INSERT INTO dtest VALUES (1,'a'),(2,'b')")
            .unwrap();
        // 编辑引用不存在的列 → 整批回滚,删除也不生效
        let edits = vec![CellEdit {
            table: "dtest".into(),
            pk_col: "id".into(),
            pk_value: "1".into(),
            column: "no_such_col".into(),
            new_value: "x".into(),
        }];
        let dels = vec![RowDelete {
            table: "dtest".into(),
            pk_col: "id".into(),
            pk_value: "2".into(),
        }];
        assert!(commit_edits(&p, "t", &edits, &dels).is_err());
        let r = p.query("t", "SELECT count(*) FROM dtest").unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("2"));
    }

    #[test]
    fn edits_a_numeric_column() {
        let p = SqlitePool::default();
        p.connect("t", ":memory:").unwrap();
        p.query(
            "t",
            "CREATE TABLE numtest(id INTEGER PRIMARY KEY, qty INTEGER)",
        )
        .unwrap();
        p.query("t", "INSERT INTO numtest VALUES (1, 10)").unwrap();
        let edits = vec![CellEdit {
            table: "numtest".into(),
            pk_col: "id".into(),
            pk_value: "1".into(),
            column: "qty".into(),
            new_value: "42".into(),
        }];
        assert_eq!(commit_edits(&p, "t", &edits, &[]).unwrap(), 1);
        let r = p.query("t", "SELECT qty FROM numtest WHERE id=1").unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("42"));
    }
}
