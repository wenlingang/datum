use crate::core::connection::SslMode;
use crate::error::{AppError, AppResult, ErrorKind};
use rust_i18n::t;
use serde::Serialize;
use std::collections::HashMap;
use tokio::sync::Mutex;
use tokio_postgres::config::SslMode as PgSslMode;
use tokio_postgres::{Client, Config, SimpleQueryMessage};
use tokio_postgres_rustls::MakeRustlsConnect;

/// 按 SSL 模式设置握手策略:禁用 / 优先(服务器不支持就退回明文)/ 必须。
/// 「校验证书」在握手层面也是「必须」,区别在连接器是否校验证书。
pub fn apply_ssl_mode(cfg: &mut Config, mode: SslMode) {
    cfg.ssl_mode(match mode {
        SslMode::Disable => PgSslMode::Disable,
        SslMode::Prefer => PgSslMode::Prefer,
        SslMode::Require | SslMode::VerifyFull => PgSslMode::Require,
    });
}

fn tls_connector(mode: SslMode) -> MakeRustlsConnect {
    MakeRustlsConnect::new(crate::tls::client_config(mode == SslMode::VerifyFull))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub affected: Option<u64>,
}

/// 双引号转义的标识符。
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// 列的完整类型名(如 `bigint`、`character varying(32)`、`timestamp with time zone`),
/// 取自 pg_attribute/format_type,可安全拼进 SQL 做类型转换。同一批编辑内缓存。
async fn column_type(
    txn: &tokio_postgres::Transaction<'_>,
    cache: &mut HashMap<(String, String), String>,
    table: &str,
    column: &str,
) -> AppResult<String> {
    let key = (table.to_string(), column.to_string());
    if let Some(t) = cache.get(&key) {
        return Ok(t.clone());
    }
    let row = txn
        .query_opt(
            "SELECT format_type(a.atttypid, a.atttypmod) FROM pg_attribute a \
             WHERE a.attrelid = $1::text::regclass AND a.attname = $2 AND NOT a.attisdropped",
            &[&quote_ident(table), &column],
        )
        .await
        .map_err(|e| {
            AppError::new(ErrorKind::Edit, t!("core.err.read_column_type_failed"))
                .with_detail(describe(&e))
        })?
        .ok_or_else(|| {
            AppError::new(
                ErrorKind::Edit,
                t!("core.err.column_not_found", table = table, column = column),
            )
        })?;
    let ty: String = row.get(0);
    cache.insert(key, ty.clone());
    Ok(ty)
}

#[derive(Default)]
pub struct PgPool {
    clients: Mutex<HashMap<String, Client>>,
}

/// tokio-postgres 的 Display 只输出错误类别(如 "db error"),服务器的真实报错
/// (DbError,含 "syntax error"、"permission denied" 等)只在 source 链里。
/// 展开整条链,拼成 "db error: ERROR: relation ... does not exist" 形态。
fn describe(e: &tokio_postgres::Error) -> String {
    use std::error::Error as _;
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(cause) = src {
        s.push_str(": ");
        s.push_str(&cause.to_string());
        src = cause.source();
    }
    s
}

/// 用给定参数尝试建立一次连接以验证可达性与认证(不缓存)。
pub async fn test_connect(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    db: &str,
    ssl: SslMode,
) -> AppResult<()> {
    let mut cfg = Config::new();
    cfg.host(host)
        .port(port)
        .user(user)
        .password(pass)
        .dbname(db);
    apply_ssl_mode(&mut cfg, ssl);
    // 建连成功(完成握手与认证)即视为连通;立即丢弃。
    let (_client, _connection) = cfg.connect(tls_connector(ssl)).await.map_err(|e| {
        AppError::new(
            ErrorKind::Connection,
            t!("core.err.connect_failed", db = "PostgreSQL"),
        )
        .with_detail(describe(&e))
    })?;
    Ok(())
}

impl PgPool {
    /// 用 Config 建立连接并缓存到 id。
    /// 用 Config(显式 setter)而非手拼连接串:空密码或含特殊字符的库名都能正确处理,
    /// 避免 `password= dbname=...` 这类空格分隔串在空值时解析丢失 dbname 的问题。
    /// `config` 需已按 `ssl` 调用过 [`apply_ssl_mode`]。
    pub async fn connect(&self, id: &str, config: &Config, ssl: SslMode) -> AppResult<()> {
        let (client, connection) = config.connect(tls_connector(ssl)).await.map_err(|e| {
            AppError::new(
                ErrorKind::Connection,
                t!("core.err.connect_failed", db = "PostgreSQL"),
            )
            .with_detail(describe(&e))
        })?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        self.clients.lock().await.insert(id.to_string(), client);
        Ok(())
    }

    #[allow(dead_code)] // 目前仅测试使用,保留备将来重连/状态显示
    pub async fn is_connected(&self, id: &str) -> bool {
        self.clients.lock().await.contains_key(id)
    }

    /// 断开并移除缓存的连接(不存在时静默)。Client 被 drop 后其连接任务随之结束,
    /// 服务器侧会话立即终止 —— 删库前强制断开残留连接用。
    pub async fn disconnect(&self, id: &str) {
        self.clients.lock().await.remove(id);
    }

    /// 在一个事务里批量执行编辑与删除(全成功或全回滚)。返回受影响行数。先 UPDATE 后 DELETE。
    pub async fn with_txn(
        &self,
        id: &str,
        edits: &[crate::pg::edit::CellEdit],
        deletes: &[crate::pg::edit::RowDelete],
    ) -> AppResult<u64> {
        let mut guard = self.clients.lock().await;
        let client = guard
            .get_mut(id)
            .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.not_connected")))?;
        let txn = client.transaction().await.map_err(|e| {
            AppError::new(ErrorKind::Edit, t!("core.err.begin_tx_failed")).with_detail(describe(&e))
        })?;
        // 值一律走绑定参数,不拼进 SQL。参数按 text 传,再显式转成列的真实类型
        // (类型名取自系统目录,不来自用户输入);标识符双引号转义。
        let mut types: HashMap<(String, String), String> = HashMap::new();
        let mut n = 0u64;
        for e in edits {
            let col_ty = column_type(&txn, &mut types, &e.table, &e.column).await?;
            let pk_ty = column_type(&txn, &mut types, &e.table, &e.pk_col).await?;
            let sql = format!(
                "UPDATE {} SET {} = $1::text::{col_ty} WHERE {} = $2::text::{pk_ty}",
                quote_ident(&e.table),
                quote_ident(&e.column),
                quote_ident(&e.pk_col),
            );
            let affected = txn
                .execute(&sql, &[&e.new_value, &e.pk_value])
                .await
                .map_err(|err| {
                    AppError::new(ErrorKind::Edit, t!("core.err.update_failed"))
                        .with_detail(describe(&err))
                })?;
            n += affected;
        }
        for d in deletes {
            let pk_ty = column_type(&txn, &mut types, &d.table, &d.pk_col).await?;
            let sql = format!(
                "DELETE FROM {} WHERE {} = $1::text::{pk_ty}",
                quote_ident(&d.table),
                quote_ident(&d.pk_col),
            );
            let affected = txn.execute(&sql, &[&d.pk_value]).await.map_err(|err| {
                AppError::new(ErrorKind::Edit, t!("core.err.delete_failed"))
                    .with_detail(describe(&err))
            })?;
            n += affected;
        }
        txn.commit().await.map_err(|e| {
            AppError::new(ErrorKind::Edit, t!("core.err.commit_tx_failed"))
                .with_detail(describe(&e))
        })?;
        Ok(n)
    }

    /// 在单事务里逐条执行导入 SQL:全成功提交,任一失败整体回滚(错误标注第几条)。
    pub async fn import_sql(&self, id: &str, statements: &[String]) -> AppResult<usize> {
        let mut guard = self.clients.lock().await;
        let client = guard
            .get_mut(id)
            .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.not_connected")))?;
        let txn = client.transaction().await.map_err(|e| {
            AppError::new(ErrorKind::Query, t!("core.err.begin_tx_failed"))
                .with_detail(describe(&e))
        })?;
        for (i, sql) in statements.iter().enumerate() {
            txn.simple_query(sql).await.map_err(|e| {
                AppError::new(
                    ErrorKind::Query,
                    t!("core.err.stmt_failed_all_rolled_back", n = i + 1),
                )
                .with_detail(describe(&e))
            })?;
        }
        txn.commit().await.map_err(|e| {
            AppError::new(ErrorKind::Query, t!("core.err.commit_tx_failed"))
                .with_detail(describe(&e))
        })?;
        Ok(statements.len())
    }

    /// 执行任意 SQL,文本协议返回。
    pub async fn query(&self, id: &str, sql: &str) -> AppResult<QueryResult> {
        self.query_capped(id, sql, None).await
    }

    /// 同 query,但结果行数不超过 max_rows(None = 不限)。
    /// 用 simple_query_raw 流式消费:超出上限的行原样丢弃,峰值内存 O(max_rows)。
    /// (简单协议服务器仍会发送全部行,网络传输不可省;省的是物化与转换。)
    pub async fn query_capped(
        &self,
        id: &str,
        sql: &str,
        max_rows: Option<u32>,
    ) -> AppResult<QueryResult> {
        let guard = self.clients.lock().await;
        let client = guard
            .get(id)
            .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.not_connected")))?;
        run_capped(client, sql, max_rows).await
    }

    /// 只读执行:切只读、执行、恢复三步在同一把锁里完成,
    /// 期间同一连接上的其他请求(包括另一次只读执行的「恢复」)插不进来。
    pub async fn query_read_only(
        &self,
        id: &str,
        sql: &str,
        max_rows: Option<u32>,
    ) -> AppResult<QueryResult> {
        let guard = self.clients.lock().await;
        let client = guard
            .get(id)
            .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.not_connected")))?;
        run_capped(client, "SET default_transaction_read_only = on", None).await?;
        let result = run_capped(client, sql, max_rows).await;
        // 语句可能留下中止的事务,先回滚再恢复,否则 RESET 会因事务已中止而失败
        let _ = client.batch_execute("ROLLBACK").await;
        run_capped(client, "RESET default_transaction_read_only", None).await?;
        result
    }
}

/// 在已拿到的连接上执行 SQL(调用方持有连接锁)。
async fn run_capped(client: &Client, sql: &str, max_rows: Option<u32>) -> AppResult<QueryResult> {
    use futures_util::StreamExt;
    {
        let stream = client.simple_query_raw(sql).await.map_err(|e| {
            AppError::new(ErrorKind::Query, t!("core.err.query_failed")).with_detail(describe(&e))
        })?;
        let mut stream = std::pin::pin!(stream);

        let cap = max_rows.map(|n| n as usize).unwrap_or(usize::MAX);
        let mut columns: Vec<String> = vec![];
        let mut rows: Vec<Vec<Option<String>>> = vec![];
        let mut affected: Option<u64> = None;
        while let Some(m) = stream.next().await {
            let m = m.map_err(|e| {
                AppError::new(ErrorKind::Query, t!("core.err.query_failed"))
                    .with_detail(describe(&e))
            })?;
            match m {
                // 列名以 RowDescription 为准:0 行结果也有它,空表才能渲染出表头
                SimpleQueryMessage::RowDescription(cols) => {
                    if columns.is_empty() {
                        columns = cols.iter().map(|c| c.name().to_string()).collect();
                    }
                }
                SimpleQueryMessage::Row(r) => {
                    if columns.is_empty() {
                        columns = r.columns().iter().map(|c| c.name().to_string()).collect();
                    }
                    if rows.len() < cap {
                        let row = (0..r.len())
                            .map(|i| r.get(i).map(|s| s.to_string()))
                            .collect();
                        rows.push(row);
                    }
                }
                SimpleQueryMessage::CommandComplete(n) => affected = Some(n),
                _ => {}
            }
        }
        Ok(QueryResult {
            columns,
            rows,
            affected,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_config() -> Config {
        std::env::var("DBSTUDIO_TEST_PG")
            .unwrap_or_else(|_| {
                "host=127.0.0.1 port=5432 user=postgres password=postgres dbname=postgres".into()
            })
            .parse()
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn connects_and_queries_users() {
        let pool = PgPool::default();
        pool.connect("t", &test_config(), SslMode::Disable)
            .await
            .unwrap();
        assert!(pool.is_connected("t").await);
        let r = pool
            .query("t", "SELECT id, name FROM users ORDER BY id")
            .await
            .unwrap();
        assert_eq!(r.columns, vec!["id", "name"]);
        assert_eq!(r.rows[0], vec![Some("1".into()), Some("Alice".into())]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn max_rows_caps_result() {
        let pool = PgPool::default();
        pool.connect("t", &test_config(), SslMode::Disable)
            .await
            .unwrap();
        let capped = pool
            .query_capped("t", "SELECT generate_series(1, 10)", Some(4))
            .await
            .unwrap();
        assert_eq!(capped.rows.len(), 4);
        let all = pool
            .query_capped("t", "SELECT generate_series(1, 10)", None)
            .await
            .unwrap();
        assert_eq!(all.rows.len(), 10);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn query_error_surfaces_as_app_error() {
        let pool = PgPool::default();
        pool.connect("t", &test_config(), SslMode::Disable)
            .await
            .unwrap();
        let err = pool
            .query("t", "SELECT * FROM no_such_table")
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Query);
    }

    // 回归:tokio-postgres 的 Display 只输出错误类别("db error"),服务器的真实报错
    // (如 "relation ... does not exist")在 source 链里。detail 必须包含服务器消息,
    // 否则任何 DDL/DML 失败用户都只能看到无意义的 "db error"。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn query_error_detail_includes_server_message() {
        let pool = PgPool::default();
        pool.connect("t", &test_config(), SslMode::Disable)
            .await
            .unwrap();
        let err = pool
            .query("t", "CREATE INDEX idx_x ON no_such_table (id)")
            .await
            .unwrap_err();
        let detail = err.detail.unwrap_or_default();
        assert!(
            detail.contains("no_such_table"),
            "detail 应包含服务器报错,实际: {detail}"
        );
    }

    // 空表(0 行)查询也必须返回列名:前端要用它渲染空网格的表头。
    // 列名来自 RowDescription 消息,不能只依赖数据行。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn empty_result_still_returns_columns() {
        let pool = PgPool::default();
        pool.connect("t", &test_config(), SslMode::Disable)
            .await
            .unwrap();
        pool.query("t", "DROP TABLE IF EXISTS dbstudio_empty_test")
            .await
            .unwrap();
        pool.query("t", "CREATE TABLE dbstudio_empty_test(id INT, name TEXT)")
            .await
            .unwrap();
        let r = pool
            .query("t", "SELECT * FROM dbstudio_empty_test")
            .await
            .unwrap();
        assert_eq!(r.columns, vec!["id", "name"]);
        assert!(r.rows.is_empty());
        pool.query("t", "DROP TABLE dbstudio_empty_test")
            .await
            .unwrap();
    }

    // CREATE INDEX 等 DDL 走 simple_query 应正常执行并返回 CommandComplete。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn create_index_executes() {
        let pool = PgPool::default();
        pool.connect("t", &test_config(), SslMode::Disable)
            .await
            .unwrap();
        pool.query("t", "DROP TABLE IF EXISTS dbstudio_idx_test")
            .await
            .unwrap();
        pool.query("t", "CREATE TABLE dbstudio_idx_test(id INT, name TEXT)")
            .await
            .unwrap();
        pool.query(
            "t",
            "CREATE INDEX idx_dbstudio_idx_test_id ON dbstudio_idx_test(id)",
        )
        .await
        .unwrap();
        // 建成后确实可见
        let r = pool
            .query(
                "t",
                "SELECT indexname FROM pg_indexes WHERE tablename = 'dbstudio_idx_test'",
            )
            .await
            .unwrap();
        assert!(r
            .rows
            .iter()
            .any(|row| row[0].as_deref() == Some("idx_dbstudio_idx_test_id")));
        pool.query("t", "DROP TABLE dbstudio_idx_test")
            .await
            .unwrap();
    }

    // 回归:空密码时 dbname 必须被尊重。旧实现手拼 `password= dbname=...` 连接串,
    // 空密码会让 dbname 丢失、回退到默认 postgres 库。改用 Config 后必须连到指定库。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn empty_password_respects_dbname() {
        let admin = PgPool::default();
        let mut admin_cfg = Config::new();
        admin_cfg
            .host("127.0.0.1")
            .port(5432)
            .user("postgres")
            .dbname("postgres")
            .password("");
        admin
            .connect("admin", &admin_cfg, SslMode::Disable)
            .await
            .unwrap();
        admin
            .query("admin", "DROP DATABASE IF EXISTS dbstudio_dbname_test")
            .await
            .ok();
        admin
            .query("admin", "CREATE DATABASE dbstudio_dbname_test")
            .await
            .unwrap();

        let pool = PgPool::default();
        let mut cfg = Config::new();
        cfg.host("127.0.0.1")
            .port(5432)
            .user("postgres")
            .dbname("dbstudio_dbname_test")
            .password("");
        pool.connect("t", &cfg, SslMode::Disable).await.unwrap();
        let r = pool.query("t", "SELECT current_database()").await.unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("dbstudio_dbname_test"));
    }
}
