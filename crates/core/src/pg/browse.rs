use crate::error::AppResult;
use crate::pg::client::PgPool;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub default: Option<String>,
    pub comment: Option<String>,
    pub not_null: bool,
    pub is_pk: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexInfo {
    pub name: String,
    pub columns: String, // 逗号分隔的列名
    pub is_primary: bool,
    pub is_unique: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableDetail {
    pub columns: Vec<ColumnInfo>,
    pub indexes: Vec<IndexInfo>,
}

/// 列出实例下的库名(排除模板库)。
pub async fn list_databases(pool: &PgPool, id: &str) -> AppResult<Vec<String>> {
    let r = pool
        .query(
            id,
            "SELECT datname FROM pg_database WHERE NOT datistemplate ORDER BY datname",
        )
        .await?;
    Ok(r.rows
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect())
}

/// 列出 public schema 下的表名。
pub async fn list_tables(pool: &PgPool, id: &str) -> AppResult<Vec<String>> {
    let r = pool
        .query(
            id,
            "SELECT tablename FROM pg_tables WHERE schemaname='public' ORDER BY tablename",
        )
        .await?;
    Ok(r.rows
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect())
}

/// simple_query 把布尔以 't'/'f' 文本返回。
fn truthy(cell: Option<&Option<String>>) -> bool {
    matches!(cell, Some(Some(s)) if s == "t")
}

/// 表详情:列定义(名/类型/默认值/注释/非空/主键) + 索引(名/列/主键/唯一)。
pub async fn table_detail(pool: &PgPool, id: &str, table: &str) -> AppResult<TableDetail> {
    let reg = format!("'public.\"{}\"'::regclass", table.replace('"', "\"\""));

    let cols_sql = format!(
        "SELECT a.attname, format_type(a.atttypid, a.atttypmod), \
         pg_get_expr(d.adbin, d.adrelid), a.attnotnull, \
         col_description(a.attrelid, a.attnum), COALESCE(i.indisprimary, false) \
         FROM pg_attribute a \
         LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum \
         LEFT JOIN pg_index i ON i.indrelid=a.attrelid AND a.attnum=ANY(i.indkey) AND i.indisprimary \
         WHERE a.attrelid={reg} AND a.attnum>0 AND NOT a.attisdropped \
         ORDER BY a.attnum");
    let cr = pool.query(id, &cols_sql).await?;
    let columns = cr
        .rows
        .into_iter()
        .map(|r| ColumnInfo {
            name: r.first().cloned().flatten().unwrap_or_default(),
            data_type: r.get(1).cloned().flatten().unwrap_or_default(),
            default: r.get(2).cloned().flatten(),
            not_null: truthy(r.get(3)),
            comment: r.get(4).cloned().flatten(),
            is_pk: truthy(r.get(5)),
        })
        .collect();

    let idx_sql = format!(
        "SELECT i.relname, ix.indisprimary, ix.indisunique, \
         array_to_string(array_agg(a.attname ORDER BY k.ord), ', ') \
         FROM pg_index ix \
         JOIN pg_class i ON i.oid=ix.indexrelid \
         CROSS JOIN LATERAL unnest(string_to_array(ix.indkey::text, ' ')::int[]) WITH ORDINALITY AS k(attnum, ord) \
         JOIN pg_attribute a ON a.attrelid=ix.indrelid AND a.attnum=k.attnum \
         WHERE ix.indrelid={reg} \
         GROUP BY i.relname, ix.indisprimary, ix.indisunique \
         ORDER BY ix.indisprimary DESC, i.relname");
    let ir = pool.query(id, &idx_sql).await?;
    let indexes = ir
        .rows
        .into_iter()
        .map(|r| IndexInfo {
            name: r.first().cloned().flatten().unwrap_or_default(),
            is_primary: truthy(r.get(1)),
            is_unique: truthy(r.get(2)),
            columns: r.get(3).cloned().flatten().unwrap_or_default(),
        })
        .collect();

    Ok(TableDetail { columns, indexes })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_config() -> tokio_postgres::Config {
        std::env::var("DBSTUDIO_TEST_PG")
            .unwrap_or_else(|_| {
                "host=127.0.0.1 port=5432 user=postgres password=postgres dbname=postgres".into()
            })
            .parse()
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn lists_tables_and_full_detail() {
        let pool = PgPool::default();
        pool.connect(
            "t",
            &test_config(),
            crate::core::connection::SslMode::Disable,
        )
        .await
        .unwrap();
        pool.query("t", "DROP TABLE IF EXISTS dbstudio_detail_test")
            .await
            .unwrap();
        pool.query("t", "CREATE TABLE dbstudio_detail_test(id int8 PRIMARY KEY, email text NOT NULL DEFAULT 'x@x.com', note text)").await.unwrap();
        pool.query(
            "t",
            "COMMENT ON COLUMN dbstudio_detail_test.email IS '邮箱'",
        )
        .await
        .unwrap();
        pool.query(
            "t",
            "CREATE UNIQUE INDEX dbstudio_detail_email_idx ON dbstudio_detail_test(email)",
        )
        .await
        .unwrap();

        let tables = list_tables(&pool, "t").await.unwrap();
        assert!(tables.contains(&"dbstudio_detail_test".to_string()));

        let d = table_detail(&pool, "t", "dbstudio_detail_test")
            .await
            .unwrap();
        let id_col = d.columns.iter().find(|c| c.name == "id").unwrap();
        assert!(id_col.is_pk && id_col.not_null);
        let email = d.columns.iter().find(|c| c.name == "email").unwrap();
        assert!(email.not_null);
        assert_eq!(email.comment.as_deref(), Some("邮箱"));
        assert!(email.default.as_deref().unwrap_or("").contains("x@x.com"));
        let note = d.columns.iter().find(|c| c.name == "note").unwrap();
        assert!(!note.not_null && !note.is_pk);

        assert!(d.indexes.iter().any(|i| i.is_primary && i.columns == "id"));
        let uq = d
            .indexes
            .iter()
            .find(|i| i.name == "dbstudio_detail_email_idx")
            .unwrap();
        assert!(uq.is_unique && !uq.is_primary && uq.columns == "email");

        pool.query("t", "DROP TABLE dbstudio_detail_test")
            .await
            .unwrap();
    }
}
