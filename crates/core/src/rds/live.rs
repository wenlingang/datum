//! Redis 实时:Pub/Sub 订阅(SUBSCRIBE / PSUBSCRIBE)与 MONITOR。
//!
//! - **专用连接**:订阅 / MONITOR 模式下的连接不能再执行别的命令,所以每个会话自己开一条连接,
//!   不碰共享的多路复用连接。建连走和普通连接同一个 [`Target`](直连 / 本地转发端口 / SSH 通道)、
//!   同样的认证与 TLS 设置([`LiveParams`] 在建立普通连接时记下)。
//! - **自己读写 RESP2**:redis crate 的 PubSub / Monitor 会把整条消息原样读进内存,而消息内容、
//!   MONITOR 输出都是外部输入,可能很大。这里读的时候就按上限截断,超出部分从网络上读走丢掉,
//!   一条消息占用的内存与它的实际大小无关地有界。
//! - **有界缓冲**:消息进环形缓冲(条数 + 字节双上限),满了丢最旧的并计数;界面按序号增量轮询
//!   (与整库分析的进度轮询同一个模式)。暂停只是界面不拉取,后台照常收(仍受上限约束)。
//! - **释放**:停止时(关 tab、断开连接、MONITOR 到时)退订、关掉专用连接、结束后台任务。
//!
//! 集群:SUBSCRIBE 的消息全集群广播,连任一节点即可收到;7.0 的分片频道(SSUBSCRIBE)没有做。
//! MONITOR 只看得到所连那个节点上执行的命令。

use crate::core::connection::SslMode;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::ssh::{Target, Via};
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};

/// 缓冲里最多保留的消息条数。
pub const MAX_MESSAGES: usize = 5000;
/// 缓冲里所有消息合计的字节上限(条数没到也按它丢最旧的)。
pub const MAX_BUFFER_BYTES: usize = 32 << 20;
/// 一条消息最多保存的字节数(详情里看到的「完整」内容);超出部分读走丢掉。
pub const MAX_STORED_BYTES: usize = 1 << 20;
/// 列表里每条消息只带前这么多字节。
pub const PREVIEW_BYTES: usize = 4 << 10;
/// 一次轮询最多返回的条数(其余下次再拉)。
pub const MAX_POLL: usize = 1000;
/// 一个会话最多订阅的频道 + 模式数。
pub const MAX_SUBSCRIPTIONS: usize = 64;
/// 同时存在的实时会话数上限(防界面漏关时无限累积连接)。
pub const MAX_SESSIONS: usize = 8;
/// MONITOR 默认 / 最长时长(秒)。
pub const MONITOR_DEFAULT_SECS: u64 = 60;
pub const MONITOR_MAX_SECS: u64 = 600;
/// 频道名、模式名、客户端地址保存的上限。
const MAX_NAME: usize = 1024;
/// MONITOR 的命令名最多取的字符数。
const MAX_COMMAND: usize = 64;
/// 类型行(`$123`、`*4`、`:5`)的长度上限:再长就是协议错乱。
const MAX_HEADER: usize = 64;
/// 错误回复保留的字节数。
const MAX_ERROR: usize = 4096;
/// RESP 协议本身允许的最大 bulk 长度。
const MAX_BULK_LEN: usize = 512 << 20;
/// 数组只保留前这么多个元素(Pub/Sub 推送最多 4 个),其余读走丢掉。
const MAX_ARRAY_KEPT: usize = 16;
/// 数组长度上限:再多就是协议错乱。
const MAX_ARRAY_LEN: usize = 1 << 20;
/// 数组嵌套层数上限。
const MAX_DEPTH: usize = 3;
/// 一个顶层帧里所有字符串合计保留的字节数,在单条上限之外多留的余量(频道名、模式名等)。
/// 只限单个 bulk 不够:嵌套数组每层各留 [`MAX_ARRAY_KEPT`] 个元素,保留量会按层相乘。
const FRAME_SLACK: usize = 64 << 10;
/// 识别 JSON 时最多完整解析的字节数(列表里的格式标签,详情里另做完整识别)。
const SNIFF_JSON_BYTES: usize = 16 << 10;
/// 建连 + 握手 + 认证的总时限(同普通连接)。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 停止时发退订命令最多等的时间。
const STOP_TIMEOUT: Duration = Duration::from_secs(1);

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ---------------------------------------------------------------- RESP 读写

/// 截断过的字节串:`data` 最多保留上限个字节,`len` 是原长。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bytes {
    pub data: Vec<u8>,
    pub len: usize,
}

impl Bytes {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.data).into_owned()
    }
}

/// RESP2 的一帧(不协商 RESP3,只会收到这几种)。
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Simple(Bytes),
    Error(String),
    Int(i64),
    Bulk(Option<Bytes>),
    Array(Option<Vec<Frame>>),
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// 有界的 RESP 读取:单行 / 单个 bulk 最多保留 `cap` 字节,超出部分读走丢掉。
pub struct FrameReader<R> {
    r: BufReader<R>,
    cap: usize,
    /// 当前顶层帧还能保留的字节数(每次 `read` 重置为 `cap + FRAME_SLACK`)。
    budget: usize,
}

impl<R: AsyncRead + Unpin + Send> FrameReader<R> {
    pub fn new(r: R, cap: usize) -> Self {
        Self { r: BufReader::new(r), cap, budget: 0 }
    }

    /// 读一行(到 `\n`),去掉行尾 `\r\n`,最多保留 `cap` 字节;`len` 为去掉行尾后的原长。
    async fn line(&mut self, cap: usize) -> io::Result<Bytes> {
        let mut out = Vec::new();
        let mut total = 0usize;
        // 上一块的最后一个字节:行尾的 `\r` 可能落在上一块
        let mut last: Option<u8> = None;
        loop {
            let buf = self.r.fill_buf().await?;
            if buf.is_empty() {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let (n, done) = match buf.iter().position(|&b| b == b'\n') {
                Some(i) => (i + 1, true),
                None => (buf.len(), false),
            };
            let chunk = &buf[..n];
            let room = cap.saturating_sub(out.len());
            out.extend_from_slice(&chunk[..n.min(room)]);
            total += n;
            if done {
                let cr = if n >= 2 { chunk[n - 2] == b'\r' } else { last == Some(b'\r') };
                self.r.consume(n);
                let len = total - 1 - usize::from(cr);
                out.truncate(len);
                return Ok(Bytes { data: out, len });
            }
            last = chunk.last().copied();
            self.r.consume(n);
        }
    }

    /// 读走 `n` 个字节丢掉。
    async fn skip(&mut self, n: u64) -> io::Result<()> {
        let copied = tokio::io::copy(&mut (&mut self.r).take(n), &mut tokio::io::sink()).await?;
        if copied < n {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(())
    }

    async fn header_num(&mut self) -> io::Result<i64> {
        let l = self.line(MAX_HEADER).await?;
        if l.len > MAX_HEADER {
            return Err(bad("RESP header too long"));
        }
        std::str::from_utf8(&l.data)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| bad("bad RESP number"))
    }

    pub async fn read(&mut self) -> io::Result<Frame> {
        self.budget = self.cap.saturating_add(FRAME_SLACK);
        self.frame(0).await
    }

    /// 从本帧预算里扣出最多 `want` 字节的保留额度。
    fn take_budget(&mut self, want: usize) -> usize {
        let keep = want.min(self.cap).min(self.budget);
        self.budget -= keep;
        keep
    }

    fn frame(&mut self, depth: usize) -> Pin<Box<dyn Future<Output = io::Result<Frame>> + Send + '_>> {
        Box::pin(async move {
            let kind = self.r.read_u8().await?;
            match kind {
                b'+' => {
                    let keep = self.take_budget(self.cap);
                    let l = self.line(keep).await?;
                    // 没用完的额度还回去(line 只保留实际长度)
                    self.budget += keep - l.data.len();
                    Ok(Frame::Simple(l))
                }
                b'-' => Ok(Frame::Error(self.line(MAX_ERROR).await?.text())),
                b':' => Ok(Frame::Int(self.header_num().await?)),
                b'$' => {
                    let n = self.header_num().await?;
                    if n < 0 {
                        return Ok(Frame::Bulk(None));
                    }
                    let n = n as usize;
                    if n > MAX_BULK_LEN {
                        return Err(bad("RESP bulk too long"));
                    }
                    let keep = self.take_budget(n);
                    let mut data = vec![0u8; keep];
                    self.r.read_exact(&mut data).await?;
                    // 超出上限的部分与行尾 \r\n 一起读走
                    self.skip((n - keep) as u64 + 2).await?;
                    Ok(Frame::Bulk(Some(Bytes { data, len: n })))
                }
                b'*' => {
                    let n = self.header_num().await?;
                    if n < 0 {
                        return Ok(Frame::Array(None));
                    }
                    if depth >= MAX_DEPTH || n as usize > MAX_ARRAY_LEN {
                        return Err(bad("RESP array too deep or too long"));
                    }
                    let mut items = Vec::with_capacity((n as usize).min(MAX_ARRAY_KEPT));
                    for i in 0..n as usize {
                        let f = self.frame(depth + 1).await?;
                        if i < MAX_ARRAY_KEPT {
                            items.push(f);
                        }
                    }
                    Ok(Frame::Array(Some(items)))
                }
                other => Err(bad(format!("unexpected RESP type byte {other:#04x}"))),
            }
        })
    }
}

/// 把命令编码成 RESP 数组。
pub fn encode(args: &[&[u8]]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", args.len()).into_bytes();
    for a in args {
        out.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
        out.extend_from_slice(a);
        out.extend_from_slice(b"\r\n");
    }
    out
}

// ---------------------------------------------------------------- 专用连接

trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}
type Stream = Box<dyn Io>;
type Reader = FrameReader<ReadHalf<Stream>>;
type Writer = WriteHalf<Stream>;

/// 建专用连接需要的参数:和普通连接同一个目标(含 SSH 隧道)、同样的认证与 TLS。
#[derive(Clone)]
pub struct LiveParams {
    pub target: Target,
    pub user: String,
    pub pass: String,
    pub ssl: SslMode,
}

impl LiveParams {
    /// 从连接串取认证与加密设置;`target` 为实际要连的地址(可能经隧道)。
    pub fn from_url(target: Target, url: &str, ssl: SslMode) -> Option<Self> {
        let info = redis::Client::open(url).ok()?.get_connection_info().clone();
        Some(Self {
            target,
            user: info.redis.username.unwrap_or_default(),
            pass: info.redis.password.unwrap_or_default(),
            ssl,
        })
    }

    /// 只有连接串时(直连):地址、认证、加密都从连接串里取。
    pub fn from_plain_url(url: &str) -> Option<Self> {
        let info = redis::Client::open(url).ok()?.get_connection_info().clone();
        let (target, ssl) = match &info.addr {
            redis::ConnectionAddr::Tcp(h, p) => (Target::direct(h, *p), SslMode::Disable),
            redis::ConnectionAddr::TcpTls { host, port, insecure, .. } => {
                (Target::direct(host, *port), if *insecure { SslMode::Require } else { SslMode::VerifyFull })
            }
            _ => return None,
        };
        Some(Self {
            target,
            user: info.redis.username.unwrap_or_default(),
            pass: info.redis.password.unwrap_or_default(),
            ssl,
        })
    }
}

async fn connect_stream(p: &LiveParams) -> Result<Stream, String> {
    let raw: Stream = match p.target.open_stream().await {
        Some(s) => Box::new(s.map_err(|e| e.display())?),
        None => match &p.target.via {
            #[cfg(unix)]
            Some(Via::Socket(path)) => Box::new(tokio::net::UnixStream::connect(path).await.map_err(|e| e.to_string())?),
            _ => {
                let tcp = tokio::net::TcpStream::connect((p.target.host.as_str(), p.target.port))
                    .await
                    .map_err(|e| e.to_string())?;
                let _ = tcp.set_nodelay(true);
                Box::new(tcp)
            }
        },
    };
    // 「优先」与普通连接一致:Redis 不能在同一端口协商加密,等同不加密
    let verify = match p.ssl {
        SslMode::Disable | SslMode::Prefer => return Ok(raw),
        SslMode::Require => false,
        SslMode::VerifyFull => true,
    };
    crate::tls::install_default_provider();
    let name = rustls::pki_types::ServerName::try_from(p.target.tls_host.clone()).map_err(|e| e.to_string())?;
    let tls = tokio_rustls::TlsConnector::from(Arc::new(crate::tls::client_config(verify)))
        .connect(name, raw)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Box::new(tls))
}

async fn send(w: &mut Writer, args: &[&[u8]]) -> io::Result<()> {
    w.write_all(&encode(args)).await?;
    w.flush().await
}

fn connect_failed(detail: String) -> AppError {
    AppError::new(ErrorKind::Connection, t!("core.err.connect_failed", db = "Redis")).with_detail(detail)
}

/// 建专用连接并认证;`name` 设为 CLIENT SETNAME,便于在 CLIENT LIST 里认出来(也方便测试确认已关闭)。
async fn open(p: &LiveParams, name: &str) -> AppResult<(Reader, Writer)> {
    let fut = async {
        let stream = connect_stream(p).await?;
        let (r, mut w) = tokio::io::split(stream);
        let mut rd = FrameReader::new(r, MAX_STORED_BYTES);
        if !p.pass.is_empty() {
            let mut args: Vec<&[u8]> = vec![b"AUTH"];
            if !p.user.is_empty() {
                args.push(p.user.as_bytes());
            }
            args.push(p.pass.as_bytes());
            send(&mut w, &args).await.map_err(|e| e.to_string())?;
            if let Frame::Error(e) = rd.read().await.map_err(|e| e.to_string())? {
                return Err(e);
            }
        }
        send(&mut w, &[b"CLIENT", b"SETNAME", name.as_bytes()]).await.map_err(|e| e.to_string())?;
        // 老版本 / 被禁用都不影响后续,回复只读掉
        rd.read().await.map_err(|e| e.to_string())?;
        Ok((rd, w))
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, fut).await {
        Ok(r) => r.map_err(connect_failed),
        Err(_) => Err(connect_failed(t!("core.err.connect_timeout").to_string())),
    }
}

// ---------------------------------------------------------------- 会话

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LiveKind {
    Pubsub,
    Monitor,
}

/// 一个订阅:频道或模式。`active` 为服务器已确认。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sub {
    pub pattern: bool,
    pub name: String,
    #[serde(default)]
    pub active: bool,
}

/// MONITOR 的过滤条件(在本地过滤:服务器照样把全部命令发过来)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorFilter {
    /// 命令名,空格或逗号分隔多个,不分大小写;空为不限
    #[serde(default)]
    pub command: String,
    /// 参数里包含的子串(区分大小写);空为不限
    #[serde(default)]
    pub key: String,
}

impl MonitorFilter {
    fn commands(&self) -> Vec<String> {
        self.command
            .split([' ', ',', '\t'])
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_uppercase())
            .collect()
    }

    fn matches(&self, commands: &[String], e: &Entry) -> bool {
        (commands.is_empty() || commands.contains(&e.command))
            && (self.key.is_empty() || contains(&e.data, self.key.as_bytes()))
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle)
}

/// 会话为什么结束了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EndReason {
    /// 用户停止 / 关 tab / 断开连接
    Stopped,
    /// MONITOR 到时自动停止
    Timeout,
    /// 服务器关了连接(重启、CLIENT KILL、超出输出缓冲上限…)
    Closed,
    /// 读写出错
    Error,
}

/// 缓冲上限(测试里调小)。
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_messages: usize,
    pub max_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_messages: MAX_MESSAGES, max_bytes: MAX_BUFFER_BYTES }
    }
}

/// 缓冲里的一条消息。
#[derive(Debug, Clone, Default)]
struct Entry {
    seq: u64,
    time: i64,
    /// Pub/Sub:频道
    channel: String,
    /// Pub/Sub 模式订阅收到的:匹配的模式
    pattern: Option<String>,
    /// Pub/Sub:消息内容;MONITOR:命令及参数(服务器转义过的文本)。最多 [`MAX_STORED_BYTES`]
    data: Vec<u8>,
    /// 原始字节数
    size: usize,
    /// MONITOR:客户端地址、库号、命令名(大写)
    client: String,
    db: Option<u32>,
    command: String,
}

impl Entry {
    fn cost(&self) -> usize {
        self.data.len() + self.channel.len() + self.pattern.as_ref().map_or(0, |p| p.len()) + self.client.len() + self.command.len() + 64
    }

    /// 不是合法 UTF-8(在保存上限处截断造成的半个字符不算)。
    fn binary(&self) -> bool {
        match std::str::from_utf8(&self.data) {
            Ok(_) => false,
            Err(e) => !(e.error_len().is_none() && self.size > self.data.len()),
        }
    }
}

/// 环形缓冲:条数与字节数都不超过上限,超出丢最旧的。
#[derive(Debug)]
struct Buffer {
    entries: VecDeque<Entry>,
    bytes: usize,
    next_seq: u64,
    dropped: u64,
    limits: Limits,
}

impl Buffer {
    fn new(limits: Limits) -> Self {
        Self { entries: VecDeque::new(), bytes: 0, next_seq: 1, dropped: 0, limits }
    }

    fn push(&mut self, mut e: Entry) {
        e.seq = self.next_seq;
        self.next_seq += 1;
        self.bytes += e.cost();
        self.entries.push_back(e);
        while self.entries.len() > self.limits.max_messages || (self.bytes > self.limits.max_bytes && self.entries.len() > 1) {
            if let Some(old) = self.entries.pop_front() {
                self.bytes -= old.cost();
                self.dropped += 1;
            }
        }
    }

    fn retain(&mut self, keep: impl Fn(&Entry) -> bool) -> u64 {
        let before = self.entries.len();
        self.entries.retain(|e| keep(e));
        self.bytes = self.entries.iter().map(Entry::cost).sum();
        (before - self.entries.len()) as u64
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    fn get(&self, seq: u64) -> Option<&Entry> {
        let i = self.entries.partition_point(|e| e.seq < seq);
        self.entries.get(i).filter(|e| e.seq == seq)
    }
}

/// 列表里的一条(内容只带前 [`PREVIEW_BYTES`])。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LiveItem {
    pub seq: u64,
    /// 毫秒时间戳:Pub/Sub 为收到的时间,MONITOR 为服务器记录的时间
    pub time: i64,
    pub channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    pub text: String,
    pub size: usize,
    /// 列表里的内容不完整(详情里看保存下来的部分)
    pub truncated: bool,
    pub binary: bool,
    /// 粗略识别的格式(json / gzip / zlib / java / text / hex),列表里的标签用;详情里另做完整识别
    pub format: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub client: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db: Option<u32>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub command: String,
}

/// 一次轮询的结果。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LivePoll {
    pub kind: LiveKind,
    pub running: bool,
    pub ended: Option<EndReason>,
    /// 最近一次服务器返回的错误(如没有权限订阅)或读写错误
    pub error: Option<String>,
    /// 序号大于 `since` 的消息,最多 [`MAX_POLL`] 条
    pub items: Vec<LiveItem>,
    /// 还有没返回的(下次接着拉)
    pub more: bool,
    /// 缓冲里最旧一条的序号(缓冲为空时为下一条的序号):`since` 比它小 1 以上说明中间的被丢了
    pub first_seq: u64,
    /// 缓冲里的条数
    pub count: usize,
    /// 超出上限丢掉的条数(累计)
    pub dropped: u64,
    /// 收到的条数(累计,含被过滤掉的)
    pub received: u64,
    /// MONITOR 被过滤掉的条数(累计)
    pub filtered: u64,
    pub subs: Vec<Sub>,
    pub filter: MonitorFilter,
    pub started: i64,
    /// MONITOR 自动停止的时间(毫秒时间戳)
    pub deadline: Option<i64>,
    pub max_messages: usize,
}

/// 详情:保存下来的完整内容(最多 [`MAX_STORED_BYTES`])。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LiveDetail {
    pub seq: u64,
    pub time: i64,
    pub channel: String,
    pub pattern: Option<String>,
    pub text: String,
    /// 不是合法 UTF-8 时的原始字节(base64),格式识别要用
    pub b64: Option<String>,
    pub size: usize,
    /// 保存下来的字节数;小于 `size` 说明超出了上限,只保存了开头
    pub stored: usize,
    pub client: String,
    pub db: Option<u32>,
    pub command: String,
}

#[derive(Debug)]
struct State {
    buf: Buffer,
    running: bool,
    ended: Option<EndReason>,
    error: Option<String>,
    received: u64,
    filtered: u64,
    subs: Vec<Sub>,
    filter: MonitorFilter,
    /// `filter.commands()` 的缓存
    commands: Vec<String>,
    started: i64,
    deadline: Option<i64>,
}

/// 一个实时会话(Pub/Sub 或 MONITOR),独占一条连接。
pub struct LiveSession {
    pub kind: LiveKind,
    pub conn_id: String,
    /// 专用连接的 CLIENT SETNAME(`datum-pubsub-xxxxxxxx`),便于在 CLIENT LIST 里认出 / 确认已关闭
    pub client_name: String,
    state: Mutex<State>,
    writer: tokio::sync::Mutex<Option<Writer>>,
    task: Mutex<Option<tokio::task::AbortHandle>>,
}

fn validate_subs(subs: &[Sub]) -> AppResult<()> {
    if subs.iter().any(|s| s.name.is_empty() || s.name.len() > MAX_NAME) {
        return Err(AppError::new(ErrorKind::Query, t!("core.err.redis_live_bad_channel")));
    }
    Ok(())
}

fn truncate_name(b: &Bytes) -> String {
    let s = String::from_utf8_lossy(&b.data[..b.data.len().min(MAX_NAME)]).into_owned();
    if b.len > MAX_NAME { format!("{s}…") } else { s }
}

fn bulk(f: Option<&Frame>) -> Option<&Bytes> {
    match f {
        Some(Frame::Bulk(Some(b))) | Some(Frame::Simple(b)) => Some(b),
        _ => None,
    }
}

/// 粗略识别格式(只看开头的特征;详情里用 decode 完整识别)。
fn sniff(e: &Entry) -> &'static str {
    let d = &e.data;
    if d.starts_with(&[0x1f, 0x8b]) {
        return "gzip";
    }
    if d.len() >= 2 && d[0] == 0x78 && matches!(d[1], 0x01 | 0x5e | 0x9c | 0xda) && (u16::from(d[0]) << 8 | u16::from(d[1])) % 31 == 0 {
        return "zlib";
    }
    if d.starts_with(&[0xac, 0xed, 0x00, 0x05]) {
        return "java";
    }
    if e.binary() {
        return "hex";
    }
    let first = d.iter().find(|b| !b.is_ascii_whitespace());
    if matches!(first, Some(b'{') | Some(b'[')) && e.size == d.len() && d.len() <= SNIFF_JSON_BYTES && serde_json::from_slice::<serde::de::IgnoredAny>(d).is_ok() {
        return "json";
    }
    "text"
}

/// 前 `max` 字节的文本(UTF-8 在字符边界处截断)与实际用了的字节数。
fn preview(data: &[u8], max: usize) -> (String, usize) {
    if data.len() <= max {
        return (String::from_utf8_lossy(data).into_owned(), data.len());
    }
    let mut end = max;
    while end > 0 && (data[end] & 0xc0) == 0x80 {
        end -= 1;
    }
    (String::from_utf8_lossy(&data[..end]).into_owned(), end)
}

impl LiveSession {
    #[allow(clippy::too_many_arguments)]
    fn new(kind: LiveKind, conn_id: &str, client_name: String, limits: Limits, subs: Vec<Sub>, filter: MonitorFilter, deadline: Option<i64>, writer: Writer) -> Arc<Self> {
        let commands = filter.commands();
        Arc::new(Self {
            kind,
            conn_id: conn_id.to_string(),
            client_name,
            state: Mutex::new(State {
                buf: Buffer::new(limits),
                running: true,
                ended: None,
                error: None,
                received: 0,
                filtered: 0,
                subs,
                filter,
                commands,
                started: now_ms(),
                deadline,
            }),
            writer: tokio::sync::Mutex::new(Some(writer)),
            task: Mutex::new(None),
        })
    }

    pub fn running(&self) -> bool {
        self.state.lock().unwrap().running
    }

    /// 序号大于 `since` 的消息(最多 `limit` 条,且不超过 [`MAX_POLL`])与当前状态。
    pub fn poll(&self, since: u64, limit: usize) -> LivePoll {
        let s = self.state.lock().unwrap();
        let limit = limit.clamp(1, MAX_POLL);
        let start = s.buf.entries.partition_point(|e| e.seq <= since);
        let items: Vec<LiveItem> = s
            .buf
            .entries
            .iter()
            .skip(start)
            .take(limit)
            .map(|e| {
                let (text, used) = preview(&e.data, PREVIEW_BYTES);
                LiveItem {
                    seq: e.seq,
                    time: e.time,
                    channel: e.channel.clone(),
                    pattern: e.pattern.clone(),
                    truncated: e.size > used,
                    binary: e.binary(),
                    format: sniff(e),
                    text,
                    size: e.size,
                    client: e.client.clone(),
                    db: e.db,
                    command: e.command.clone(),
                }
            })
            .collect();
        let more = s.buf.entries.len() > start + items.len();
        LivePoll {
            kind: self.kind,
            running: s.running,
            ended: s.ended,
            error: s.error.clone(),
            items,
            more,
            first_seq: s.buf.entries.front().map_or(s.buf.next_seq, |e| e.seq),
            count: s.buf.entries.len(),
            dropped: s.buf.dropped,
            received: s.received,
            filtered: s.filtered,
            subs: s.subs.clone(),
            filter: s.filter.clone(),
            started: s.started,
            deadline: s.deadline,
            max_messages: s.buf.limits.max_messages,
        }
    }

    /// 一条消息保存下来的完整内容;已被丢掉 / 清空时为 None。
    pub fn detail(&self, seq: u64) -> Option<LiveDetail> {
        use base64::Engine;
        let s = self.state.lock().unwrap();
        let e = s.buf.get(seq)?;
        let binary = e.binary();
        Some(LiveDetail {
            seq: e.seq,
            time: e.time,
            channel: e.channel.clone(),
            pattern: e.pattern.clone(),
            text: String::from_utf8_lossy(&e.data).into_owned(),
            b64: binary.then(|| base64::engine::general_purpose::STANDARD.encode(&e.data)),
            size: e.size,
            stored: e.data.len(),
            client: e.client.clone(),
            db: e.db,
            command: e.command.clone(),
        })
    }

    /// 清空缓冲(序号继续递增,界面据此认出清空前的旧消息)。
    pub fn clear(&self) {
        self.state.lock().unwrap().buf.clear();
    }

    /// 换 MONITOR 的过滤条件:之后收到的按新条件过滤,缓冲里已有的不符合的也拿掉。
    pub fn set_filter(&self, filter: MonitorFilter) {
        let mut s = self.state.lock().unwrap();
        let commands = filter.commands();
        let removed = s.buf.retain(|e| filter.matches(&commands, e));
        s.filtered += removed;
        s.filter = filter;
        s.commands = commands;
    }

    /// 增加订阅(Pub/Sub 会话)。已订阅的跳过。
    pub async fn subscribe(&self, add: &[Sub]) -> AppResult<()> {
        validate_subs(add)?;
        let (channels, patterns) = {
            let mut s = self.state.lock().unwrap();
            if !s.running {
                return Err(AppError::new(ErrorKind::Connection, t!("core.err.redis_live_ended")));
            }
            let fresh: Vec<Sub> = add
                .iter()
                .filter(|a| !s.subs.iter().any(|x| x.pattern == a.pattern && x.name == a.name))
                .map(|a| Sub { pattern: a.pattern, name: a.name.clone(), active: false })
                .collect();
            if s.subs.len() + fresh.len() > MAX_SUBSCRIPTIONS {
                return Err(AppError::new(ErrorKind::Query, t!("core.err.redis_live_too_many_subs", max = MAX_SUBSCRIPTIONS)));
            }
            s.subs.extend(fresh.iter().cloned());
            split_subs(&fresh)
        };
        self.send_subs(b"SUBSCRIBE", &channels).await?;
        self.send_subs(b"PSUBSCRIBE", &patterns).await
    }

    /// 取消订阅。
    pub async fn unsubscribe(&self, remove: &[Sub]) -> AppResult<()> {
        let (channels, patterns) = {
            let mut s = self.state.lock().unwrap();
            s.subs.retain(|x| !remove.iter().any(|r| r.pattern == x.pattern && r.name == x.name));
            split_subs(remove)
        };
        self.send_subs(b"UNSUBSCRIBE", &channels).await?;
        self.send_subs(b"PUNSUBSCRIBE", &patterns).await
    }

    async fn send_subs(&self, cmd: &[u8], names: &[String]) -> AppResult<()> {
        if names.is_empty() {
            return Ok(());
        }
        let mut args: Vec<&[u8]> = vec![cmd];
        args.extend(names.iter().map(|n| n.as_bytes()));
        let mut w = self.writer.lock().await;
        let w = w.as_mut().ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.redis_live_ended")))?;
        send(w, &args)
            .await
            .map_err(|e| AppError::new(ErrorKind::Connection, t!("core.err.redis_command_failed")).with_detail(e.to_string()))
    }

    /// 停止:Pub/Sub 先退订,再关掉专用连接、结束后台任务。可重复调用。
    pub async fn stop(&self) {
        {
            let mut s = self.state.lock().unwrap();
            if s.running {
                s.running = false;
                s.ended = Some(EndReason::Stopped);
            }
        }
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
        let w = self.writer.lock().await.take();
        if let Some(mut w) = w {
            let _ = tokio::time::timeout(STOP_TIMEOUT, async {
                if self.kind == LiveKind::Pubsub {
                    let _ = send(&mut w, &[b"UNSUBSCRIBE"]).await;
                    let _ = send(&mut w, &[b"PUNSUBSCRIBE"]).await;
                }
                let _ = w.shutdown().await;
            })
            .await;
            // 读半边随任务一起释放,两半都放掉连接才真正关闭
        }
    }

    /// 后台任务自己结束(到时、对端关闭、出错)时收尾。
    async fn finish(&self, reason: EndReason, error: Option<String>) {
        {
            let mut s = self.state.lock().unwrap();
            if s.running {
                s.running = false;
                s.ended = Some(reason);
                if error.is_some() {
                    s.error = error;
                }
            }
        }
        self.task.lock().unwrap().take();
        if let Some(mut w) = self.writer.lock().await.take() {
            let _ = tokio::time::timeout(STOP_TIMEOUT, w.shutdown()).await;
        }
    }

    fn handle_pubsub(&self, f: Frame) {
        let mut s = self.state.lock().unwrap();
        match f {
            Frame::Array(Some(items)) => {
                let kind = bulk(items.first()).map(|b| b.text().to_ascii_lowercase()).unwrap_or_default();
                match kind.as_str() {
                    "message" | "pmessage" => {
                        let pm = kind == "pmessage";
                        let at = |i: usize| bulk(items.get(if pm { i + 1 } else { i }));
                        let (Some(ch), Some(data)) = (at(1), at(2)) else { return };
                        s.received += 1;
                        let e = Entry {
                            time: now_ms(),
                            channel: truncate_name(ch),
                            pattern: if pm { bulk(items.get(1)).map(truncate_name) } else { None },
                            data: data.data.clone(),
                            size: data.len,
                            ..Default::default()
                        };
                        s.buf.push(e);
                    }
                    "subscribe" | "psubscribe" => {
                        if let Some(name) = bulk(items.get(1)) {
                            let pattern = kind == "psubscribe";
                            let name = name.text();
                            if let Some(x) = s.subs.iter_mut().find(|x| x.pattern == pattern && x.name == name) {
                                x.active = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Frame::Error(e) => s.error = Some(e),
            _ => {}
        }
    }

    fn handle_monitor(&self, f: Frame) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        match f {
            Frame::Simple(line) => {
                let Some(mut e) = parse_monitor(&line.data) else { return Ok(()) };
                // 行在保存上限处截断过:参数文本同样不完整
                e.size = e.data.len() + (line.len - line.data.len());
                s.received += 1;
                if s.filter.matches(&s.commands, &e) {
                    s.buf.push(e);
                } else {
                    s.filtered += 1;
                }
                Ok(())
            }
            Frame::Error(e) => Err(e),
            _ => Ok(()),
        }
    }
}

fn split_subs(subs: &[Sub]) -> (Vec<String>, Vec<String>) {
    let channels = subs.iter().filter(|s| !s.pattern).map(|s| s.name.clone()).collect();
    let patterns = subs.iter().filter(|s| s.pattern).map(|s| s.name.clone()).collect();
    (channels, patterns)
}

/// 后台读循环:直到停止(任务被取消)、到时、对端关闭或出错。
async fn run(s: Arc<LiveSession>, mut rd: Reader, deadline: Option<tokio::time::Instant>) {
    let (reason, error) = loop {
        let f = match deadline {
            Some(d) => match tokio::time::timeout_at(d, rd.read()).await {
                Ok(f) => f,
                Err(_) => break (EndReason::Timeout, None),
            },
            None => rd.read().await,
        };
        match f {
            Ok(f) => {
                if s.kind == LiveKind::Monitor {
                    if let Err(e) = s.handle_monitor(f) {
                        break (EndReason::Error, Some(e));
                    }
                } else {
                    s.handle_pubsub(f);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof || e.kind() == io::ErrorKind::ConnectionReset => {
                break (EndReason::Closed, None)
            }
            Err(e) => break (EndReason::Error, Some(e.to_string())),
        }
    };
    drop(rd);
    s.finish(reason, error).await;
}

fn spawn(s: &Arc<LiveSession>, rd: Reader, deadline: Option<tokio::time::Instant>) {
    let handle = tokio::spawn(run(s.clone(), rd, deadline));
    *s.task.lock().unwrap() = Some(handle.abort_handle());
}

fn client_name(kind: &str) -> String {
    format!("datum-{kind}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// 开始 Pub/Sub 会话:建专用连接,订阅 `subs`(可以为空,之后再加)。
pub async fn start_pubsub(conn_id: &str, p: &LiveParams, subs: Vec<Sub>, limits: Limits) -> AppResult<Arc<LiveSession>> {
    validate_subs(&subs)?;
    if subs.len() > MAX_SUBSCRIPTIONS {
        return Err(AppError::new(ErrorKind::Query, t!("core.err.redis_live_too_many_subs", max = MAX_SUBSCRIPTIONS)));
    }
    let name = client_name("pubsub");
    let (rd, w) = open(p, &name).await?;
    let mut uniq: Vec<Sub> = vec![];
    for x in subs {
        if !uniq.iter().any(|u| u.pattern == x.pattern && u.name == x.name) {
            uniq.push(Sub { active: false, ..x });
        }
    }
    let (channels, patterns) = split_subs(&uniq);
    let s = LiveSession::new(LiveKind::Pubsub, conn_id, name, limits, uniq, MonitorFilter::default(), None, w);
    // 先起读循环再发订阅:确认与消息都由它处理
    spawn(&s, rd, None);
    let sent = async {
        s.send_subs(b"SUBSCRIBE", &channels).await?;
        s.send_subs(b"PSUBSCRIBE", &patterns).await
    };
    if let Err(e) = sent.await {
        s.stop().await;
        return Err(e);
    }
    Ok(s)
}

/// 开始 MONITOR:建专用连接,发 MONITOR 并等到 +OK(没有权限等错误直接报出来),`secs` 秒后自动停止。
pub async fn start_monitor(conn_id: &str, p: &LiveParams, secs: u64, filter: MonitorFilter, limits: Limits) -> AppResult<Arc<LiveSession>> {
    let secs = secs.clamp(1, MONITOR_MAX_SECS);
    let name = client_name("monitor");
    let (mut rd, mut w) = open(p, &name).await?;
    let failed = |d: String| AppError::new(ErrorKind::Query, t!("core.err.redis_command_failed")).with_detail(d);
    let ok = async {
        send(&mut w, &[b"MONITOR"]).await.map_err(|e| e.to_string())?;
        match rd.read().await.map_err(|e| e.to_string())? {
            Frame::Error(e) => Err(e),
            _ => Ok(()),
        }
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, ok).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(failed(e)),
        Err(_) => return Err(failed(t!("core.err.connect_timeout").to_string())),
    }
    let dur = Duration::from_secs(secs);
    let deadline_ms = now_ms() + dur.as_millis() as i64;
    let s = LiveSession::new(LiveKind::Monitor, conn_id, name, limits, vec![], filter, Some(deadline_ms), w);
    spawn(&s, rd, Some(tokio::time::Instant::now() + dur));
    Ok(s)
}

// ---------------------------------------------------------------- MONITOR 行

/// 解析 MONITOR 的一行:`1339518083.107412 [0 127.0.0.1:60866] "keys" "*"`
/// (客户端也可能是 `lua`、`unix:/tmp/redis.sock`、IPv6 `[::1]:6379`)。
/// 参数部分保留服务器转义过的原文(二进制已转成 `\xhh`,不含换行)。
fn parse_monitor(line: &[u8]) -> Option<Entry> {
    let sp = line.iter().position(|&b| b == b' ')?;
    let ts: f64 = std::str::from_utf8(&line[..sp]).ok()?.parse().ok()?;
    let rest = &line[sp + 1..];
    if rest.first() != Some(&b'[') {
        return None;
    }
    // 方括号里的地址本身可能带方括号(IPv6),以 `] "` 作为结束
    let close = rest.windows(3).position(|w| w == b"] \"")?;
    let inner = &rest[1..close];
    let args = &rest[close + 2..];
    let (db, client) = match inner.iter().position(|&b| b == b' ') {
        Some(i) => (std::str::from_utf8(&inner[..i]).ok().and_then(|d| d.parse().ok()), &inner[i + 1..]),
        None => (None, inner),
    };
    let command = first_arg(args).to_ascii_uppercase();
    Some(Entry {
        time: (ts * 1000.0) as i64,
        client: String::from_utf8_lossy(&client[..client.len().min(MAX_NAME)]).into_owned(),
        db,
        command,
        size: args.len(),
        data: args.to_vec(),
        ..Default::default()
    })
}

/// 第一个带引号的参数(反转义),最多 [`MAX_COMMAND`] 个字符。
fn first_arg(args: &[u8]) -> String {
    let mut out = String::new();
    let mut it = args.iter().skip(1);
    while let Some(&b) = it.next() {
        if out.len() >= MAX_COMMAND {
            break;
        }
        match b {
            b'"' => break,
            b'\\' => match it.next() {
                Some(&c) => out.push(c as char),
                None => break,
            },
            _ => out.push(b as char),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn frames(input: &[u8], cap: usize) -> Vec<Frame> {
        let mut rd = FrameReader::new(input, cap);
        let mut out = vec![];
        while let Ok(f) = rd.read().await {
            out.push(f);
        }
        out
    }

    fn b(s: &[u8]) -> Bytes {
        Bytes { data: s.to_vec(), len: s.len() }
    }

    #[tokio::test]
    async fn reads_resp_frames() {
        let f = frames(b"+OK\r\n-ERR no\r\n:12\r\n$3\r\nabc\r\n$-1\r\n*3\r\n$7\r\nmessage\r\n$2\r\nch\r\n$5\r\nhello\r\n", 1024).await;
        assert_eq!(
            f,
            vec![
                Frame::Simple(b(b"OK")),
                Frame::Error("ERR no".into()),
                Frame::Int(12),
                Frame::Bulk(Some(b(b"abc"))),
                Frame::Bulk(None),
                Frame::Array(Some(vec![Frame::Bulk(Some(b(b"message"))), Frame::Bulk(Some(b(b"ch"))), Frame::Bulk(Some(b(b"hello")))])),
            ]
        );
    }

    #[tokio::test]
    async fn truncates_big_bulk_and_line_but_keeps_stream_in_sync() {
        let mut input = b"$10\r\n0123456789\r\n+".to_vec();
        input.extend(vec![b'x'; 100]);
        input.extend(b"\r\n:7\r\n");
        let f = frames(&input, 4).await;
        assert_eq!(
            f,
            vec![
                Frame::Bulk(Some(Bytes { data: b"0123".to_vec(), len: 10 })),
                Frame::Simple(Bytes { data: b"xxxx".to_vec(), len: 100 }),
                Frame::Int(7),
            ]
        );
    }

    fn kept_bytes(f: &Frame) -> usize {
        match f {
            Frame::Simple(b) | Frame::Bulk(Some(b)) => b.data.len(),
            Frame::Array(Some(items)) => items.iter().map(kept_bytes).sum(),
            _ => 0,
        }
    }

    #[tokio::test]
    async fn nested_arrays_share_one_budget_per_frame() {
        // 3 层嵌套、每层 16 个元素、每个 bulk 都到单条上限:不设整帧预算时保留量是 16³ × cap
        let cap = 1024;
        let bulk = format!("${cap}\r\n{}\r\n", "x".repeat(cap));
        let lvl1 = format!("*16\r\n{}", bulk.repeat(16));
        let lvl2 = format!("*16\r\n{}", lvl1.repeat(16));
        let mut input = format!("*16\r\n{}", lvl2.repeat(16)).into_bytes();
        input.extend(b":7\r\n");
        let f = frames(&input, cap).await;
        assert_eq!(f.len(), 2, "后续帧要照常对齐");
        assert!(kept_bytes(&f[0]) <= cap + FRAME_SLACK, "kept {}", kept_bytes(&f[0]));
        assert_eq!(f[1], Frame::Int(7));
    }

    #[tokio::test]
    async fn rejects_garbage_and_huge_headers() {
        assert!(frames(b"?what\r\n", 16).await.is_empty());
        assert!(frames(b"$999999999999\r\n", 16).await.is_empty());
        let deep = b"*1\r\n*1\r\n*1\r\n*1\r\n:1\r\n";
        assert!(frames(deep, 16).await.is_empty());
    }

    #[tokio::test]
    async fn keeps_only_first_array_items() {
        let mut input = b"*20\r\n".to_vec();
        for i in 0..20 {
            input.extend(format!(":{i}\r\n").as_bytes());
        }
        input.extend(b":99\r\n");
        let f = frames(&input, 16).await;
        let Frame::Array(Some(items)) = &f[0] else { panic!() };
        assert_eq!(items.len(), MAX_ARRAY_KEPT);
        assert_eq!(f[1], Frame::Int(99));
    }

    #[test]
    fn encodes_commands() {
        assert_eq!(encode(&[b"PUBLISH", b"ch", b"hi"]), b"*3\r\n$7\r\nPUBLISH\r\n$2\r\nch\r\n$2\r\nhi\r\n");
    }

    #[test]
    fn parses_monitor_lines() {
        let e = parse_monitor(br#"1339518083.107412 [0 127.0.0.1:60866] "set" "k\"1" "v""#).unwrap();
        assert_eq!(e.time, 1339518083107);
        assert_eq!(e.db, Some(0));
        assert_eq!(e.client, "127.0.0.1:60866");
        assert_eq!(e.command, "SET");
        assert_eq!(e.data, br#""set" "k\"1" "v""#);
        let e = parse_monitor(br#"1.5 [3 lua] "get" "x""#).unwrap();
        assert_eq!((e.db, e.client.as_str(), e.command.as_str()), (Some(3), "lua", "GET"));
        let e = parse_monitor(br#"1.5 [0 [::1]:6379] "ping""#).unwrap();
        assert_eq!(e.client, "[::1]:6379");
        assert!(parse_monitor(b"OK").is_none());
    }

    #[test]
    fn buffer_drops_oldest_beyond_limits() {
        let mut buf = Buffer::new(Limits { max_messages: 3, max_bytes: usize::MAX });
        for i in 0..5 {
            buf.push(Entry { data: vec![i], size: 1, ..Default::default() });
        }
        assert_eq!(buf.entries.len(), 3);
        assert_eq!(buf.dropped, 2);
        assert_eq!(buf.entries.front().unwrap().seq, 3);
        assert!(buf.get(2).is_none());
        assert_eq!(buf.get(4).unwrap().data, vec![3]);

        // 字节上限:大消息挤掉旧的,但至少留最新一条
        let mut buf = Buffer::new(Limits { max_messages: 100, max_bytes: 1000 });
        for _ in 0..3 {
            buf.push(Entry { data: vec![0; 400], size: 400, ..Default::default() });
        }
        assert_eq!(buf.entries.len(), 2);
        buf.push(Entry { data: vec![0; 5000], size: 5000, ..Default::default() });
        assert_eq!(buf.entries.len(), 1);
    }

    #[test]
    fn monitor_filter_matches_command_and_key() {
        let f = MonitorFilter { command: "get, set".into(), key: "user:".into() };
        let cmds = f.commands();
        let e = parse_monitor(br#"1 [0 x] "SET" "user:1" "v""#).unwrap();
        assert!(f.matches(&cmds, &e));
        let e = parse_monitor(br#"1 [0 x] "DEL" "user:1""#).unwrap();
        assert!(!f.matches(&cmds, &e));
        let e = parse_monitor(br#"1 [0 x] "GET" "order:1""#).unwrap();
        assert!(!f.matches(&cmds, &e));
    }

    #[test]
    fn sniffs_formats_and_previews() {
        let e = |d: &[u8]| Entry { data: d.to_vec(), size: d.len(), ..Default::default() };
        assert_eq!(sniff(&e(br#"{"a":1}"#)), "json");
        assert_eq!(sniff(&e(b"[INFO] started")), "text");
        assert_eq!(sniff(&e(&[0x1f, 0x8b, 8, 0])), "gzip");
        assert_eq!(sniff(&e(&[0xff, 0xfe, 0x00])), "hex");
        // 截断在多字节字符中间:不算二进制
        let mut t = Entry { data: "好".as_bytes()[..2].to_vec(), size: 3, ..Default::default() };
        assert!(!t.binary());
        t.size = 2;
        assert!(t.binary());
        assert_eq!(preview("好好".as_bytes(), 4), ("好".to_string(), 3));
    }
}
