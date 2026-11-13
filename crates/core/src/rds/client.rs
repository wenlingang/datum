use crate::core::connection::SslMode;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::ssh::Target;
use redis::aio::MultiplexedConnection;
use redis::FromRedisValue;
use rust_i18n::t;
use serde::Serialize;
use std::collections::HashMap;
use tokio::sync::Mutex;

/// key 的值,统一成 列 + 行 的表格形态,前端按类型渲染。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyValue {
    pub key: String,
    pub key_type: String, // string/hash/list/set/zset/none
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// 元素太多只取了前 [`MAX_ELEMENTS`] 个
    #[serde(default)]
    pub truncated: bool,
    /// 有元素不是合法 UTF-8,显示的是替换过的文本:按显示的文本去改 / 删会对不上原值,界面据此只读
    #[serde(default)]
    pub binary: bool,
    /// 不是合法 UTF-8 的单元格的原始字节:`rows` 里是替换过的文本,查看时识别格式(MessagePack、gzip…)要用原始字节。
    /// 合法 UTF-8 的单元格不重复带,文本本身就能还原出原始字节
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw: Vec<RawCell>,
}

/// 一个单元格的原始字节(base64)。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RawCell {
    pub row: usize,
    pub col: usize,
    pub b64: String,
}

/// 字节转文本;不是合法 UTF-8 时用替换符,并把原始字节记进 `raw`。
pub(super) fn cell_text(b: Vec<u8>, row: usize, col: usize, raw: &mut Vec<RawCell>) -> String {
    use base64::Engine;
    match String::from_utf8(b) {
        Ok(s) => s,
        Err(e) => {
            let b = e.into_bytes();
            raw.push(RawCell { row, col, b64: base64::engine::general_purpose::STANDARD.encode(&b) });
            String::from_utf8_lossy(&b).into_owned()
        }
    }
}

/// 查看集合类 key 时最多取的元素数:大 key 一次全读会拖住实例,也撑爆界面。
pub const MAX_ELEMENTS: usize = 1000;
/// 字符串值最多读取的字节数。
const MAX_STRING_BYTES: usize = 1 << 20;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyDetail {
    pub key: String,
    pub key_type: String,
    pub ttl: i64,  // -1 永不过期,-2 不存在
    pub size: i64, // 字符串长度 / 元素个数
}

#[derive(Default)]
pub struct RedisPool {
    conns: Mutex<HashMap<String, MultiplexedConnection>>,
    /// 建连参数(集群分析时照着它连其余节点);经 SSH 隧道的连接没有(隧道只通当前节点)
    infos: Mutex<HashMap<String, redis::ConnectionInfo>>,
    /// 各连接最近一次的分析(进行中或已结束)
    analyses: Mutex<HashMap<String, std::sync::Arc<super::analysis::Analysis>>>,
}

pub(super) fn qerr(e: redis::RedisError) -> AppError {
    AppError::new(ErrorKind::Query, t!("core.err.redis_command_failed")).with_detail(e.to_string())
}

/// 用给定参数尝试建立一次连接以验证可达性与认证(不缓存)。
/// 构造连接串:redis://[user][:password]@host:port/db;加密用 rediss://。
/// Redis 无法在同一端口协商加密,「优先」等同不加密;「必须」用 rediss 且不校验证书(#insecure)。
pub fn build_url(host: &str, port: u16, user: &str, pass: &str, db: &str, ssl: SslMode) -> String {
    let dbn = db.trim().parse::<u8>().unwrap_or(0);
    let auth = if !user.is_empty() {
        format!("{user}:{pass}@")
    } else if !pass.is_empty() {
        format!(":{pass}@")
    } else {
        String::new()
    };
    match ssl {
        SslMode::Disable | SslMode::Prefer => format!("redis://{auth}{host}:{port}/{dbn}"),
        SslMode::Require => format!("rediss://{auth}{host}:{port}/{dbn}#insecure"),
        SslMode::VerifyFull => format!("rediss://{auth}{host}:{port}/{dbn}"),
    }
}

pub async fn test_connect(
    target: &Target,
    user: &str,
    pass: &str,
    db: &str,
    ssl: SslMode,
) -> AppResult<()> {
    let url = build_url(&target.host, target.port, user, pass, db, ssl);
    open_target(target, &url, ssl).await?;
    Ok(())
}

/// 按目标建连。经 SSH 通道时(不开本地端口)在通道上建多路复用连接,需要加密时自己在通道上做 TLS
/// (「校验证书」按数据库原主机名校验,「必须」只加密不校验)。走本地转发端口且要校验证书时同理:
/// redis 自己建的 TLS 只能按连接地址(127.0.0.1)校验主机名。
pub async fn open_target(target: &Target, url: &str, ssl: SslMode) -> AppResult<MultiplexedConnection> {
    let via_stream = matches!(target.via, Some(crate::ssh::Via::Stream(_)));
    if !via_stream && !(target.tunneled() && ssl == SslMode::VerifyFull) {
        return open(url).await;
    }
    crate::tls::install_default_provider();
    let info = redis::Client::open(url)
        .map_err(|e| {
            AppError::new(ErrorKind::Connection, t!("core.err.invalid_redis_url")).with_detail(e.to_string())
        })?
        .get_connection_info()
        .redis
        .clone();
    let failed = |e: String| {
        AppError::new(ErrorKind::Connection, t!("core.err.connect_failed", db = "Redis")).with_detail(e)
    };
    async fn over<S>(info: &redis::RedisConnectionInfo, stream: S, tls: Option<(&str, bool)>) -> Result<MultiplexedConnection, String>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        match tls {
            None => {
                let (con, driver) = MultiplexedConnection::new(info, stream).await.map_err(|e| e.to_string())?;
                tokio::spawn(driver);
                Ok(con)
            }
            Some((host, verify)) => {
                let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|e| e.to_string())?;
                let tls = tokio_rustls::TlsConnector::from(std::sync::Arc::new(crate::tls::client_config(verify)))
                    .connect(name, stream)
                    .await
                    .map_err(|e| e.to_string())?;
                let (con, driver) = MultiplexedConnection::new(info, tls).await.map_err(|e| e.to_string())?;
                tokio::spawn(driver);
                Ok(con)
            }
        }
    }
    let tls = match ssl {
        SslMode::Disable | SslMode::Prefer => None,
        SslMode::Require => Some((target.tls_host.as_str(), false)),
        SslMode::VerifyFull => Some((target.tls_host.as_str(), true)),
    };
    let connect = async {
        match target.open_stream().await {
            Some(stream) => over(&info, stream.map_err(|e| e.display())?, tls).await,
            None => {
                let tcp = tokio::net::TcpStream::connect((target.host.as_str(), target.port)).await.map_err(|e| e.to_string())?;
                over(&info, tcp, tls).await
            }
        }
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(r) => r.map_err(failed),
        Err(_) => Err(failed(t!("core.err.connect_timeout").to_string())),
    }
}

/// 握手 + 认证的总时限。rediss 连到明文端口时服务端不会回应 TLS 握手,不设时限会一直挂住。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

async fn open(url: &str) -> AppResult<MultiplexedConnection> {
    crate::tls::install_default_provider();
    let client = redis::Client::open(url).map_err(|e| {
        AppError::new(ErrorKind::Connection, t!("core.err.invalid_redis_url"))
            .with_detail(e.to_string())
    })?;
    let failed = || {
        AppError::new(
            ErrorKind::Connection,
            t!("core.err.connect_failed", db = "Redis"),
        )
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, client.get_multiplexed_async_connection()).await {
        Ok(Ok(con)) => Ok(con),
        Ok(Err(e)) => Err(failed().with_detail(e.to_string())),
        Err(_) => Err(failed().with_detail(t!("core.err.connect_timeout").to_string())),
    }
}

/// 把任意 Redis 回复转成可读字符串(版本无关,基于 FromRedisValue)。
fn format_reply(v: &redis::Value) -> String {
    if let Ok(s) = String::from_redis_value(v) {
        return s;
    }
    if let Ok(bytes) = Vec::<u8>::from_redis_value(v) {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    if let Ok(list) = Vec::<Vec<u8>>::from_redis_value(v) {
        return list
            .iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect::<Vec<_>>()
            .join("\n");
    }
    format!("{:?}", v)
}

/// 读一个 key 的值,统一成表格。集合类最多取 [`MAX_ELEMENTS`] 个元素(hash / set 用 SCAN 分批取,
/// 不用 HGETALL / SMEMBERS 一次读完),字符串最多 1 MB;超出时 `truncated`。
async fn read_value(con: &mut MultiplexedConnection, key: &str) -> AppResult<KeyValue> {
    let t: String = redis::cmd("TYPE").arg(key).query_async(con).await.map_err(qerr)?;
    let max = MAX_ELEMENTS as i64;
    let mut truncated = false;
    // HSCAN / SSCAN 分批取到上限;返回的是扁平列表
    async fn scan_all(con: &mut MultiplexedConnection, cmd: &str, key: &str, cap: usize) -> AppResult<(Vec<Vec<u8>>, bool)> {
        let mut out = vec![];
        let mut cursor: u64 = 0;
        loop {
            let (next, batch): (u64, Vec<Vec<u8>>) = redis::cmd(cmd)
                .arg(key)
                .arg(cursor)
                .arg("COUNT")
                .arg(200)
                .query_async(con)
                .await
                .map_err(qerr)?;
            out.extend(batch);
            cursor = next;
            if out.len() >= cap {
                let more = cursor != 0 || out.len() > cap;
                out.truncate(cap);
                return Ok((out, more));
            }
            if cursor == 0 {
                return Ok((out, false));
            }
        }
    }
    // 各类型先取原始字节,最后统一转文本(顺带记下非 UTF-8 单元格的原始字节)
    let (columns, cells): (Vec<String>, Vec<Vec<Vec<u8>>>) = match t.as_str() {
        "string" => {
            let len: i64 = redis::cmd("STRLEN").arg(key).query_async(con).await.unwrap_or(0);
            let v: Option<Vec<u8>> = if len as usize > MAX_STRING_BYTES {
                truncated = true;
                redis::cmd("GETRANGE").arg(key).arg(0).arg(MAX_STRING_BYTES as i64 - 1).query_async(con).await.map_err(qerr)?
            } else {
                redis::cmd("GET").arg(key).query_async(con).await.map_err(qerr)?
            };
            (vec!["value".into()], vec![vec![v.unwrap_or_default()]])
        }
        "hash" => {
            let (flat, more) = scan_all(con, "HSCAN", key, MAX_ELEMENTS * 2).await?;
            truncated = more;
            (
                vec!["field".into(), "value".into()],
                flat.chunks(2).map(|p| vec![p[0].clone(), p.get(1).cloned().unwrap_or_default()]).collect(),
            )
        }
        "list" => {
            let len: i64 = redis::cmd("LLEN").arg(key).query_async(con).await.unwrap_or(0);
            truncated = len > max;
            let l: Vec<Vec<u8>> = redis::cmd("LRANGE").arg(key).arg(0).arg(max - 1).query_async(con).await.map_err(qerr)?;
            (
                vec!["index".into(), "value".into()],
                l.into_iter().enumerate().map(|(i, v)| vec![i.to_string().into_bytes(), v]).collect(),
            )
        }
        "set" => {
            let (members, more) = scan_all(con, "SSCAN", key, MAX_ELEMENTS).await?;
            truncated = more;
            (vec!["member".into()], members.into_iter().map(|m| vec![m]).collect())
        }
        "zset" => {
            let len: i64 = redis::cmd("ZCARD").arg(key).query_async(con).await.unwrap_or(0);
            truncated = len > max;
            let z: Vec<(Vec<u8>, String)> = redis::cmd("ZRANGE")
                .arg(key)
                .arg(0)
                .arg(max - 1)
                .arg("WITHSCORES")
                .query_async(con)
                .await
                .map_err(qerr)?;
            (vec!["member".into(), "score".into()], z.into_iter().map(|(m, sc)| vec![m, sc.into_bytes()]).collect())
        }
        // stream 不在这里读:按 ID 范围分页浏览,见 edit::stream_range
        _ => (vec![], vec![]),
    };
    let mut raw = vec![];
    let rows = cells
        .into_iter()
        .enumerate()
        .map(|(r, row)| row.into_iter().enumerate().map(|(c, b)| cell_text(b, r, c, &mut raw)).collect())
        .collect();
    Ok(KeyValue { key: key.into(), key_type: t, columns, rows, truncated, binary: !raw.is_empty(), raw })
}

async fn read_detail(con: &mut MultiplexedConnection, key: &str) -> AppResult<KeyDetail> {
    let t: String = redis::cmd("TYPE").arg(key).query_async(con).await.map_err(qerr)?;
    let ttl: i64 = redis::cmd("TTL").arg(key).query_async(con).await.map_err(qerr)?;
    let size_cmd = match t.as_str() {
        "string" => Some("STRLEN"),
        "hash" => Some("HLEN"),
        "list" => Some("LLEN"),
        "set" => Some("SCARD"),
        "zset" => Some("ZCARD"),
        "stream" => Some("XLEN"),
        _ => None,
    };
    let size: i64 = match size_cmd {
        Some(c) => redis::cmd(c).arg(key).query_async(con).await.unwrap_or(0),
        None => 0,
    };
    Ok(KeyDetail { key: key.into(), key_type: t, ttl, size })
}

impl RedisPool {
    pub async fn connect(&self, id: &str, url: &str) -> AppResult<()> {
        let con = open(url).await?;
        self.conns.lock().await.insert(id.to_string(), con);
        self.remember_info(id, url, false).await;
        Ok(())
    }

    /// 按连接地址建连(可能经 SSH 隧道,见 [`open_target`])。
    pub async fn connect_target(&self, id: &str, target: &Target, url: &str, ssl: SslMode) -> AppResult<()> {
        let con = open_target(target, url, ssl).await?;
        self.conns.lock().await.insert(id.to_string(), con);
        self.remember_info(id, url, target.tunneled()).await;
        Ok(())
    }

    async fn remember_info(&self, id: &str, url: &str, tunneled: bool) {
        let mut infos = self.infos.lock().await;
        match redis::Client::open(url) {
            Ok(c) if !tunneled => {
                infos.insert(id.to_string(), c.get_connection_info().clone());
            }
            _ => {
                infos.remove(id);
            }
        }
    }

    /// 概览(INFO);`db` 为当前库号。
    pub async fn overview(&self, id: &str, db: u8) -> AppResult<super::analysis::Overview> {
        super::analysis::overview(&mut self.con(id).await?, db).await
    }

    /// 最近 `count` 条慢日志。
    pub async fn slowlog(&self, id: &str, count: u32) -> AppResult<Vec<super::analysis::SlowEntry>> {
        super::analysis::slowlog(&mut self.con(id).await?, count).await
    }

    /// 开始整库扫描分析(已有进行中的先取消)。多路复用连接:扫描与浏览、命令的请求交错进行,互不阻塞。
    pub async fn start_analysis(&self, id: &str) -> AppResult<()> {
        let con = self.con(id).await?;
        if let Some(old) = self.analyses.lock().await.remove(id) {
            old.cancel();
        }
        let base = self.infos.lock().await.get(id).cloned();
        let a = super::analysis::start(con, base).await?;
        self.analyses.lock().await.insert(id.to_string(), a);
        Ok(())
    }

    /// 分析的当前汇总;没开始过为 None。
    pub async fn analysis_report(&self, id: &str) -> Option<super::analysis::Report> {
        self.analyses.lock().await.get(id).map(|a| a.report())
    }

    pub async fn cancel_analysis(&self, id: &str) {
        if let Some(a) = self.analyses.lock().await.get(id) {
            a.cancel();
        }
    }

    /// 分析排行榜里的 key 所在的连接:集群模式用扫描时连上的那个节点。
    async fn con_for(&self, id: &str, node: Option<&str>) -> AppResult<MultiplexedConnection> {
        match node {
            Some(addr) => self
                .analyses
                .lock()
                .await
                .get(id)
                .and_then(|a| a.node_con(addr))
                .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.not_connected"))),
            None => self.con(id).await,
        }
    }

    /// 查看某个节点上的 key(集群分析的排行榜用;`node` 为 None 即当前连接)。
    pub async fn get_value_on(&self, id: &str, node: Option<&str>, key: &str) -> AppResult<KeyValue> {
        read_value(&mut self.con_for(id, node).await?, key).await
    }

    /// 删除一个 key:优先 UNLINK(后台释放内存,大 key 不阻塞实例),老版本退回 DEL。返回删掉的个数。
    pub async fn delete_key(&self, id: &str, node: Option<&str>, key: &str) -> AppResult<i64> {
        let mut con = self.con_for(id, node).await?;
        let n: i64 = match redis::cmd("UNLINK").arg(key).query_async(&mut con).await {
            Ok(n) => n,
            Err(e) if e.to_string().to_ascii_lowercase().contains("unknown command") => {
                redis::cmd("DEL").arg(key).query_async(&mut con).await.map_err(qerr)?
            }
            Err(e) => return Err(qerr(e)),
        };
        if let Some(a) = self.analyses.lock().await.get(id) {
            a.forget(key, node);
        }
        Ok(n)
    }

    /// MultiplexedConnection 可廉价 clone,克隆出来避免在 await 期间持锁。
    async fn con(&self, id: &str) -> AppResult<MultiplexedConnection> {
        self.conns
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.not_connected")))
    }

    /// SCAN 遍历匹配的 key,最多返回 2000 个(避免超大库卡死)。
    pub async fn scan(&self, id: &str, pattern: &str) -> AppResult<Vec<String>> {
        let mut con = self.con(id).await?;
        let mut cursor: u64 = 0;
        let mut keys: Vec<String> = vec![];
        loop {
            let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(500)
                .query_async(&mut con)
                .await
                .map_err(qerr)?;
            keys.extend(batch);
            cursor = next;
            if cursor == 0 || keys.len() >= 2000 {
                break;
            }
        }
        keys.sort();
        Ok(keys)
    }

    pub async fn get_value(&self, id: &str, key: &str) -> AppResult<KeyValue> {
        read_value(&mut self.con(id).await?, key).await
    }

    pub async fn key_detail(&self, id: &str, key: &str) -> AppResult<KeyDetail> {
        read_detail(&mut self.con(id).await?, key).await
    }

    /// 同 [`Self::key_detail`],集群模式下在 `node` 上读。
    pub async fn key_detail_on(&self, id: &str, node: Option<&str>, key: &str) -> AppResult<KeyDetail> {
        read_detail(&mut self.con_for(id, node).await?, key).await
    }

    /// 元素级编辑(见 [`super::edit::KeyEdit`]);集群模式下在 key 所在的 `node` 上执行。
    pub async fn edit_key(&self, id: &str, node: Option<&str>, key: &str, edit: &super::edit::KeyEdit) -> AppResult<super::edit::EditResult> {
        super::edit::apply(&mut self.con_for(id, node).await?, key, edit).await
    }

    /// 设置(`Some(秒)`)或移除(`None`)TTL。
    pub async fn set_ttl(&self, id: &str, node: Option<&str>, key: &str, seconds: Option<i64>) -> AppResult<()> {
        super::edit::set_ttl(&mut self.con_for(id, node).await?, key, seconds).await
    }

    /// 重命名(RENAMENX)。分析排行榜里的旧名一并拿掉。
    pub async fn rename_key(&self, id: &str, node: Option<&str>, key: &str, new_key: &str) -> AppResult<()> {
        super::edit::rename(&mut self.con_for(id, node).await?, key, new_key).await?;
        if let Some(a) = self.analyses.lock().await.get(id) {
            a.forget(key, node);
        }
        Ok(())
    }

    pub async fn create_key(&self, id: &str, node: Option<&str>, spec: &super::edit::NewKey) -> AppResult<()> {
        super::edit::create(&mut self.con_for(id, node).await?, spec).await
    }

    pub async fn stream_range(&self, id: &str, node: Option<&str>, key: &str, q: &super::edit::StreamRange) -> AppResult<super::edit::StreamPage> {
        super::edit::stream_range(&mut self.con_for(id, node).await?, key, q).await
    }

    pub async fn stream_groups(&self, id: &str, node: Option<&str>, key: &str) -> AppResult<Vec<super::edit::StreamGroup>> {
        super::edit::stream_groups(&mut self.con_for(id, node).await?, key).await
    }

    /// 执行任意命令(按空白拆分),返回可读文本。
    pub async fn exec(&self, id: &str, command: &str) -> AppResult<String> {
        let mut con = self.con(id).await?;
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return Ok(String::new());
        }
        let mut cmd = redis::cmd(parts[0]);
        for a in &parts[1..] {
            cmd.arg(*a);
        }
        let val: redis::Value = cmd.query_async(&mut con).await.map_err(qerr)?;
        Ok(format_reply(&val))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn url() -> String {
        std::env::var("DBSTUDIO_TEST_REDIS").unwrap_or_else(|_| "redis://127.0.0.1:6379/0".into())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn scan_get_and_exec() {
        let pool = RedisPool::default();
        pool.connect("t", &url()).await.unwrap();
        // 自种数据(前缀避免与真实数据冲突)
        pool.exec("t", "SET dbstudio:greeting hello").await.unwrap();
        pool.exec("t", "HSET dbstudio:user name Alice age 30")
            .await
            .unwrap();

        let keys = pool.scan("t", "dbstudio:*").await.unwrap();
        assert!(keys.contains(&"dbstudio:greeting".to_string()));

        let v = pool.get_value("t", "dbstudio:greeting").await.unwrap();
        assert_eq!(v.key_type, "string");
        assert_eq!(v.rows[0][0], "hello");

        let h = pool.get_value("t", "dbstudio:user").await.unwrap();
        assert_eq!(h.key_type, "hash");
        assert!(h.rows.iter().any(|r| r[0] == "name" && r[1] == "Alice"));

        let d = pool.key_detail("t", "dbstudio:user").await.unwrap();
        assert_eq!(d.key_type, "hash");
        assert_eq!(d.size, 2);

        let pong = pool.exec("t", "PING").await.unwrap();
        assert_eq!(pong, "PONG");

        // 清理
        pool.exec("t", "DEL dbstudio:greeting dbstudio:user")
            .await
            .unwrap();
    }
}
