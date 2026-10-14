use crate::error::AppResult;
use crate::my::client::{esc_ident, MyPool};
use crate::pg::browse::{ColumnInfo, IndexInfo, TableDetail};
use std::collections::HashMap;

/// 列出实例下的库名。
pub async fn list_databases(pool: &MyPool, id: &str) -> AppResult<Vec<String>> {
    let r = pool.query(id, "SHOW DATABASES").await?;
    Ok(r.rows
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect())
}

/// 列出当前库的表。
pub async fn list_tables(pool: &MyPool, id: &str) -> AppResult<Vec<String>> {
    let r = pool.query(id, "SHOW TABLES").await?;
    Ok(r.rows
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect())
}

/// 表详情:字段(SHOW FULL COLUMNS)+ 索引(SHOW INDEX)。
pub async fn table_detail(pool: &MyPool, id: &str, table: &str) -> AppResult<TableDetail> {
    let esc = esc_ident(table);

    // 字段:Field(0) Type(1) Collation(2) Null(3) Key(4) Default(5) Extra(6) Privileges(7) Comment(8)
    let cols = pool
        .query(id, &format!("SHOW FULL COLUMNS FROM `{esc}`"))
        .await?;
    let columns = cols
        .rows
        .iter()
        .map(|r| {
            let g = |i: usize| r.get(i).cloned().flatten();
            ColumnInfo {
                name: g(0).unwrap_or_default(),
                data_type: g(1).unwrap_or_default(),
                default: g(5),
                comment: g(8).filter(|s| !s.is_empty()),
                not_null: g(3).as_deref() == Some("NO"),
                is_pk: g(4).as_deref() == Some("PRI"),
            }
        })
        .collect();

    // 索引:按列名定位,分组聚合
    let idx = pool.query(id, &format!("SHOW INDEX FROM `{esc}`")).await?;
    let pos = |name: &str| {
        idx.columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
    };
    let (kn, nu, cn) = (pos("Key_name"), pos("Non_unique"), pos("Column_name"));

    let mut order: Vec<String> = vec![];
    let mut acc: HashMap<String, (bool, bool, Vec<String>)> = HashMap::new();
    for r in &idx.rows {
        let g = |i: Option<usize>| i.and_then(|x| r.get(x).cloned().flatten());
        let key = g(kn).unwrap_or_default();
        if key.is_empty() {
            continue;
        }
        let non_unique = g(nu).as_deref() == Some("1");
        let col = g(cn).unwrap_or_default();
        if !acc.contains_key(&key) {
            order.push(key.clone());
            acc.insert(key.clone(), (key == "PRIMARY", !non_unique, vec![]));
        }
        acc.get_mut(&key).unwrap().2.push(col);
    }
    let indexes: Vec<IndexInfo> = order
        .into_iter()
        .map(|k| {
            let (is_primary, is_unique, cols) = acc.remove(&k).unwrap();
            IndexInfo {
                name: k,
                columns: cols.join(", "),
                is_primary,
                is_unique,
            }
        })
        .collect();

    Ok(TableDetail { columns, indexes })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cfg() -> (String, u16, String, String, String) {
        let raw = std::env::var("DBSTUDIO_TEST_MYSQL")
            .unwrap_or_else(|_| "127.0.0.1:3306:root::dbstudio_test".into());
        let p: Vec<&str> = raw.split(':').collect();
        (
            p[0].into(),
            p[1].parse().unwrap_or(3306),
            p[2].into(),
            p.get(3).cloned().unwrap_or("").into(),
            p.get(4).cloned().unwrap_or("dbstudio_test").into(),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn lists_and_table_detail() {
        let (h, port, u, pw, db) = cfg();
        let pool = MyPool::default();
        pool.connect("t", &h, port, &u, &pw, &db).await.unwrap();
        pool.query("t", "DROP TABLE IF EXISTS my_detail_test")
            .await
            .unwrap();
        pool.query("t", "CREATE TABLE my_detail_test(id BIGINT PRIMARY KEY, email VARCHAR(80) NOT NULL DEFAULT 'x@x.com' COMMENT '邮箱', note TEXT)").await.unwrap();
        pool.query(
            "t",
            "CREATE UNIQUE INDEX my_email_idx ON my_detail_test(email)",
        )
        .await
        .unwrap();

        let tables = list_tables(&pool, "t").await.unwrap();
        assert!(tables.contains(&"my_detail_test".to_string()));

        let d = table_detail(&pool, "t", "my_detail_test").await.unwrap();
        let id = d.columns.iter().find(|c| c.name == "id").unwrap();
        assert!(id.is_pk && id.not_null);
        let email = d.columns.iter().find(|c| c.name == "email").unwrap();
        assert!(email.not_null);
        assert_eq!(email.comment.as_deref(), Some("邮箱"));
        assert!(d.indexes.iter().any(|i| i.is_primary && i.columns == "id"));
        let uq = d.indexes.iter().find(|i| i.name == "my_email_idx").unwrap();
        assert!(uq.is_unique && !uq.is_primary && uq.columns == "email");

        pool.query("t", "DROP TABLE my_detail_test").await.unwrap();
    }
}
