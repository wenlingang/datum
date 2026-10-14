use crate::error::{AppError, AppResult, ErrorKind};
use crate::pg::client::QueryResult;
use mysql_async::prelude::Queryable;
use mysql_async::{Opts, OptsBuilder, Pool, Row, TxOpts, Value};
use rust_i18n::t;
use std::collections::HashMap;
use tokio::sync::Mutex;

#[derive(Default)]
pub struct MyPool {
    pools: Mutex<HashMap<String, Pool>>,
}

fn connerr(e: impl std::fmt::Display) -> AppError {
    AppError::new(
        ErrorKind::Connection,
        t!("core.err.connect_failed", db = "MySQL"),
    )
    .with_detail(e.to_string())
}
fn qerr(e: impl std::fmt::Display) -> AppError {
    AppError::new(ErrorKind::Query, t!("core.err.query_failed")).with_detail(e.to_string())
}
fn ederr(e: impl std::fmt::Display) -> AppError {
    AppError::new(ErrorKind::Edit, t!("core.err.update_failed")).with_detail(e.to_string())
}

/// 反引号标识符转义。
pub fn esc_ident(s: &str) -> String {
    s.replace('`', "``")
}
/// 字符串字面量转义(MySQL 默认开启反斜杠转义)。
pub fn esc_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "''")
}

/// 把 MySQL 值转成展示字符串;None 表示 SQL NULL。
fn val_to_string(v: &Value) -> Option<String> {
    match v {
        Value::NULL => None,
        Value::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
        Value::Int(i) => Some(i.to_string()),
        Value::UInt(u) => Some(u.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Double(d) => Some(d.to_string()),
        Value::Date(y, mo, d, h, mi, s, us) => {
            let base = format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, mi, s);
            Some(if *us > 0 {
                format!("{base}.{us:06}")
            } else {
                base
            })
        }
        Value::Time(neg, d, h, mi, s, us) => {
            let sign = if *neg { "-" } else { "" };
            let base = format!(
                "{sign}{:03}:{:02}:{:02}",
                (*d as u32) * 24 + *h as u32,
                mi,
                s
            );
            Some(if *us > 0 {
                format!("{base}.{us:06}")
            } else {
                base
            })
        }
    }
}

fn row_to_strings(row: &Row) -> Vec<Option<String>> {
    (0..row.len())
        .map(|i| row.as_ref(i).and_then(val_to_string))
        .collect()
}

/// 用给定参数尝试建立一次连接以验证可达性与认证(不缓存)。
pub async fn test_connect(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    db: &str,
) -> AppResult<()> {
    let mut b = OptsBuilder::default()
        .ip_or_hostname(host.to_string())
        .tcp_port(port)
        .user(Some(user.to_string()))
        .db_name(Some(db.to_string()));
    if !pass.is_empty() {
        b = b.pass(Some(pass.to_string()));
    }
    let pool = Pool::new(Opts::from(b));
    let conn = pool.get_conn().await.map_err(connerr)?;
    drop(conn);
    pool.disconnect().await.ok();
    Ok(())
}

impl MyPool {
    pub async fn connect(
        &self,
        id: &str,
        host: &str,
        port: u16,
        user: &str,
        pass: &str,
        db: &str,
    ) -> AppResult<()> {
        let mut b = OptsBuilder::default()
            .ip_or_hostname(host.to_string())
            .tcp_port(port)
            .user(Some(user.to_string()))
            .db_name(Some(db.to_string()));
        if !pass.is_empty() {
            b = b.pass(Some(pass.to_string()));
        }
        let pool = Pool::new(Opts::from(b));
        // 试连一次,确保配置可用
        let conn = pool.get_conn().await.map_err(connerr)?;
        drop(conn);
        self.pools.lock().await.insert(id.to_string(), pool);
        Ok(())
    }

    async fn pool(&self, id: &str) -> AppResult<Pool> {
        self.pools
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.not_connected")))
    }

    /// 断开并移除连接池(不存在时静默):先移出映射再优雅关闭,
    /// 服务器侧会话随之结束 —— 删库前强制断开残留连接用。
    pub async fn disconnect(&self, id: &str) {
        let pool = self.pools.lock().await.remove(id);
        if let Some(p) = pool {
            let _ = p.disconnect().await;
        }
    }

    /// 执行任意 SQL,值统一转为文本。
    /// 只读执行:连接池里每次取到的连接可能不同,会话级只读设置必须和语句在同一条连接上,
    /// 并且归还连接前恢复读写,否则池里的这条连接会一直只读。
    pub async fn query_read_only(
        &self,
        id: &str,
        sql: &str,
        max_rows: Option<u32>,
    ) -> AppResult<QueryResult> {
        let pool = self.pool(id).await?;
        let mut conn = pool.get_conn().await.map_err(qerr)?;
        conn.query_drop("SET SESSION TRANSACTION READ ONLY")
            .await
            .map_err(qerr)?;
        let result = Self::run_capped(&mut conn, sql, max_rows).await;
        conn.query_drop("SET SESSION TRANSACTION READ WRITE")
            .await
            .map_err(qerr)?;
        result
    }

    pub async fn query(&self, id: &str, sql: &str) -> AppResult<QueryResult> {
        self.query_capped(id, sql, None).await
    }

    /// 同 query,但结果行数不超过 max_rows(None = 不限)。
    /// 逐行流式读取:上限之内才转换存储,之后的行读完即丢(排空结果集保持协议干净、
    /// affected_rows 可靠),峰值内存 O(max_rows)。
    pub async fn query_capped(
        &self,
        id: &str,
        sql: &str,
        max_rows: Option<u32>,
    ) -> AppResult<QueryResult> {
        let pool = self.pool(id).await?;
        let mut conn = pool.get_conn().await.map_err(qerr)?;
        Self::run_capped(&mut conn, sql, max_rows).await
    }

    async fn run_capped(
        conn: &mut mysql_async::Conn,
        sql: &str,
        max_rows: Option<u32>,
    ) -> AppResult<QueryResult> {
        let mut result = conn.query_iter(sql).await.map_err(qerr)?;
        let columns: Vec<String> = result
            .columns()
            .map(|cs| cs.iter().map(|c| c.name_str().to_string()).collect())
            .unwrap_or_default();
        let cap = max_rows.map(|n| n as usize).unwrap_or(usize::MAX);
        let mut rows: Vec<Vec<Option<String>>> = vec![];
        while let Some(row) = result.next().await.map_err(qerr)? {
            if rows.len() < cap {
                rows.push(row_to_strings(&row));
            }
        }
        let affected = result.affected_rows();
        Ok(QueryResult {
            columns,
            rows,
            affected: Some(affected),
        })
    }

    /// 在单事务里逐条执行导入 SQL:全成功提交,任一失败回滚(错误标注第几条)。
    /// 注意 MySQL 的 DDL 会隐式提交,含 DDL 的导入无法保证整体原子。
    pub async fn import_sql(&self, id: &str, statements: &[String]) -> AppResult<usize> {
        let pool = self.pool(id).await?;
        let mut conn = pool.get_conn().await.map_err(qerr)?;
        let mut tx = conn
            .start_transaction(TxOpts::default())
            .await
            .map_err(qerr)?;
        for (i, sql) in statements.iter().enumerate() {
            if let Err(e) = tx.query_drop(sql).await {
                let detail = e.to_string();
                let _ = tx.rollback().await;
                return Err(AppError::new(
                    ErrorKind::Query,
                    t!("core.err.stmt_failed_rolled_back", n = i + 1),
                )
                .with_detail(detail));
            }
        }
        tx.commit().await.map_err(qerr)?;
        Ok(statements.len())
    }

    /// 批量编辑与删除:单事务内先逐条 UPDATE 再逐条 DELETE,全成功提交、任一失败回滚。
    pub async fn commit_edits(
        &self,
        id: &str,
        edits: &[crate::pg::edit::CellEdit],
        deletes: &[crate::pg::edit::RowDelete],
    ) -> AppResult<u64> {
        if edits.is_empty() && deletes.is_empty() {
            return Ok(0);
        }
        let pool = self.pool(id).await?;
        let mut conn = pool.get_conn().await.map_err(ederr)?;
        let mut tx = conn
            .start_transaction(TxOpts::default())
            .await
            .map_err(ederr)?;
        let mut n = 0u64;
        for e in edits {
            let sql = format!(
                "UPDATE `{}` SET `{}` = '{}' WHERE `{}` = '{}'",
                esc_ident(&e.table),
                esc_ident(&e.column),
                esc_str(&e.new_value),
                esc_ident(&e.pk_col),
                esc_str(&e.pk_value),
            );
            tx.query_drop(&sql).await.map_err(ederr)?;
            n += tx.affected_rows();
        }
        for d in deletes {
            let sql = format!(
                "DELETE FROM `{}` WHERE `{}` = '{}'",
                esc_ident(&d.table),
                esc_ident(&d.pk_col),
                esc_str(&d.pk_value),
            );
            tx.query_drop(&sql).await.map_err(ederr)?;
            n += tx.affected_rows();
        }
        tx.commit().await.map_err(ederr)?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cfg() -> (String, u16, String, String, String) {
        // DBSTUDIO_TEST_MYSQL=host:port:user:pass:db
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
    async fn connect_query_edit() {
        let (h, port, u, pw, db) = cfg();
        let pool = MyPool::default();
        pool.connect("t", &h, port, &u, &pw, &db).await.unwrap();
        pool.query("t", "DROP TABLE IF EXISTS my_test")
            .await
            .unwrap();
        pool.query(
            "t",
            "CREATE TABLE my_test(id BIGINT PRIMARY KEY, name VARCHAR(50), qty INT)",
        )
        .await
        .unwrap();
        pool.query(
            "t",
            "INSERT INTO my_test VALUES (1,'Alice',10),(2,'Bob',20)",
        )
        .await
        .unwrap();

        let r = pool
            .query("t", "SELECT id, name FROM my_test ORDER BY id")
            .await
            .unwrap();
        assert_eq!(r.columns, vec!["id", "name"]);
        assert_eq!(r.rows[0], vec![Some("1".into()), Some("Alice".into())]);

        let edits = vec![crate::pg::edit::CellEdit {
            table: "my_test".into(),
            pk_col: "id".into(),
            pk_value: "2".into(),
            column: "name".into(),
            new_value: "Bobby".into(),
        }];
        assert_eq!(pool.commit_edits("t", &edits, &[]).await.unwrap(), 1);
        let r2 = pool
            .query("t", "SELECT name FROM my_test WHERE id=2")
            .await
            .unwrap();
        assert_eq!(r2.rows[0][0].as_deref(), Some("Bobby"));

        pool.query("t", "DROP TABLE my_test").await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn max_rows_caps_result() {
        let (h, port, u, pw, db) = cfg();
        let pool = MyPool::default();
        pool.connect("t", &h, port, &u, &pw, &db).await.unwrap();
        pool.query("t", "DROP TABLE IF EXISTS my_cap_test")
            .await
            .unwrap();
        pool.query("t", "CREATE TABLE my_cap_test(i INT)")
            .await
            .unwrap();
        pool.query("t", "INSERT INTO my_cap_test VALUES (1),(2),(3),(4),(5)")
            .await
            .unwrap();
        let capped = pool
            .query_capped("t", "SELECT i FROM my_cap_test", Some(2))
            .await
            .unwrap();
        assert_eq!(capped.rows.len(), 2);
        let all = pool
            .query_capped("t", "SELECT i FROM my_cap_test", None)
            .await
            .unwrap();
        assert_eq!(all.rows.len(), 5);
        pool.query("t", "DROP TABLE my_cap_test").await.unwrap();
    }
}
