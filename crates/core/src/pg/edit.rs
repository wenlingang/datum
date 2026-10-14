use crate::error::AppResult;
use crate::pg::client::PgPool;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellEdit {
    pub table: String,
    pub pk_col: String,
    pub pk_value: String,
    pub column: String,
    pub new_value: String,
}

/// 待删除的行:按主键定位。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowDelete {
    pub table: String,
    pub pk_col: String,
    pub pk_value: String,
}

/// 批量提交编辑与删除:单事务,全成功或全回滚。返回受影响行数。
pub async fn commit_edits(
    pool: &PgPool,
    id: &str,
    edits: &[CellEdit],
    deletes: &[RowDelete],
) -> AppResult<u64> {
    if edits.is_empty() && deletes.is_empty() {
        return Ok(0);
    }
    pool.with_txn(id, edits, deletes).await
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
    async fn commits_a_cell_edit() {
        let pool = PgPool::default();
        pool.connect("t", &test_config()).await.unwrap();
        let edits = vec![CellEdit {
            table: "users".into(),
            pk_col: "id".into(),
            pk_value: "2".into(),
            column: "name".into(),
            new_value: "Bobby".into(),
        }];
        let n = commit_edits(&pool, "t", &edits, &[]).await.unwrap();
        assert_eq!(n, 1);
        let r = pool
            .query("t", "SELECT name FROM users WHERE id=2")
            .await
            .unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("Bobby"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn bad_edit_rolls_back() {
        let pool = PgPool::default();
        pool.connect("t", &test_config()).await.unwrap();
        // 先确保 id=1 的 email 是已知值
        pool.query("t", "UPDATE users SET email='a@x.com' WHERE id=1")
            .await
            .unwrap();
        // id 是 int8,写入非数字会失败 → 整批回滚
        let edits = vec![
            CellEdit {
                table: "users".into(),
                pk_col: "id".into(),
                pk_value: "1".into(),
                column: "email".into(),
                new_value: "changed@x.com".into(),
            },
            CellEdit {
                table: "users".into(),
                pk_col: "id".into(),
                pk_value: "1".into(),
                column: "id".into(),
                new_value: "not-a-number".into(),
            },
        ];
        assert!(commit_edits(&pool, "t", &edits, &[]).await.is_err());
        let r = pool
            .query("t", "SELECT email FROM users WHERE id=1")
            .await
            .unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("a@x.com")); // 第一条也被回滚
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn deletes_rows_in_txn() {
        let pool = PgPool::default();
        pool.connect("t", &test_config()).await.unwrap();
        pool.query("t", "DROP TABLE IF EXISTS dbstudio_deltest")
            .await
            .unwrap();
        pool.query(
            "t",
            "CREATE TABLE dbstudio_deltest(id int8 PRIMARY KEY, name text)",
        )
        .await
        .unwrap();
        pool.query(
            "t",
            "INSERT INTO dbstudio_deltest VALUES (1,'a'),(2,'b'),(3,'c')",
        )
        .await
        .unwrap();
        // 编辑 + 删除同一事务:改 1 行、删 2 行
        let edits = vec![CellEdit {
            table: "dbstudio_deltest".into(),
            pk_col: "id".into(),
            pk_value: "1".into(),
            column: "name".into(),
            new_value: "aa".into(),
        }];
        let dels = vec![
            RowDelete {
                table: "dbstudio_deltest".into(),
                pk_col: "id".into(),
                pk_value: "2".into(),
            },
            RowDelete {
                table: "dbstudio_deltest".into(),
                pk_col: "id".into(),
                pk_value: "3".into(),
            },
        ];
        assert_eq!(commit_edits(&pool, "t", &edits, &dels).await.unwrap(), 3);
        let r = pool
            .query("t", "SELECT id, name FROM dbstudio_deltest ORDER BY id")
            .await
            .unwrap();
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0], vec![Some("1".into()), Some("aa".into())]);
        pool.query("t", "DROP TABLE dbstudio_deltest")
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn edits_a_numeric_column() {
        let pool = PgPool::default();
        pool.connect("t", &test_config()).await.unwrap();
        pool.query("t", "DROP TABLE IF EXISTS dbstudio_numtest")
            .await
            .unwrap();
        pool.query(
            "t",
            "CREATE TABLE dbstudio_numtest(id int8 PRIMARY KEY, qty int4)",
        )
        .await
        .unwrap();
        pool.query("t", "INSERT INTO dbstudio_numtest VALUES (1, 10)")
            .await
            .unwrap();
        let edits = vec![CellEdit {
            table: "dbstudio_numtest".into(),
            pk_col: "id".into(),
            pk_value: "1".into(),
            column: "qty".into(),
            new_value: "42".into(),
        }];
        let n = commit_edits(&pool, "t", &edits, &[]).await.unwrap();
        assert_eq!(n, 1);
        let r = pool
            .query("t", "SELECT qty FROM dbstudio_numtest WHERE id=1")
            .await
            .unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("42"));
        pool.query("t", "DROP TABLE dbstudio_numtest")
            .await
            .unwrap();
    }

    /// 值走绑定参数 + 按列真实类型转换:各种类型都能写,引号/反斜杠/注入尝试按原样存成数据。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn typed_values_and_hostile_strings() {
        let pool = PgPool::default();
        pool.connect("t", &test_config()).await.unwrap();
        pool.query("t", "DROP TABLE IF EXISTS dbstudio_types")
            .await
            .unwrap();
        pool.query(
            "t",
            "CREATE TABLE \"dbstudio_types\"(id uuid PRIMARY KEY, n int4, t timestamptz, \
             b boolean, m numeric(10,2), j jsonb, a int[], s varchar(64))",
        )
        .await
        .unwrap();
        let id = "6f1c2c1e-2d7e-4f7a-9d55-0d6a3c2b1a90";
        pool.query(
            "t",
            &format!("INSERT INTO dbstudio_types(id) VALUES ('{id}')"),
        )
        .await
        .unwrap();
        let set = |column: &str, value: &str| CellEdit {
            table: "dbstudio_types".into(),
            pk_col: "id".into(),
            pk_value: id.into(),
            column: column.into(),
            new_value: value.into(),
        };
        let hostile = r"it's \'; DROP TABLE dbstudio_types; --";
        let edits = vec![
            set("n", "42"),
            set("t", "2025-01-02 03:04:05+00"),
            set("b", "true"),
            set("m", "12.30"),
            set("j", r#"{"k": [1, "x"]}"#),
            set("a", "{1,2,3}"),
            set("s", hostile),
        ];
        assert_eq!(commit_edits(&pool, "t", &edits, &[]).await.unwrap(), 7);
        let r = pool
            .query(
                "t",
                "SELECT n::text, (t AT TIME ZONE 'UTC')::text, b::text, m::text, j::text, a::text, s \
                 FROM dbstudio_types",
            )
            .await
            .unwrap();
        let row: Vec<_> = r.rows[0].iter().map(|v| v.clone().unwrap()).collect();
        assert_eq!(
            row,
            [
                "42",
                "2025-01-02 03:04:05",
                "true",
                "12.30",
                r#"{"k": [1, "x"]}"#,
                "{1,2,3}",
                hostile
            ]
        );
        // 主键为 uuid 也能按类型定位删除
        let del = RowDelete {
            table: "dbstudio_types".into(),
            pk_col: "id".into(),
            pk_value: id.into(),
        };
        assert_eq!(commit_edits(&pool, "t", &[], &[del]).await.unwrap(), 1);
        // 不存在的列给出明确错误
        assert!(commit_edits(&pool, "t", &[set("nope", "1")], &[])
            .await
            .is_err());
        pool.query("t", "DROP TABLE dbstudio_types").await.unwrap();
    }
}
