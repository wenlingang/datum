//! Redis 分析:概览(INFO)、慢日志(SLOWLOG)与整库扫描(SCAN + 管道里的 TYPE / PTTL /
//! MEMORY USAGE / OBJECT FREQ),汇总出最大的 key、前缀占用、TTL 与类型分布、热 key。
//!
//! 扫描在后台任务里渐进进行,界面轮询进度,可随时取消;每批之间稍作停顿,减轻对线上实例的压力。
//! 超过 [`MAX_KEYS`] 就停,报告标为抽样。汇总([`Aggregator`])是纯逻辑,单独测试。
//!
//! 前缀按 `:` 分级统计到第 [`PREFIX_DEPTH`] 级;集群模式(cluster_enabled)下逐个主节点扫描,
//! 每个 key 记下所在节点,查看 / 删除时连到那个节点。

use crate::error::{AppError, AppResult, ErrorKind};
use redis::aio::MultiplexedConnection;
use redis::Value;
use rust_i18n::t;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 一次分析最多扫描的 key 数;超过即停,报告标为抽样。
pub const MAX_KEYS: u64 = 200_000;
/// 每次 SCAN 的 COUNT。
const BATCH: u64 = 500;
/// 批与批之间的停顿。
const PAUSE: Duration = Duration::from_millis(5);
/// 各排行榜的条数。
const TOP: usize = 20;
/// 前缀榜的条数。
const TOP_PREFIXES: usize = 30;
/// 前缀统计的层数(`a:*` → `a:b:*` → `a:b:c:*`)。
pub const PREFIX_DEPTH: usize = 3;
/// 每个前缀下最多跟踪多少个不同的子前缀;再多的并进「其余」(`…`),内存与 key 的花样无关地有界。
const CHILD_CAP: usize = 500;
/// 报告里每个前缀展开后列出的子前缀数。
const TOP_CHILDREN: usize = 15;
/// 子前缀放不下时并入的名字(最后一段)。
pub const OTHER_SEGMENT: &str = "…";

fn qerr(e: redis::RedisError) -> AppError {
    AppError::new(ErrorKind::Query, t!("core.err.redis_command_failed")).with_detail(e.to_string())
}

// ---------------------------------------------------------------- 概览

/// `INFO` 里分析页关心的几项(原样字符串,缺的为 None)。
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub version: Option<String>,
    pub used_memory: Option<String>,
    pub used_memory_peak: Option<String>,
    pub max_memory: Option<String>,
    pub max_memory_policy: Option<String>,
    pub fragmentation_ratio: Option<String>,
    pub connected_clients: Option<String>,
    /// 命中率(%),hits + misses 为 0 时为 None
    pub hit_rate: Option<f64>,
    pub evicted_keys: Option<String>,
    pub expired_keys: Option<String>,
    /// 当前库的 key 数与设了过期时间的 key 数
    pub keys: Option<u64>,
    pub expires: Option<u64>,
}

/// 解析 `INFO` 的文本(`k:v` 行),`db` 为当前库号。
pub fn parse_info(text: &str, db: u8) -> Overview {
    let map: HashMap<&str, &str> = text
        .lines()
        .filter_map(|l| l.trim().split_once(':'))
        .collect();
    let get = |k: &str| map.get(k).map(|v| v.to_string());
    let num = |k: &str| map.get(k).and_then(|v| v.parse::<f64>().ok());
    let hit_rate = match (num("keyspace_hits"), num("keyspace_misses")) {
        (Some(h), Some(m)) if h + m > 0.0 => Some((h / (h + m) * 1000.0).round() / 10.0),
        _ => None,
    };
    // db0:keys=12,expires=3,avg_ttl=0
    let (keys, expires) = map
        .get(format!("db{db}").as_str())
        .map(|v| {
            let field = |name: &str| {
                v.split(',')
                    .find_map(|kv| kv.strip_prefix(&format!("{name}=")).and_then(|n| n.parse().ok()))
            };
            (field("keys"), field("expires"))
        })
        .unwrap_or((Some(0), Some(0)));
    Overview {
        version: get("redis_version"),
        used_memory: get("used_memory_human"),
        used_memory_peak: get("used_memory_peak_human"),
        max_memory: get("maxmemory_human"),
        max_memory_policy: get("maxmemory_policy"),
        fragmentation_ratio: get("mem_fragmentation_ratio"),
        connected_clients: get("connected_clients"),
        hit_rate,
        evicted_keys: get("evicted_keys"),
        expired_keys: get("expired_keys"),
        keys,
        expires,
    }
}

pub async fn overview(con: &mut MultiplexedConnection, db: u8) -> AppResult<Overview> {
    let text: String = redis::cmd("INFO").query_async(con).await.map_err(qerr)?;
    Ok(parse_info(&text, db))
}

// ---------------------------------------------------------------- 慢日志

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SlowEntry {
    pub id: i64,
    /// Unix 秒
    pub time: i64,
    pub micros: i64,
    pub command: String,
    pub client: String,
}

fn text_of(v: &Value) -> String {
    match v {
        Value::BulkString(b) => String::from_utf8_lossy(b).into_owned(),
        Value::SimpleString(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::VerbatimString { text, .. } => text.clone(),
        _ => String::new(),
    }
}

fn int_of(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Int(i)) => *i,
        Some(other) => text_of(other).parse().unwrap_or(0),
        None => 0,
    }
}

/// 解析 `SLOWLOG GET` 的回复:每条 `[id, 时间, 微秒, [参数…], 客户端地址, 客户端名]`(后两项 4.0 起才有)。
pub fn parse_slowlog(v: &Value) -> Vec<SlowEntry> {
    let Value::Array(items) = v else {
        return vec![];
    };
    items
        .iter()
        .filter_map(|e| {
            let Value::Array(f) = e else {
                return None;
            };
            let args: Vec<String> = match f.get(3) {
                Some(Value::Array(a)) => a.iter().map(text_of).collect(),
                _ => vec![],
            };
            let addr = f.get(4).map(text_of).unwrap_or_default();
            let name = f.get(5).map(text_of).unwrap_or_default();
            Some(SlowEntry {
                id: int_of(f.first()),
                time: int_of(f.get(1)),
                micros: int_of(f.get(2)),
                command: args.join(" "),
                client: if name.is_empty() { addr } else { format!("{addr} ({name})") },
            })
        })
        .collect()
}

pub async fn slowlog(con: &mut MultiplexedConnection, count: u32) -> AppResult<Vec<SlowEntry>> {
    let v: Value = redis::cmd("SLOWLOG")
        .arg("GET")
        .arg(count)
        .query_async(con)
        .await
        .map_err(qerr)?;
    Ok(parse_slowlog(&v))
}

// ---------------------------------------------------------------- 汇总

/// 一个 key 的扫描结果。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeyStat {
    pub key: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// MEMORY USAGE(字节);实例不支持时为 None
    pub bytes: Option<u64>,
    /// 剩余毫秒;-1 永不过期
    pub ttl_ms: i64,
    /// OBJECT FREQ(LFU 访问频度);非 LFU 策略为 None
    pub freq: Option<u64>,
    /// 集群模式:key 所在的主节点(host:port);单机为 None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

/// 集群里一个主节点的扫描情况。
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NodeStat {
    pub addr: String,
    /// 扫描开始时的 DBSIZE
    pub keys: u64,
    /// 连不上 / 扫描出错的原因
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GroupStat {
    pub name: String,
    pub keys: u64,
    pub bytes: u64,
}

/// TTL 分布:永不过期、1 小时内、1 天内、7 天内、更久。
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TtlBuckets {
    pub persistent: u64,
    pub within_hour: u64,
    pub within_day: u64,
    pub within_week: u64,
    pub longer: u64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// 已扫描的 key 数
    pub scanned: u64,
    /// 扫描开始时的 DBSIZE(进度的分母)
    pub total: u64,
    pub done: bool,
    pub cancelled: bool,
    /// 达到上限提前停止:结果是抽样
    pub truncated: bool,
    pub error: Option<String>,
    /// 实例是否支持 MEMORY USAGE(不支持时只按 key 数统计)
    pub memory_supported: bool,
    /// 淘汰策略是 LFU 才统计热 key
    pub lfu: bool,
    pub total_bytes: u64,
    pub biggest: Vec<KeyStat>,
    pub hottest: Vec<KeyStat>,
    /// 第一级前缀(`name` 是前缀本身,不含 `:*`)
    pub prefixes: Vec<GroupStat>,
    /// 前缀 → 下一级子前缀(`name` 是完整前缀,如 `user:profile`);只列报告里出现的前缀
    pub prefix_children: std::collections::BTreeMap<String, Vec<GroupStat>>,
    /// 集群模式:逐个主节点扫描
    pub cluster: bool,
    pub nodes: Vec<NodeStat>,
    pub types: Vec<GroupStat>,
    pub ttl: TtlBuckets,
}

/// key 的前缀:第一个 `:` 之前;没有 `:` 归入空前缀(界面显示为「无前缀」)。
pub fn prefix_of(key: &str) -> &str {
    key.split_once(':').map(|(p, _)| p).unwrap_or("")
}

/// key 的各级前缀(不含 key 本身的最后一段):`a:b:c:d` → `a`、`a:b`、`a:b:c`(最多 [`PREFIX_DEPTH`] 级)。
pub fn prefix_path(key: &str) -> Vec<&str> {
    key.match_indices(':').take(PREFIX_DEPTH).map(|(i, _)| &key[..i]).collect()
}

/// 渐进汇总扫描结果。排行榜只留前 N(插入时维护),内存与 key 数无关地有界。
#[derive(Debug, Default)]
pub struct Aggregator {
    report: Report,
    prefixes: HashMap<String, GroupStat>,
    /// 第二级起的子前缀(完整前缀 → 统计)
    sub: HashMap<String, GroupStat>,
    /// 前缀 → 已跟踪的不同子前缀个数
    child_count: HashMap<String, usize>,
    types: HashMap<String, GroupStat>,
}

/// 按 `score` 降序维护前 `n` 名。
fn push_top(list: &mut Vec<KeyStat>, s: &KeyStat, score: fn(&KeyStat) -> u64, n: usize) {
    if list.len() >= n && list.last().is_some_and(|last| score(last) >= score(s)) {
        return;
    }
    let at = list.partition_point(|x| score(x) >= score(s));
    list.insert(at, s.clone());
    list.truncate(n);
}

impl Aggregator {
    pub fn new(total: u64, memory_supported: bool, lfu: bool) -> Self {
        Self {
            report: Report { total, memory_supported, lfu, ..Default::default() },
            ..Default::default()
        }
    }

    pub fn add(&mut self, s: &KeyStat) {
        let r = &mut self.report;
        r.scanned += 1;
        let bytes = s.bytes.unwrap_or(0);
        r.total_bytes += bytes;
        let p = self.prefixes.entry(prefix_of(&s.key).to_string()).or_default();
        p.keys += 1;
        p.bytes += bytes;
        // 第二级起:子前缀太多时并入父前缀下的「其余」
        let path = prefix_path(&s.key);
        let mut parent: Option<String> = path.first().map(|p| p.to_string());
        for full in path.iter().skip(1) {
            let Some(par) = parent.clone() else { break };
            let name = if self.sub.contains_key(*full) {
                full.to_string()
            } else {
                let n = self.child_count.entry(par.clone()).or_default();
                if *n < CHILD_CAP {
                    *n += 1;
                    full.to_string()
                } else {
                    // 「其余」只计数,不再往下分
                    let other = format!("{par}:{OTHER_SEGMENT}");
                    let g = self.sub.entry(other).or_default();
                    g.keys += 1;
                    g.bytes += bytes;
                    break;
                }
            };
            let g = self.sub.entry(name.clone()).or_default();
            g.keys += 1;
            g.bytes += bytes;
            parent = Some(name);
        }
        let t = self.types.entry(s.kind.clone()).or_default();
        t.keys += 1;
        t.bytes += bytes;
        let ttl = &mut r.ttl;
        match s.ttl_ms {
            ms if ms < 0 => ttl.persistent += 1,
            ms if ms <= 3_600_000 => ttl.within_hour += 1,
            ms if ms <= 86_400_000 => ttl.within_day += 1,
            ms if ms <= 604_800_000 => ttl.within_week += 1,
            _ => ttl.longer += 1,
        }
        if r.memory_supported {
            push_top(&mut r.biggest, s, |x| x.bytes.unwrap_or(0), TOP);
        }
        if r.lfu && s.freq.is_some() {
            push_top(&mut r.hottest, s, |x| x.freq.unwrap_or(0), TOP);
        }
    }

    /// 当前汇总(前缀与类型按内存、再按 key 数降序)。
    pub fn report(&self) -> Report {
        let sort = |m: &HashMap<String, GroupStat>, n: usize| {
            let mut v: Vec<GroupStat> = m
                .iter()
                .map(|(name, g)| GroupStat { name: name.clone(), ..g.clone() })
                .collect();
            v.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(b.keys.cmp(&a.keys)).then(a.name.cmp(&b.name)));
            v.truncate(n);
            v
        };
        let prefixes = sort(&self.prefixes, TOP_PREFIXES);
        // 子前缀:从报告里的第一级前缀往下展开,每级取前 TOP_CHILDREN 个
        let mut children: HashMap<&str, Vec<(&String, &GroupStat)>> = HashMap::new();
        for (name, g) in &self.sub {
            if let Some((par, _)) = name.rsplit_once(':') {
                children.entry(par).or_default().push((name, g));
            }
        }
        let mut prefix_children = std::collections::BTreeMap::new();
        let mut queue: Vec<String> = prefixes.iter().map(|g| g.name.clone()).filter(|n| !n.is_empty()).collect();
        while let Some(par) = queue.pop() {
            let Some(list) = children.get(par.as_str()) else { continue };
            let mut v: Vec<GroupStat> = list.iter().map(|(n, g)| GroupStat { name: (*n).clone(), ..(*g).clone() }).collect();
            v.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(b.keys.cmp(&a.keys)).then(a.name.cmp(&b.name)));
            v.truncate(TOP_CHILDREN);
            queue.extend(v.iter().map(|g| g.name.clone()).filter(|n| !n.ends_with(OTHER_SEGMENT)));
            prefix_children.insert(par, v);
        }
        Report {
            prefixes,
            prefix_children,
            types: sort(&self.types, usize::MAX),
            ..self.report.clone()
        }
    }

    /// 删掉的 key 从排行榜里拿掉(统计不回退:删除前它确实占着这些内存)。
    pub fn forget(&mut self, key: &str, node: Option<&str>) {
        let keep = |k: &KeyStat| !(k.key == key && k.node.as_deref() == node);
        self.report.biggest.retain(keep);
        self.report.hottest.retain(keep);
    }

    fn set_nodes(&mut self, nodes: Vec<NodeStat>) {
        self.report.cluster = true;
        self.report.nodes = nodes;
    }

    fn node_error(&mut self, addr: &str, error: String) {
        if let Some(n) = self.report.nodes.iter_mut().find(|n| n.addr == addr) {
            n.error = Some(error);
        }
    }

    pub fn finish(&mut self, cancelled: bool, truncated: bool, error: Option<String>) {
        self.report.done = true;
        self.report.cancelled = cancelled;
        self.report.truncated = truncated;
        self.report.error = error;
    }
}

// ---------------------------------------------------------------- 扫描

/// 一次进行中的(或已结束的)分析。
#[derive(Default)]
pub struct Analysis {
    agg: Mutex<Aggregator>,
    cancel: AtomicBool,
    /// 集群模式:各主节点的连接(查看 / 删除排行榜里的 key 时用)
    nodes: Mutex<HashMap<String, MultiplexedConnection>>,
}

impl Analysis {
    pub fn report(&self) -> Report {
        self.agg.lock().unwrap().report()
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    /// 集群节点的连接(MultiplexedConnection 可廉价 clone)。
    pub fn node_con(&self, addr: &str) -> Option<MultiplexedConnection> {
        self.nodes.lock().unwrap().get(addr).cloned()
    }
    pub fn forget(&self, key: &str, node: Option<&str>) {
        self.agg.lock().unwrap().forget(key, node);
    }
}

/// `CLUSTER NODES` 里在线的主节点:(host, port, 是否就是当前连着的节点)。
/// 行格式:`id ip:port@cport[,hostname] flags master_id ping pong epoch link_state slot…`;
/// ip 为空(节点还不知道自己的地址)时用 `fallback_host`。
pub fn parse_cluster_masters(text: &str, fallback_host: &str) -> Vec<(String, u16, bool)> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let (addr, flags, link) = (f.get(1)?, f.get(2)?, f.get(7)?);
            let flags: Vec<&str> = flags.split(',').collect();
            if !flags.contains(&"master") || flags.iter().any(|x| x.starts_with("fail") || *x == "noaddr" || *x == "handshake") {
                return None;
            }
            if *link != "connected" {
                return None;
            }
            let hostport = addr.split(['@', ',']).next()?;
            let (host, port) = hostport.rsplit_once(':')?;
            let port: u16 = port.parse().ok()?;
            let host = if host.is_empty() { fallback_host } else { host };
            Some((host.to_string(), port, flags.contains(&"myself")))
        })
        .collect()
}

/// 用同样的认证 / 加密设置连到集群里的另一个节点。
async fn open_node(base: &redis::ConnectionInfo, host: &str, port: u16) -> Result<MultiplexedConnection, String> {
    let mut info = base.clone();
    info.redis.db = 0;
    info.addr = match &info.addr {
        redis::ConnectionAddr::TcpTls { insecure, tls_params, .. } => redis::ConnectionAddr::TcpTls {
            host: host.to_string(),
            port,
            insecure: *insecure,
            tls_params: tls_params.clone(),
        },
        _ => redis::ConnectionAddr::Tcp(host.to_string(), port),
    };
    let client = redis::Client::open(info).map_err(|e| e.to_string())?;
    match tokio::time::timeout(Duration::from_secs(10), client.get_multiplexed_async_connection()).await {
        Ok(Ok(c)) => Ok(c),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(t!("core.err.connect_timeout").to_string()),
    }
}

fn is_unknown_command(e: &redis::RedisError) -> bool {
    let s = e.to_string().to_ascii_lowercase();
    s.contains("unknown command") || s.contains("unknown subcommand") || s.contains("noperm") || s.contains("disabled")
}

/// 开始扫描:先探测实例能力(DBSIZE、MEMORY USAGE、淘汰策略、是否集群),再在后台逐批扫描。
/// `base`:建这条连接用的参数(集群模式下照着它连其余主节点);经 SSH 隧道时为 None,只扫当前节点。
pub async fn start(mut con: MultiplexedConnection, base: Option<redis::ConnectionInfo>) -> AppResult<Arc<Analysis>> {
    let info: String = redis::cmd("INFO").arg("memory").query_async(&mut con).await.map_err(qerr)?;
    let lfu = parse_info(&info, 0).max_memory_policy.is_some_and(|p| p.contains("lfu"));
    // MEMORY USAGE 不存在的 key 返回 nil;命令被禁用 / 不认识则报错
    let memory_supported = match redis::cmd("MEMORY")
        .arg("USAGE")
        .arg("__datum_probe__")
        .query_async::<Option<u64>>(&mut con)
        .await
    {
        Ok(_) => true,
        Err(e) if is_unknown_command(&e) => false,
        Err(e) => return Err(qerr(e)),
    };
    let cluster_info: String = redis::cmd("INFO").arg("cluster").query_async(&mut con).await.unwrap_or_default();
    let cluster = cluster_info.lines().any(|l| l.trim() == "cluster_enabled:1");

    // 要扫描的节点:(地址, 连接);集群时连不上的节点记下原因
    let mut targets: Vec<(Option<String>, MultiplexedConnection)> = vec![];
    let mut node_stats: Vec<NodeStat> = vec![];
    if cluster {
        let text: String = redis::cmd("CLUSTER").arg("NODES").query_async(&mut con).await.map_err(qerr)?;
        let fallback = match base.as_ref().map(|b| &b.addr) {
            Some(redis::ConnectionAddr::Tcp(h, _)) | Some(redis::ConnectionAddr::TcpTls { host: h, .. }) => h.clone(),
            _ => "127.0.0.1".into(),
        };
        for (host, port, is_me) in parse_cluster_masters(&text, &fallback) {
            let addr = format!("{host}:{port}");
            let opened = if is_me {
                Ok(con.clone())
            } else {
                match &base {
                    Some(b) => open_node(b, &host, port).await,
                    None => Err(t!("core.err.redis_cluster_tunnel").to_string()),
                }
            };
            match opened {
                Ok(mut c) => {
                    let keys: u64 = redis::cmd("DBSIZE").query_async(&mut c).await.unwrap_or(0);
                    node_stats.push(NodeStat { addr: addr.clone(), keys, error: None });
                    targets.push((Some(addr), c));
                }
                Err(e) => node_stats.push(NodeStat { addr, keys: 0, error: Some(e) }),
            }
        }
        if targets.is_empty() {
            targets.push((None, con.clone()));
        }
    } else {
        targets.push((None, con.clone()));
    }
    let total: u64 = if cluster {
        node_stats.iter().map(|n| n.keys).sum()
    } else {
        redis::cmd("DBSIZE").query_async(&mut con).await.map_err(qerr)?
    };

    let mut agg = Aggregator::new(total, memory_supported, lfu);
    if cluster {
        agg.set_nodes(node_stats);
    }
    let analysis = Arc::new(Analysis {
        agg: Mutex::new(agg),
        cancel: AtomicBool::new(false),
        nodes: Mutex::new(targets.iter().filter_map(|(a, c)| Some((a.clone()?, c.clone()))).collect()),
    });
    let a = analysis.clone();
    tokio::spawn(async move {
        let mut outcome = (false, false, None);
        let mut seen = 0u64;
        for (node, mut c) in targets {
            match scan_all(&mut c, &a, memory_supported, lfu, node.as_deref(), &mut seen).await {
                Ok((false, false)) => {}
                Ok((cancelled, truncated)) => {
                    outcome = (cancelled, truncated, None);
                    break;
                }
                // 集群里一个节点出错:记在节点上,接着扫别的节点
                Err(e) if node.is_some() => a.agg.lock().unwrap().node_error(node.as_deref().unwrap_or(""), e.display()),
                Err(e) => {
                    outcome = (false, false, Some(e.display()));
                    break;
                }
            }
        }
        a.agg.lock().unwrap().finish(outcome.0, outcome.1, outcome.2);
    });
    Ok(analysis)
}

/// 逐批 SCAN,每批用一个管道取各 key 的类型、剩余时间、内存与访问频度。返回 (取消, 截断)。
async fn scan_all(
    con: &mut MultiplexedConnection,
    a: &Analysis,
    memory: bool,
    lfu: bool,
    node: Option<&str>,
    seen: &mut u64,
) -> AppResult<(bool, bool)> {
    let mut cursor: u64 = 0;
    loop {
        if a.cancel.load(Ordering::Relaxed) {
            return Ok((true, false));
        }
        let (next, keys): (u64, Vec<Vec<u8>>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("COUNT")
            .arg(BATCH)
            .query_async(con)
            .await
            .map_err(qerr)?;
        if !keys.is_empty() {
            let mut pipe = redis::pipe();
            for k in &keys {
                pipe.cmd("TYPE").arg(k).cmd("PTTL").arg(k);
                if memory {
                    pipe.cmd("MEMORY").arg("USAGE").arg(k).arg("SAMPLES").arg(5);
                }
                if lfu {
                    // 已确认是 LFU 策略:不存在的 key 回 nil,不会报错(管道里任一条报错整批作废)
                    pipe.cmd("OBJECT").arg("FREQ").arg(k);
                }
            }
            let values: Vec<Value> = pipe.query_async(con).await.map_err(qerr)?;
            let per = 2 + usize::from(memory) + usize::from(lfu);
            let mut agg = a.agg.lock().unwrap();
            for (i, k) in keys.iter().enumerate() {
                let v = &values[i * per..(i + 1) * per];
                let kind = text_of(&v[0]);
                if kind == "none" {
                    continue; // 扫描期间被删
                }
                let mut j = 2;
                let bytes = memory.then(|| {
                    j += 1;
                    u64::try_from(int_of(Some(&v[j - 1]))).unwrap_or(0)
                });
                let freq = lfu.then(|| u64::try_from(int_of(Some(&v[j]))).unwrap_or(0));
                agg.add(&KeyStat {
                    key: String::from_utf8_lossy(k).into_owned(),
                    kind,
                    bytes,
                    ttl_ms: int_of(Some(&v[1])),
                    freq,
                    node: node.map(str::to_string),
                });
            }
            *seen += keys.len() as u64;
        }
        cursor = next;
        if cursor == 0 {
            return Ok((false, false));
        }
        if *seen >= MAX_KEYS {
            return Ok((false, true));
        }
        tokio::time::sleep(PAUSE).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(key: &str, kind: &str, bytes: u64, ttl_ms: i64, freq: Option<u64>) -> KeyStat {
        KeyStat { key: key.into(), kind: kind.into(), bytes: Some(bytes), ttl_ms, freq, node: None }
    }

    #[test]
    fn info_overview() {
        let text = "# Server\r\nredis_version:7.2.4\r\n# Memory\r\nused_memory_human:1.50M\r\nmaxmemory_human:0B\r\nmaxmemory_policy:allkeys-lfu\r\nmem_fragmentation_ratio:1.23\r\n# Stats\r\nkeyspace_hits:90\r\nkeyspace_misses:10\r\nevicted_keys:0\r\n# Keyspace\r\ndb0:keys=12,expires=3,avg_ttl=0\r\ndb2:keys=5,expires=0,avg_ttl=0\r\n";
        let o = parse_info(text, 2);
        assert_eq!(o.version.as_deref(), Some("7.2.4"));
        assert_eq!(o.used_memory.as_deref(), Some("1.50M"));
        assert_eq!(o.max_memory_policy.as_deref(), Some("allkeys-lfu"));
        assert_eq!(o.hit_rate, Some(90.0));
        assert_eq!((o.keys, o.expires), (Some(5), Some(0)));
        // 空库:Keyspace 里没有这一行
        assert_eq!(parse_info(text, 9).keys, Some(0));
        assert_eq!(parse_info("keyspace_hits:0\nkeyspace_misses:0", 0).hit_rate, None);
    }

    #[test]
    fn slowlog_entries() {
        let bulk = |s: &str| Value::BulkString(s.as_bytes().to_vec());
        let v = Value::Array(vec![
            Value::Array(vec![
                Value::Int(7),
                Value::Int(1_700_000_000),
                Value::Int(15_000),
                Value::Array(vec![bulk("KEYS"), bulk("*")]),
                bulk("10.0.0.5:51234"),
                bulk("worker"),
            ]),
            // 3.x:没有客户端两项
            Value::Array(vec![Value::Int(6), Value::Int(1), Value::Int(2), Value::Array(vec![bulk("GET"), bulk("k")])]),
        ]);
        let e = parse_slowlog(&v);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].command, "KEYS *");
        assert_eq!(e[0].micros, 15_000);
        assert_eq!(e[0].client, "10.0.0.5:51234 (worker)");
        assert_eq!(e[1].client, "");
    }

    #[test]
    fn aggregates_prefixes_types_ttl_and_tops() {
        let mut a = Aggregator::new(6, true, true);
        a.add(&stat("user:1", "hash", 100, -1, Some(3)));
        a.add(&stat("user:2", "hash", 300, 10_000, Some(9)));
        a.add(&stat("session:abc", "string", 50, 7_200_000, Some(1)));
        a.add(&stat("big", "list", 1_000, 3 * 86_400_000, Some(0)));
        a.add(&stat("cache:x", "string", 10, 30 * 86_400_000, None));
        let r = a.report();
        assert_eq!(r.scanned, 5);
        assert_eq!(r.total_bytes, 1_460);
        assert_eq!(r.biggest.iter().map(|k| k.key.as_str()).collect::<Vec<_>>(), ["big", "user:2", "user:1", "session:abc", "cache:x"]);
        assert_eq!(r.hottest.first().map(|k| k.key.as_str()), Some("user:2"));
        assert_eq!(r.prefixes[0], GroupStat { name: "".into(), keys: 1, bytes: 1_000 });
        assert_eq!(r.prefixes[1], GroupStat { name: "user".into(), keys: 2, bytes: 400 });
        assert_eq!(r.types[0].name, "list");
        assert_eq!(
            r.ttl,
            TtlBuckets { persistent: 1, within_hour: 1, within_day: 1, within_week: 1, longer: 1 }
        );
        assert!(!r.done);
        a.finish(false, true, None);
        assert!(a.report().done && a.report().truncated);
    }

    #[test]
    fn top_lists_are_bounded() {
        let mut a = Aggregator::new(100, true, false);
        for i in 0..100u64 {
            a.add(&stat(&format!("k:{i}"), "string", i, -1, None));
        }
        let r = a.report();
        assert_eq!(r.biggest.len(), TOP);
        assert_eq!(r.biggest[0].bytes, Some(99));
        assert_eq!(r.biggest[TOP - 1].bytes, Some(80));
        assert!(r.hottest.is_empty(), "非 LFU 不统计热 key");
    }

    #[test]
    fn multi_level_prefixes() {
        let mut a = Aggregator::new(10, true, false);
        a.add(&stat("user:profile:1", "hash", 100, -1, None));
        a.add(&stat("user:profile:2", "hash", 100, -1, None));
        a.add(&stat("user:session:9", "string", 10, -1, None));
        a.add(&stat("user:42", "string", 1, -1, None));
        a.add(&stat("user:profile:avatar:7", "string", 500, -1, None));
        let r = a.report();
        let names = |p: &str| r.prefix_children.get(p).map(|v| v.iter().map(|g| (g.name.as_str(), g.keys)).collect::<Vec<_>>());
        // user 下:profile(3 个 key)与 session;user:42 是 key 本身,不算子前缀
        assert_eq!(names("user"), Some(vec![("user:profile", 3), ("user:session", 1)]));
        assert_eq!(names("user:profile"), Some(vec![("user:profile:avatar", 1)]));
        assert_eq!(prefix_path("a:b:c:d:e"), vec!["a", "a:b", "a:b:c"]);
        assert!(prefix_path("plain").is_empty());
    }

    #[test]
    fn child_prefixes_are_capped() {
        let mut a = Aggregator::new(0, true, false);
        for i in 0..(CHILD_CAP + 50) {
            a.add(&stat(&format!("sess:{i}:x"), "string", 1, -1, None));
        }
        let r = a.report();
        let kids = &r.prefix_children["sess"];
        assert_eq!(kids.len(), TOP_CHILDREN);
        // 放不下的 50 个并进「其余」;它是最大的一组,排第一
        assert_eq!(kids[0].name, format!("sess:{OTHER_SEGMENT}"));
        assert_eq!(kids[0].keys, 50);
    }

    #[test]
    fn forget_removes_from_rankings() {
        let mut a = Aggregator::new(2, true, true);
        let mut k = stat("big", "list", 1000, -1, Some(5));
        k.node = Some("10.0.0.1:7000".into());
        a.add(&k);
        a.add(&stat("big", "list", 900, -1, Some(4)));
        a.forget("big", Some("10.0.0.1:7000"));
        let r = a.report();
        assert_eq!(r.biggest.len(), 1, "只删那个节点上的");
        assert_eq!(r.biggest[0].node, None);
    }

    #[test]
    fn cluster_nodes_masters() {
        let text = "\
07c3 127.0.0.1:7000@17000 myself,master - 0 0 1 connected 0-5460
67ed 127.0.0.1:7001@17001,host-b master - 0 1426238317239 2 connected 5461-10922
292f :7002@17002 master - 0 1426238316232 3 connected 10923-16383
e7d1 127.0.0.1:7003@17003 slave 07c3 0 1426238318243 1 connected
aa11 127.0.0.1:7004@17004 master,fail - 0 0 4 disconnected
";
        assert_eq!(
            parse_cluster_masters(text, "redis.local"),
            vec![
                ("127.0.0.1".to_string(), 7000, true),
                ("127.0.0.1".to_string(), 7001, false),
                ("redis.local".to_string(), 7002, false),
            ]
        );
    }

    #[test]
    fn prefix_rules() {
        assert_eq!(prefix_of("user:1:profile"), "user");
        assert_eq!(prefix_of("plain"), "");
        assert_eq!(prefix_of(":odd"), "");
    }
}
