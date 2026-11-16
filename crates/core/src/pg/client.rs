use crate::core::connection::SslMode;
use crate::error::{AppError, AppResult, ErrorKind};
use rust_i18n::t;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::config::SslMode as PgSslMode;
use tokio_postgres::{Client, Config, SimpleQueryMessage};
use crate::ssh::Target;
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

/// 已建立的连接。每个连接各有两条会话,各自一把锁:
/// - `session`:用户会话。编辑器执行、改数据、导入,以及要看到会话状态(事务里未提交的改动、
///   临时表、SET 过的 search_path)的只读查询(影响预估、撤销快照、免确认执行、AI)都在这里。
/// - `aux`:辅助会话,首次用到时才建立。表清单、列与外键、诊断、补全取样这类元数据查询走这里,
///   用户会话上跑着慢查询、或停在一个事务里时都不受影响;它从不处于事务中。
///
/// 连接表只在查找时短暂加锁(同步锁、不跨 await),一条连接上的慢查询不挡别的连接。
pub struct PgConn {
    session: Mutex<Client>,
    /// 用户会话的服务器进程号:取消查询时在辅助会话上 `pg_cancel_backend` 它
    /// (走已有的辅助会话,SSH 隧道与 TLS 下都一样可用)
    session_pid: i32,
    aux: Mutex<Option<Client>>,
    config: Config,
    ssl: SslMode,
    target: Target,
}

#[derive(Default)]
pub struct PgPool {
    conns: std::sync::Mutex<HashMap<String, Arc<PgConn>>>,
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

/// 连接地址:走 SSH 隧道时实际连本地转发端口(hostaddr),
/// 主机名仍填数据库的原主机名,证书与 SNI 按它校验。
pub fn apply_target(cfg: &mut Config, target: &Target) {
    cfg.host(&target.tls_host).port(target.port);
    if target.tunneled() {
        if let Ok(ip) = target.host.parse::<std::net::IpAddr>() {
            cfg.hostaddr(ip);
        }
    }
}

/// 按目标建一条连接:直连 / 本地端口走 `Config::connect`;经 SSH 通道时在通道上 `connect_raw`
/// (TLS 照常按数据库原主机名校验)。连接的驱动任务在后台跑,Client 丢弃即断开。
async fn dial(cfg: &Config, target: &Target, ssl: SslMode) -> Result<Client, String> {
    // 主机不可达时不等系统的 TCP 超时(约 75 秒);经 SSH 通道的握手同样受限
    match tokio::time::timeout(CONNECT_TIMEOUT, dial_inner(cfg, target, ssl)).await {
        Ok(r) => r,
        Err(_) => Err(t!("core.err.connect_timeout").to_string()),
    }
}

/// 建连(含 TLS 握手与认证)最多等多久。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

async fn dial_inner(cfg: &Config, target: &Target, ssl: SslMode) -> Result<Client, String> {
    use tokio_postgres::tls::MakeTlsConnect;
    // 空闲连接每分钟探测一次:NAT / 防火墙悄悄断开的连接能尽早发现
    let mut cfg = cfg.clone();
    cfg.keepalives(true).keepalives_idle(std::time::Duration::from_secs(60));
    let cfg = &cfg;
    if let Some(stream) = target.open_stream().await {
        let stream = stream.map_err(|e| e.display())?;
        let mut make = tls_connector(ssl);
        let tls = <MakeRustlsConnect as MakeTlsConnect<crate::ssh::SshStream>>::make_tls_connect(&mut make, &target.tls_host)
            .map_err(|e| e.to_string())?;
        let (client, connection) = cfg.connect_raw(stream, tls).await.map_err(|e| describe(&e))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        return Ok(client);
    }
    let (client, connection) = cfg.connect(tls_connector(ssl)).await.map_err(|e| describe(&e))?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

/// 用给定参数尝试建立一次连接以验证可达性与认证(不缓存)。
pub async fn test_connect(
    target: &Target,
    user: &str,
    pass: &str,
    db: &str,
    ssl: SslMode,
) -> AppResult<()> {
    let mut cfg = Config::new();
    apply_target(&mut cfg, target);
    cfg.user(user).password(pass).dbname(db);
    apply_ssl_mode(&mut cfg, ssl);
    // 建连成功(完成握手与认证)即视为连通;立即丢弃。
    dial(&cfg, target, ssl).await.map_err(|e| {
        AppError::new(ErrorKind::Connection, t!("core.err.connect_failed", db = "PostgreSQL")).with_detail(e)
    })?;
    Ok(())
}

/// 用给定参数临时连接实例,列出可连接的非模板库(新建连接表单里选库用,不缓存)。
/// 先连 `db`(表单里填的库);它为空或连不上时依次退回 `postgres`、`template1`。
pub async fn probe_databases(
    target: &Target,
    user: &str,
    pass: &str,
    db: &str,
    ssl: SslMode,
) -> AppResult<Vec<String>> {
    let mut candidates: Vec<&str> = Vec::new();
    if !db.trim().is_empty() {
        candidates.push(db.trim());
    }
    for fallback in ["postgres", "template1"] {
        if !candidates.contains(&fallback) {
            candidates.push(fallback);
        }
    }
    let mut last_err = None;
    for name in candidates {
        let mut cfg = Config::new();
        apply_target(&mut cfg, target);
        cfg.user(user).password(pass).dbname(name);
        apply_ssl_mode(&mut cfg, ssl);
        match dial(&cfg, target, ssl).await {
            Ok(client) => {
                let rows = client
                    .simple_query(
                        "SELECT datname FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY datname",
                    )
                    .await
                    .map_err(|e| {
                        AppError::new(ErrorKind::Query, t!("core.err.connect_failed", db = "PostgreSQL"))
                            .with_detail(describe(&e))
                    });
                drop(client);
                return Ok(rows?
                    .into_iter()
                    .filter_map(|m| match m {
                        SimpleQueryMessage::Row(r) => r.get(0).map(str::to_string),
                        _ => None,
                    })
                    .collect());
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(AppError::new(
        ErrorKind::Connection,
        t!("core.err.connect_failed", db = "PostgreSQL"),
    )
    .with_detail(last_err.unwrap_or_default()))
}

impl PgPool {
    /// 用 Config 建立连接并缓存到 id。
    /// 用 Config(显式 setter)而非手拼连接串:空密码或含特殊字符的库名都能正确处理,
    /// 避免 `password= dbname=...` 这类空格分隔串在空值时解析丢失 dbname 的问题。
    /// `config` 需已按 `ssl` 调用过 [`apply_ssl_mode`]。
    pub async fn connect(&self, id: &str, config: &Config, ssl: SslMode) -> AppResult<()> {
        self.connect_target(id, config, ssl, &Target::direct("", 0)).await
    }

    /// 同 [`connect`](Self::connect),经 SSH 通道时在通道上建连(见 `target`)。
    /// 只建用户会话;辅助会话在第一次用到时再建。
    pub async fn connect_target(&self, id: &str, config: &Config, ssl: SslMode, target: &Target) -> AppResult<()> {
        let client = dial(config, target, ssl).await.map_err(|e| {
            AppError::new(ErrorKind::Connection, t!("core.err.connect_failed", db = "PostgreSQL")).with_detail(e)
        })?;
        let session_pid = backend_pid(&client).await?;
        let conn = PgConn {
            session: Mutex::new(client),
            session_pid,
            aux: Mutex::new(None),
            config: config.clone(),
            ssl,
            target: target.clone(),
        };
        self.conns.lock().unwrap().insert(id.to_string(), Arc::new(conn));
        Ok(())
    }

    fn get(&self, id: &str) -> AppResult<Arc<PgConn>> {
        self.conns
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.not_connected")))
    }

    /// 按该连接的配置另开一条独立会话(长时间的批量写入用,如造测试数据):
    /// 不占用户会话与辅助会话,调用方用完丢弃即断开。
    pub async fn dial_dedicated(&self, id: &str) -> AppResult<Client> {
        let conn = self.get(id)?;
        dial(&conn.config, &conn.target, conn.ssl).await.map_err(|e| {
            AppError::new(ErrorKind::Connection, t!("core.err.connect_failed", db = "PostgreSQL")).with_detail(e)
        })
    }

    #[allow(dead_code)] // 目前仅测试使用,保留备将来重连/状态显示
    pub async fn is_connected(&self, id: &str) -> bool {
        self.conns.lock().unwrap().contains_key(id)
    }

    /// 断开并移除缓存的连接(不存在时静默)。两条会话的 Client 随最后一个引用释放,
    /// 服务器侧会话随之终止(正在执行的语句先跑完)—— 删库前强制断开残留连接用。
    pub async fn disconnect(&self, id: &str) {
        let conn = self.conns.lock().unwrap().remove(id);
        if let Some(conn) = conn {
            // 等手上的语句跑完再返回,调用方(删库)依赖返回时会话已经结束
            let _session = conn.session.lock().await;
            let _aux = conn.aux.lock().await;
        }
    }

    /// 在一个事务里批量执行编辑与删除(全成功或全回滚)。返回受影响行数。先 UPDATE 后 DELETE。
    pub async fn with_txn(
        &self,
        id: &str,
        edits: &[crate::pg::edit::CellEdit],
        deletes: &[crate::pg::edit::RowDelete],
    ) -> AppResult<u64> {
        let conn = self.get(id)?;
        let mut client = conn.session.lock().await;
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
        let conn = self.get(id)?;
        let mut client = conn.session.lock().await;
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
        let conn = self.get(id)?;
        let client = conn.session.lock().await;
        run_capped(&client, sql, max_rows).await
    }

    /// 只读执行(用户会话上):切只读、执行、恢复在同一把锁里完成,
    /// 期间同一会话上的其他请求(包括另一次只读执行的「恢复」)插不进来。
    pub async fn query_read_only(
        &self,
        id: &str,
        sql: &str,
        max_rows: Option<u32>,
    ) -> AppResult<QueryResult> {
        self.query_read_only_bounded(id, sql, max_rows, None).await
    }

    /// 同 [`Self::query_read_only`],`timeout_ms` 给定时由服务端限制执行时长(statement_timeout),
    /// 超时即中止,不会占着连接拖住后面的语句。
    ///
    /// 用户可能在编辑器里开着事务(`BEGIN` 之后还没提交):这时在保存点里执行,
    /// 看得到事务里未提交的改动,结束后回到保存点,用户的事务原样保留;
    /// 事务已出错(要先 ROLLBACK)时直接报错,不替用户结束事务。
    pub async fn query_read_only_bounded(
        &self,
        id: &str,
        sql: &str,
        max_rows: Option<u32>,
        timeout_ms: Option<u32>,
    ) -> AppResult<QueryResult> {
        let conn = self.get(id)?;
        let client = conn.session.lock().await;
        run_read_only(&client, sql, max_rows, timeout_ms).await
    }

    /// 取消用户会话上正在执行的语句(编辑器的「停止」)。返回服务器是否接受了取消请求;
    /// 会话空闲时请求被忽略,不影响之后的语句。语句在用户开的事务里时,事务随之进入出错状态,
    /// 与 psql 里按 Ctrl+C 一样需要用户 ROLLBACK。
    pub async fn cancel(&self, id: &str) -> AppResult<bool> {
        let conn = self.get(id)?;
        let sql = format!("SELECT pg_cancel_backend({})", conn.session_pid);
        let mut aux = conn.aux().await?;
        let client = aux.as_mut().expect("aux 已建立");
        let r = run_capped(client, &sql, Some(1)).await?;
        Ok(r.rows.first().and_then(|r| r.first()).and_then(|v| v.as_deref()) == Some("t"))
    }

    /// 逐行把结果交给 `sink`(整表 / 整库导出,辅助会话):不收集全部行,不挡用户会话。
    pub async fn stream_meta(&self, id: &str, sql: &str, sink: &mut dyn crate::export::RowSink) -> AppResult<()> {
        use futures_util::StreamExt;
        let conn = self.get(id)?;
        let mut aux = conn.aux().await?;
        let client = aux.as_mut().expect("aux 已建立");
        let stream = client.simple_query_raw(sql).await.map_err(|e| {
            AppError::new(ErrorKind::Query, t!("core.err.query_failed")).with_detail(describe(&e))
        })?;
        let mut stream = std::pin::pin!(stream);
        while let Some(m) = stream.next().await {
            let m = m.map_err(|e| AppError::new(ErrorKind::Query, t!("core.err.query_failed")).with_detail(describe(&e)))?;
            match m {
                SimpleQueryMessage::RowDescription(cols) => {
                    let names: Vec<String> = cols.iter().map(|c| c.name().to_string()).collect();
                    sink.columns(&names)?;
                }
                SimpleQueryMessage::Row(r) => sink.row((0..r.len()).map(|i| r.get(i).map(str::to_string)).collect())?,
                _ => {}
            }
        }
        Ok(())
    }

    /// 元数据查询(辅助会话):表清单、列与外键、诊断等。不受用户会话上的慢查询与事务影响。
    pub async fn query_meta(&self, id: &str, sql: &str, max_rows: Option<u32>) -> AppResult<QueryResult> {
        let conn = self.get(id)?;
        let mut aux = conn.aux().await?;
        let client = aux.as_mut().expect("aux 已建立");
        run_capped(client, sql, max_rows).await
    }

    /// 同 [`Self::query_meta`],只读 + 服务端限时(诊断、补全取样)。
    pub async fn query_meta_read_only(
        &self,
        id: &str,
        sql: &str,
        max_rows: Option<u32>,
        timeout_ms: Option<u32>,
    ) -> AppResult<QueryResult> {
        let conn = self.get(id)?;
        let mut aux = conn.aux().await?;
        let client = aux.as_mut().expect("aux 已建立");
        run_read_only(client, sql, max_rows, timeout_ms).await
    }
}

impl PgConn {
    /// 拿到辅助会话(没建或已断开时先建),返回时持有它的锁,保证 Some。
    async fn aux(&self) -> AppResult<tokio::sync::MutexGuard<'_, Option<Client>>> {
        let mut aux = self.aux.lock().await;
        if aux.as_ref().is_none_or(|c| c.is_closed()) {
            let client = dial(&self.config, &self.target, self.ssl).await.map_err(|e| {
                AppError::new(ErrorKind::Connection, t!("core.err.connect_failed", db = "PostgreSQL")).with_detail(e)
            })?;
            *aux = Some(client);
        }
        Ok(aux)
    }
}

async fn backend_pid(client: &Client) -> AppResult<i32> {
    let r = run_capped(client, "SELECT pg_backend_pid()", Some(1)).await?;
    r.rows
        .first()
        .and_then(|r| r.first())
        .and_then(|v| v.as_deref())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.connect_failed", db = "PostgreSQL")))
}

/// 会话当前是否处在事务块里(`BEGIN` 之后、提交 / 回滚之前)。
/// 事务块里的语句,其开始时间晚于事务开始时间;不在事务块时每条语句自成事务,两者相等。
/// 事务已出错时这条查询本身会失败,报「先 ROLLBACK」。
async fn in_transaction(client: &Client) -> AppResult<bool> {
    let msgs = client
        .simple_query("SELECT statement_timestamp() <> transaction_timestamp()")
        .await
        .map_err(|e| {
            // 按 SQLSTATE 判断(服务器报错文本可能是本地化的)
            if e.code() == Some(&tokio_postgres::error::SqlState::IN_FAILED_SQL_TRANSACTION) {
                AppError::new(ErrorKind::Query, t!("core.err.pg_txn_aborted"))
            } else {
                AppError::new(ErrorKind::Query, t!("core.err.query_failed")).with_detail(describe(&e))
            }
        })?;
    Ok(msgs.iter().any(|m| matches!(m, SimpleQueryMessage::Row(r) if r.get(0) == Some("t"))))
}

/// 在已拿到的会话上只读执行(调用方持有它的锁)。
///
/// 会话级的 `default_transaction_read_only` 让语句里即使先 COMMIT、后面的语句开的新事务仍是只读。
/// - 不在事务块:切只读 → 执行 → 回滚语句可能留下的事务 → 恢复。
/// - 在事务块:建保存点,`SET LOCAL transaction_read_only` 让当前事务只读;
///   执行后回到保存点,这期间的设置随之撤销,用户事务里此前的改动保留。
async fn run_read_only(
    client: &Client,
    sql: &str,
    max_rows: Option<u32>,
    timeout_ms: Option<u32>,
) -> AppResult<QueryResult> {
    let timeout = timeout_ms.map(|ms| format!("; SET statement_timeout = {ms}")).unwrap_or_default();
    if in_transaction(client).await? {
        run_capped(
            client,
            &format!(
                "SAVEPOINT datum_ro; SET LOCAL transaction_read_only = on; \
                 SET default_transaction_read_only = on{timeout}"
            ),
            None,
        )
        .await?;
        let result = run_capped(client, sql, max_rows).await;
        // 语句出错时保存点之后的部分已中止,回到保存点即恢复;语句里若结束了事务,保存点已不在,
        // 这一步会失败,再单独恢复会话设置
        let restored = client
            .batch_execute("ROLLBACK TO SAVEPOINT datum_ro; RELEASE SAVEPOINT datum_ro")
            .await;
        if restored.is_err() {
            reset_session(client, timeout_ms.is_some()).await?;
        }
        return result;
    }
    run_capped(client, &format!("SET default_transaction_read_only = on{timeout}"), None).await?;
    let result = run_capped(client, sql, max_rows).await;
    // 语句可能留下中止或未结束的事务(如以 BEGIN 开头),先回滚再恢复,否则 RESET 会因事务已中止而失败
    let _ = client.batch_execute("ROLLBACK").await;
    reset_session(client, timeout_ms.is_some()).await?;
    result
}

async fn reset_session(client: &Client, timeout: bool) -> AppResult<()> {
    if timeout {
        run_capped(client, "RESET statement_timeout", None).await?;
    }
    run_capped(client, "RESET default_transaction_read_only", None).await?;
    Ok(())
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
