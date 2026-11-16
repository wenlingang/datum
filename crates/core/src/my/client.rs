use crate::core::connection::SslMode;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::pg::client::QueryResult;
use crate::ssh::Target;
use mysql_async::prelude::Queryable;
use mysql_async::{Opts, OptsBuilder, Pool, Row, SslOpts, TxOpts, Value};
use rust_i18n::t;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// 每个连接一条固定的会话(编辑器的语句、影响预估、撤销快照、AI 查询都在这里),
/// 这样 BEGIN 之后的语句仍在同一个事务里,SET 的会话变量、临时表也都有效;
/// 元数据、诊断走连接池,不排在会话上的长查询后面。
#[derive(Default)]
struct Session {
    conn: Arc<Mutex<Option<mysql_async::Conn>>>,
    /// 会话连接的服务器线程号(「停止」时 KILL QUERY 它,不用拿会话锁)
    thread: std::sync::atomic::AtomicU32,
}

#[derive(Default)]
pub struct MyPool {
    pools: Mutex<HashMap<String, Pool>>,
    sessions: std::sync::Mutex<HashMap<String, Arc<Session>>>,
}

/// 在给定连接上只读执行。`guard`:切成只读会话,由服务器拒绝写入(事务进行中时做不到,传 false)。
/// 结束后恢复读写(含 `SET TRANSACTION` 留下的「下一个事务只读」),否则这条连接之后一直只读。
async fn read_only_on(
    conn: &mut mysql_async::Conn,
    sql: &str,
    max_rows: Option<u32>,
    timeout_ms: Option<u32>,
    guard: bool,
) -> AppResult<QueryResult> {
    if guard {
        conn.query_drop("SET SESSION TRANSACTION READ ONLY").await.map_err(qerr)?;
    }
    let limited = match timeout_ms {
        Some(ms) => conn.query_drop(format!("SET SESSION max_execution_time = {ms}")).await.is_ok(),
        None => false,
    };
    let result = MyPool::run_capped(conn, sql, max_rows).await;
    if limited {
        conn.query_drop("SET SESSION max_execution_time = DEFAULT").await.map_err(qerr)?;
    }
    if guard {
        conn.query_drop("SET SESSION TRANSACTION READ WRITE").await.map_err(qerr)?;
        conn.query_drop("SET TRANSACTION READ WRITE").await.map_err(qerr)?;
    }
    result
}

/// 建连(含 TLS 握手与认证)最多等多久。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

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
/// 走 SSH 隧道时连本地转发端口,证书仍按数据库原主机名校验。
fn opts(target: &Target, user: &str, pass: &str, db: &str, ssl: Option<SslOpts>) -> Opts {
    let ssl = ssl.map(|o| {
        if target.tunneled() {
            o.with_danger_tls_hostname_override(Some(target.tls_host.clone()))
        } else {
            o
        }
    });
    let mut b = OptsBuilder::default()
        .ip_or_hostname(target.host.clone())
        .tcp_port(target.port)
        .user(Some(user.to_string()))
        .db_name(Some(db.to_string()))
        .ssl_opts(ssl)
        // 空闲连接每分钟探测一次:NAT / 防火墙悄悄断开的连接能尽早发现
        .tcp_keepalive(Some(60_000u32));
    // 经隧道的 Unix 套接字(只有本用户能连):驱动直接连它
    if let Some(crate::ssh::Via::Socket(path)) = &target.via {
        b = b.socket(Some(path.display().to_string())).prefer_socket(true);
    }
    if !pass.is_empty() {
        b = b.pass(Some(pass.to_string()));
    }
    Opts::from(b)
}

/// 按 SSL 模式依次尝试的加密选项:None 表示明文。
/// 「优先」先试加密(不校验证书),服务器不支持再退回明文;「必须」「校验证书」失败即报错,不降级。
fn ssl_attempts(mode: SslMode) -> Vec<Option<SslOpts>> {
    let encrypt_only = || {
        SslOpts::default()
            .with_danger_accept_invalid_certs(true)
            .with_danger_skip_domain_validation(true)
    };
    match mode {
        SslMode::Disable => vec![None],
        SslMode::Prefer => vec![Some(encrypt_only()), None],
        SslMode::Require => vec![Some(encrypt_only())],
        SslMode::VerifyFull => vec![Some(SslOpts::default())],
    }
}

/// 按 SSL 模式建立连接池并试连一次,返回第一个能连上的。
async fn open_pool(
    target: &Target,
    user: &str,
    pass: &str,
    db: &str,
    mode: SslMode,
) -> AppResult<Pool> {
    crate::tls::install_default_provider();
    let mut last_err = None;
    // Unix 套接字上驱动不做 TLS:只有「不加密」模式才会走套接字(见 ssh::listen_for)
    let attempts = match target.via {
        Some(crate::ssh::Via::Socket(_)) => vec![None],
        _ => ssl_attempts(mode),
    };
    for ssl in attempts {
        let pool = Pool::new(opts(target, user, pass, db, ssl));
        // 主机不可达时不等系统的 TCP 超时(约 75 秒)
        // 超时说明主机不可达,不再换明文重试
        let Ok(conn) = tokio::time::timeout(CONNECT_TIMEOUT, pool.get_conn()).await else {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), pool.disconnect()).await;
            return Err(connerr(t!("core.err.connect_timeout")));
        };
        match conn {
            Ok(conn) => {
                drop(conn);
                return Ok(pool);
            }
            Err(e) => {
                pool.disconnect().await.ok();
                last_err = Some(e);
            }
        }
    }
    Err(connerr(last_err.expect("至少尝试一次")))
}

pub async fn test_connect(
    target: &Target,
    user: &str,
    pass: &str,
    db: &str,
    ssl: SslMode,
) -> AppResult<()> {
    let pool = open_pool(target, user, pass, db, ssl).await?;
    pool.disconnect().await.ok();
    Ok(())
}

/// 用给定参数临时连接实例(不指定库),列出库名(新建连接表单里选库用,不缓存)。
pub async fn probe_databases(
    target: &Target,
    user: &str,
    pass: &str,
    ssl: SslMode,
) -> AppResult<Vec<String>> {
    let pool = open_pool(target, user, pass, "", ssl).await?;
    let result = async {
        let mut conn = pool.get_conn().await.map_err(connerr)?;
        let names: Vec<String> = conn
            .query("SHOW DATABASES")
            .await
            .map_err(|e| AppError::new(ErrorKind::Query, e.to_string()))?;
        Ok(names)
    }
    .await;
    pool.disconnect().await.ok();
    result
}

impl MyPool {
    pub async fn connect(
        &self,
        id: &str,
        target: &Target,
        user: &str,
        pass: &str,
        db: &str,
        ssl: SslMode,
    ) -> AppResult<()> {
        let pool = open_pool(target, user, pass, db, ssl).await?;
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

    /// 从连接池取一条独立连接(长时间的批量写入用,如造测试数据):不占编辑器会话,用完丢弃即归还。
    pub async fn dedicated_conn(&self, id: &str) -> AppResult<mysql_async::Conn> {
        self.pool(id).await?.get_conn().await.map_err(qerr)
    }

    /// 断开并移除连接池(不存在时静默):先移出映射再优雅关闭,
    /// 服务器侧会话随之结束 —— 删库前强制断开残留连接用。
    pub async fn disconnect(&self, id: &str) {
        let session = self.sessions.lock().unwrap().remove(id);
        if let Some(session) = session {
            // 等手上的语句结束;会话连接随之关闭(不归还连接池,池马上也要断开)
            if let Some(conn) = session.conn.lock().await.take() {
                let _ = conn.disconnect().await;
            }
        }
        let pool = self.pools.lock().await.remove(id);
        if let Some(p) = pool {
            let _ = p.disconnect().await;
        }
    }

    /// 取出该连接的会话(没有或已断开就从连接池取一条),返回时持有会话锁。
    async fn session(&self, id: &str) -> AppResult<tokio::sync::OwnedMutexGuard<Option<mysql_async::Conn>>> {
        let pool = self.pool(id).await?;
        let session = self.sessions.lock().unwrap().entry(id.to_string()).or_default().clone();
        let mut guard = session.conn.clone().lock_owned().await;
        if guard.is_none() {
            let conn = pool.get_conn().await.map_err(qerr)?;
            session.thread.store(conn.id(), std::sync::atomic::Ordering::Relaxed);
            *guard = Some(conn);
        }
        Ok(guard)
    }

    /// 会话上的语句出错后:连接已断(服务器重启、网络中断、被 KILL)就丢掉,下次重新取。
    async fn drop_if_dead(guard: &mut Option<mysql_async::Conn>) {
        if let Some(conn) = guard.as_mut() {
            if conn.ping().await.is_err() {
                *guard = None;
            }
        }
    }

    /// 只读执行(会话上):免确认执行、影响预估、撤销快照、AI 查询。
    pub async fn query_read_only(
        &self,
        id: &str,
        sql: &str,
        max_rows: Option<u32>,
    ) -> AppResult<QueryResult> {
        self.query_read_only_bounded(id, sql, max_rows, None).await
    }

    /// 同 [`Self::query_read_only`],`timeout_ms` 给定时由服务端限制 SELECT 的执行时长
    /// (MySQL 的 max_execution_time;MariaDB 没有这个变量,设置失败就不限时)。
    ///
    /// 会话不在事务里时,先切成只读会话再执行,由服务器拒绝写入。
    /// 用户在编辑器里开着事务时,MySQL 不允许改当前事务的只读属性:在保存点里执行
    /// (看得到事务里未提交的改动),结束后回到保存点,语句造成的写入一并撤销。
    pub async fn query_read_only_bounded(
        &self,
        id: &str,
        sql: &str,
        max_rows: Option<u32>,
        timeout_ms: Option<u32>,
    ) -> AppResult<QueryResult> {
        let mut guard = self.session(id).await?;
        let conn = guard.as_mut().expect("会话已建立");
        // 事务进行中时 SET TRANSACTION 报 1568;不在事务里则只作用于下一个事务(之后会恢复)
        let in_txn = match conn.query_drop("SET TRANSACTION READ ONLY").await {
            Ok(()) => false,
            Err(mysql_async::Error::Server(e)) if e.code == 1568 => true,
            Err(e) => {
                let err = qerr(e);
                Self::drop_if_dead(&mut guard).await;
                return Err(err);
            }
        };
        let result = if in_txn {
            // 事务里改不了只读属性:放进保存点执行,结束后回到保存点 ——
            // 语句看得到事务里未提交的改动,而它自己造成的写入(如视图里调用的写函数)被撤销
            conn.query_drop("SAVEPOINT datum_ro").await.map_err(qerr)?;
            let r = read_only_on(conn, sql, max_rows, timeout_ms, false).await;
            let _ = conn.query_drop("ROLLBACK TO SAVEPOINT datum_ro").await;
            let _ = conn.query_drop("RELEASE SAVEPOINT datum_ro").await;
            r
        } else {
            read_only_on(conn, sql, max_rows, timeout_ms, true).await
        };
        if result.is_err() {
            Self::drop_if_dead(&mut guard).await;
        }
        result
    }

    /// 元数据 / 诊断的只读查询(连接池):不排在会话上的长查询后面,也不受会话里事务的影响。
    pub async fn query_meta_read_only_bounded(
        &self,
        id: &str,
        sql: &str,
        max_rows: Option<u32>,
        timeout_ms: Option<u32>,
    ) -> AppResult<QueryResult> {
        let pool = self.pool(id).await?;
        let mut conn = pool.get_conn().await.map_err(qerr)?;
        read_only_on(&mut conn, sql, max_rows, timeout_ms, true).await
    }

    /// 元数据查询(连接池):表清单、表结构等。
    pub async fn query(&self, id: &str, sql: &str) -> AppResult<QueryResult> {
        self.query_pool(id, sql, None).await
    }

    /// 同 [`Self::query`],结果行数不超过 max_rows。
    pub async fn query_pool(&self, id: &str, sql: &str, max_rows: Option<u32>) -> AppResult<QueryResult> {
        let pool = self.pool(id).await?;
        let mut conn = pool.get_conn().await.map_err(qerr)?;
        Self::run_capped(&mut conn, sql, max_rows).await
    }

    /// 逐行把结果交给 `sink`(整表 / 整库导出,连接池):不收集全部行,不挡编辑器会话。
    pub async fn stream_pool(&self, id: &str, sql: &str, sink: &mut dyn crate::export::RowSink) -> AppResult<()> {
        let pool = self.pool(id).await?;
        let mut conn = pool.get_conn().await.map_err(qerr)?;
        let mut result = conn.query_iter(sql).await.map_err(qerr)?;
        let columns: Vec<String> = result
            .columns()
            .map(|cs| cs.iter().map(|c| c.name_str().to_string()).collect())
            .unwrap_or_default();
        sink.columns(&columns)?;
        while let Some(row) = result.next().await.map_err(qerr)? {
            sink.row(row_to_strings(&row))?;
        }
        Ok(())
    }

    /// 在会话上执行(编辑器):结果行数不超过 max_rows(None = 不限)。
    /// 逐行流式读取:上限之内才转换存储,之后的行读完即丢(排空结果集保持协议干净、
    /// affected_rows 可靠),峰值内存 O(max_rows)。
    pub async fn query_capped(
        &self,
        id: &str,
        sql: &str,
        max_rows: Option<u32>,
    ) -> AppResult<QueryResult> {
        let mut guard = self.session(id).await?;
        let result = Self::run_capped(guard.as_mut().expect("会话已建立"), sql, max_rows).await;
        if result.is_err() {
            Self::drop_if_dead(&mut guard).await;
        }
        result
    }

    /// 停止会话上正在执行的语句:另取一条连接 `KILL QUERY` 会话线程(只中止语句,不断开连接)。
    /// 会话空闲时什么都不做,返回 false。
    pub async fn cancel(&self, id: &str) -> AppResult<bool> {
        let Some(session) = self.sessions.lock().unwrap().get(id).cloned() else {
            return Ok(false);
        };
        // 拿得到会话锁说明没有语句在执行
        if session.conn.try_lock().is_ok() {
            return Ok(false);
        }
        let thread = session.thread.load(std::sync::atomic::Ordering::Relaxed);
        let pool = self.pool(id).await?;
        let mut conn = pool.get_conn().await.map_err(qerr)?;
        // 1094 Unknown thread id:语句刚好结束、连接已关闭
        Ok(conn.query_drop(format!("KILL QUERY {thread}")).await.is_ok())
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

    #[test]
    fn tunneled_opts_dial_local_port_but_verify_db_host() {
        let target = Target::forwarded(50124, "db.internal");
        for ssl in ssl_attempts(SslMode::VerifyFull).into_iter().chain(ssl_attempts(SslMode::Require)) {
            let o = opts(&target, "u", "p", "d", ssl);
            assert_eq!(o.ip_or_hostname(), "127.0.0.1");
            assert_eq!(o.tcp_port(), 50124);
            assert_eq!(o.ssl_opts().unwrap().tls_hostname_override(), Some("db.internal"));
        }
        let direct = opts(&Target::direct("db.internal", 3306), "u", "p", "d", ssl_attempts(SslMode::VerifyFull).remove(0));
        assert_eq!(direct.ssl_opts().unwrap().tls_hostname_override(), None);
    }

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
        pool.connect("t", &Target::direct(&h, port), &u, &pw, &db, SslMode::Prefer)
            .await
            .unwrap();
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
        pool.connect("t", &Target::direct(&h, port), &u, &pw, &db, SslMode::Prefer)
            .await
            .unwrap();
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
