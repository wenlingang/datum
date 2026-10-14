use crate::error::AppResult;
pub use crate::pg::browse::{ColumnInfo, IndexInfo, TableDetail};
use crate::sqlite::client::SqlitePool;

/// 列出用户表(排除 sqlite_ 内部表)。
pub fn list_tables(pool: &SqlitePool, id: &str) -> AppResult<Vec<String>> {
    let r = pool.query(
        id,
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    Ok(r.rows
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect())
}

fn truthy(cell: Option<&Option<String>>) -> bool {
    matches!(cell, Some(Some(s)) if s == "1")
}

/// 表详情:列定义(名/类型/默认值/非空/主键)+ 索引(名/列/主键/唯一)。
/// 列注释 SQLite 不支持,统一为 None。
pub fn table_detail(pool: &SqlitePool, id: &str, table: &str) -> AppResult<TableDetail> {
    let tq = table.replace('"', "\"\"");

    // PRAGMA table_info: cid(0) name(1) type(2) notnull(3) dflt_value(4) pk(5)
    let cr = pool.query(id, &format!("PRAGMA table_info(\"{tq}\")"))?;
    let columns: Vec<ColumnInfo> = cr
        .rows
        .into_iter()
        .map(|r| ColumnInfo {
            name: r.get(1).cloned().flatten().unwrap_or_default(),
            data_type: r.get(2).cloned().flatten().unwrap_or_default(),
            not_null: truthy(r.get(3)),
            default: r.get(4).cloned().flatten(),
            comment: None,
            // pk 列:0 表示非主键,>0 表示主键(值为复合主键中的序号)
            is_pk: r
                .get(5)
                .cloned()
                .flatten()
                .map(|s| s != "0")
                .unwrap_or(false),
        })
        .collect();

    // PRAGMA index_list: seq(0) name(1) unique(2) origin(3) partial(4)
    let il = pool.query(id, &format!("PRAGMA index_list(\"{tq}\")"))?;
    let mut indexes: Vec<IndexInfo> = vec![];
    for r in il.rows {
        let name = r.get(1).cloned().flatten().unwrap_or_default();
        let is_unique = truthy(r.get(2));
        let is_primary = r.get(3).cloned().flatten().as_deref() == Some("pk");
        // PRAGMA index_info: seqno(0) cid(1) name(2)
        let info = pool.query(
            id,
            &format!("PRAGMA index_info(\"{}\")", name.replace('"', "\"\"")),
        )?;
        let cols = info
            .rows
            .into_iter()
            .filter_map(|c| c.get(2).cloned().flatten())
            .collect::<Vec<_>>()
            .join(", ");
        indexes.push(IndexInfo {
            name,
            columns: cols,
            is_primary,
            is_unique,
        });
    }

    // INTEGER PRIMARY KEY(rowid 别名)不会出现在 index_list 中:从列信息补一条主键项。
    if !indexes.iter().any(|i| i.is_primary) {
        let pk_cols: Vec<String> = columns
            .iter()
            .filter(|c| c.is_pk)
            .map(|c| c.name.clone())
            .collect();
        if !pk_cols.is_empty() {
            indexes.insert(
                0,
                IndexInfo {
                    name: "PRIMARY".into(),
                    columns: pk_cols.join(", "),
                    is_primary: true,
                    is_unique: true,
                },
            );
        }
    }

    Ok(TableDetail { columns, indexes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> SqlitePool {
        let p = SqlitePool::default();
        p.connect("t", ":memory:").unwrap();
        p
    }

    #[test]
    fn lists_tables_and_full_detail() {
        let p = pool();
        p.query("t", "CREATE TABLE detail_test(id INTEGER PRIMARY KEY, email TEXT NOT NULL DEFAULT 'x@x.com', note TEXT)").unwrap();
        p.query(
            "t",
            "CREATE UNIQUE INDEX detail_email_idx ON detail_test(email)",
        )
        .unwrap();

        let tables = list_tables(&p, "t").unwrap();
        assert!(tables.contains(&"detail_test".to_string()));

        let d = table_detail(&p, "t", "detail_test").unwrap();
        let id_col = d.columns.iter().find(|c| c.name == "id").unwrap();
        assert!(id_col.is_pk);
        let email = d.columns.iter().find(|c| c.name == "email").unwrap();
        assert!(email.not_null);
        assert!(email.default.as_deref().unwrap_or("").contains("x@x.com"));
        let note = d.columns.iter().find(|c| c.name == "note").unwrap();
        assert!(!note.not_null && !note.is_pk);

        // 主键(INTEGER PRIMARY KEY 由补全逻辑给出)
        assert!(d.indexes.iter().any(|i| i.is_primary && i.columns == "id"));
        // 唯一索引
        let uq = d
            .indexes
            .iter()
            .find(|i| i.name == "detail_email_idx")
            .unwrap();
        assert!(uq.is_unique && !uq.is_primary && uq.columns == "email");
    }
}
