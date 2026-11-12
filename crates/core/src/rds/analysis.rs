//! Redis 分析:概览(INFO)、慢日志(SLOWLOG)与整库扫描(SCAN + 管道里的 TYPE / PTTL /
//! MEMORY USAGE / OBJECT FREQ),汇总出最大的 key、前缀占用、TTL 与类型分布、热 key。
//!
//! 扫描在后台任务里渐进进行,界面轮询进度,可随时取消;每批之间稍作停顿,减轻对线上实例的压力。
//! 超过 [`MAX_KEYS`] 就停,报告标为抽样。汇总([`Aggregator`])是纯逻辑,单独测试。

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
    pub prefixes: Vec<GroupStat>,
    pub types: Vec<GroupStat>,
    pub ttl: TtlBuckets,
}

/// key 的前缀:第一个 `:` 之前;没有 `:` 归入空前缀(界面显示为「无前缀」)。
pub fn prefix_of(key: &str) -> &str {
    key.split_once(':').map(|(p, _)| p).unwrap_or("")
}

/// 渐进汇总扫描结果。排行榜只留前 N(插入时维护),内存与 key 数无关地有界。
#[derive(Debug, Default)]
pub struct Aggregator {
    report: Report,
    prefixes: HashMap<String, GroupStat>,
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
        Report {
            prefixes: sort(&self.prefixes, TOP_PREFIXES),
            types: sort(&self.types, usize::MAX),
            ..self.report.clone()
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
}

impl Analysis {
    pub fn report(&self) -> Report {
        self.agg.lock().unwrap().report()
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn is_unknown_command(e: &redis::RedisError) -> bool {
    let s = e.to_string().to_ascii_lowercase();
    s.contains("unknown command") || s.contains("unknown subcommand") || s.contains("noperm") || s.contains("disabled")
}

/// 开始扫描:先探测实例能力(DBSIZE、MEMORY USAGE、淘汰策略),再在后台逐批扫描。
pub async fn start(mut con: MultiplexedConnection) -> AppResult<Arc<Analysis>> {
    let total: u64 = redis::cmd("DBSIZE").query_async(&mut con).await.map_err(qerr)?;
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
    let analysis = Arc::new(Analysis {
        agg: Mutex::new(Aggregator::new(total, memory_supported, lfu)),
        cancel: AtomicBool::new(false),
    });
    let a = analysis.clone();
    tokio::spawn(async move {
        let (cancelled, truncated, error) = match scan_all(&mut con, &a, memory_supported, lfu).await {
            Ok((cancelled, truncated)) => (cancelled, truncated, None),
            Err(e) => (false, false, Some(e.display())),
        };
        a.agg.lock().unwrap().finish(cancelled, truncated, error);
    });
    Ok(analysis)
}

/// 逐批 SCAN,每批用一个管道取各 key 的类型、剩余时间、内存与访问频度。返回 (取消, 截断)。
async fn scan_all(
    con: &mut MultiplexedConnection,
    a: &Analysis,
    memory: bool,
    lfu: bool,
) -> AppResult<(bool, bool)> {
    let mut cursor: u64 = 0;
    let mut seen: u64 = 0;
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
                });
            }
            seen += keys.len() as u64;
        }
        cursor = next;
        if cursor == 0 {
            return Ok((false, false));
        }
        if seen >= MAX_KEYS {
            return Ok((false, true));
        }
        tokio::time::sleep(PAUSE).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(key: &str, kind: &str, bytes: u64, ttl_ms: i64, freq: Option<u64>) -> KeyStat {
        KeyStat { key: key.into(), kind: kind.into(), bytes: Some(bytes), ttl_ms, freq }
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
    fn prefix_rules() {
        assert_eq!(prefix_of("user:1:profile"), "user");
        assert_eq!(prefix_of("plain"), "");
        assert_eq!(prefix_of(":odd"), "");
    }
}
