//! Redis Lua 脚本:执行前的静态检查(写命令 / 危险命令识别、KEYS 的集群槽)、EVAL 与 SCRIPT KILL。
//!
//! - **静态检查**只看 `redis.call` / `redis.pcall`(7.0 起的别名 `server.call` / `server.pcall`)的第一个参数:
//!   是字符串字面量就按命令清单归类(大小写不敏感);不是字面量(变量、拼接、`unpack(ARGV)`)、
//!   把 `redis.call` 赋给别的名字、用 `loadstring` 等动态加载代码时,一律当作「可能写入」。
//!   注释里的调用不算;字符串里的不算(要执行字符串里的代码只能经 `loadstring` 之类,已按动态处理)。
//!   不在只读清单、也不在写清单里的命令(模块命令等)同样当作可能写入。
//! - **专用连接执行**:脚本运行期间,发出 EVAL 的那条连接上后续的命令都要排在 EVAL 之后,
//!   共享的多路复用连接上发 SCRIPT KILL 会被它自己等着的 EVAL 挡住(死等)。所以 EVAL 自己开一条专用连接
//!   (与实时会话同一套建连参数,含 SSH 隧道、认证、TLS,并 SELECT 连接设置里的库),SCRIPT KILL 再另开一条。
//!   服务器在脚本跑满 busy-reply-threshold(默认 5 秒)之前不处理任何其他客户端,之后才接受 SCRIPT KILL;
//!   只有没执行过写命令的脚本能被 KILL,写过的服务器回 UNKILLABLE(只能 SHUTDOWN NOSAVE,这里不提供)。
//! - **结果有界**:脚本的返回值由脚本决定,可以构造得极大、嵌套极深。回复用 live 的有界 RESP 读取器读,
//!   单个字符串、每层元素数、整帧节点数、整帧字节数、嵌套层数都有上限,超出部分从网络上读走丢掉、记下丢了多少。

use super::live::{self, Frame, FrameLimits, LiveParams};
use crate::error::{AppError, AppResult, ErrorKind};
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

// ---------------------------------------------------------------- 静态检查

/// 会改数据(或有副作用,如 PUBLISH)的命令。
const WRITE_COMMANDS: &[&str] = &[
    // string / 通用
    "SET", "SETNX", "SETEX", "PSETEX", "MSET", "MSETNX", "GETSET", "GETDEL", "GETEX", "APPEND", "SETRANGE",
    "INCR", "INCRBY", "INCRBYFLOAT", "DECR", "DECRBY", "SETBIT", "BITOP", "BITFIELD",
    "DEL", "UNLINK", "EXPIRE", "EXPIREAT", "PEXPIRE", "PEXPIREAT", "PERSIST", "RENAME", "RENAMENX", "MOVE",
    "COPY", "RESTORE", "MIGRATE", "SORT", "FLUSHALL", "FLUSHDB", "SWAPDB",
    // hash
    "HSET", "HSETNX", "HMSET", "HDEL", "HINCRBY", "HINCRBYFLOAT", "HEXPIRE", "HPEXPIRE", "HEXPIREAT", "HPEXPIREAT",
    "HPERSIST", "HGETDEL", "HGETEX", "HSETEX",
    // list
    "LPUSH", "LPUSHX", "RPUSH", "RPUSHX", "LPOP", "RPOP", "LSET", "LREM", "LTRIM", "LINSERT", "RPOPLPUSH", "LMOVE",
    "LMPOP", "BLPOP", "BRPOP", "BRPOPLPUSH", "BLMOVE", "BLMPOP",
    // set
    "SADD", "SREM", "SPOP", "SMOVE", "SINTERSTORE", "SUNIONSTORE", "SDIFFSTORE",
    // zset
    "ZADD", "ZINCRBY", "ZREM", "ZREMRANGEBYSCORE", "ZREMRANGEBYRANK", "ZREMRANGEBYLEX", "ZPOPMIN", "ZPOPMAX",
    "BZPOPMIN", "BZPOPMAX", "ZMPOP", "BZMPOP", "ZUNIONSTORE", "ZINTERSTORE", "ZDIFFSTORE", "ZRANGESTORE",
    // stream
    "XADD", "XDEL", "XTRIM", "XGROUP", "XACK", "XCLAIM", "XAUTOCLAIM", "XSETID", "XREADGROUP",
    // 其他
    "PFADD", "PFMERGE", "GEOADD", "GEORADIUS", "GEORADIUSBYMEMBER", "GEOSEARCHSTORE", "PUBLISH", "SPUBLISH",
];

/// 只读命令(在脚本里调用不改数据)。不在这里也不在写清单里的按「可能写入」处理。
const READ_COMMANDS: &[&str] = &[
    "GET", "MGET", "STRLEN", "GETRANGE", "SUBSTR", "GETBIT", "BITCOUNT", "BITPOS", "BITFIELD_RO", "LCS",
    "EXISTS", "TYPE", "TTL", "PTTL", "EXPIRETIME", "PEXPIRETIME", "KEYS", "SCAN", "RANDOMKEY", "DBSIZE", "DUMP",
    "OBJECT", "MEMORY", "TOUCH", "SORT_RO", "TIME", "PING", "ECHO", "INFO",
    "HGET", "HMGET", "HGETALL", "HKEYS", "HVALS", "HLEN", "HEXISTS", "HSTRLEN", "HSCAN", "HRANDFIELD", "HTTL",
    "HPTTL", "HEXPIRETIME", "HPEXPIRETIME",
    "LRANGE", "LLEN", "LINDEX", "LPOS",
    "SMEMBERS", "SISMEMBER", "SMISMEMBER", "SCARD", "SRANDMEMBER", "SINTER", "SUNION", "SDIFF", "SINTERCARD", "SSCAN",
    "ZRANGE", "ZRANGEBYSCORE", "ZRANGEBYLEX", "ZREVRANGE", "ZREVRANGEBYSCORE", "ZREVRANGEBYLEX", "ZSCORE", "ZMSCORE",
    "ZRANK", "ZREVRANK", "ZCARD", "ZCOUNT", "ZLEXCOUNT", "ZSCAN", "ZUNION", "ZINTER", "ZDIFF", "ZINTERCARD",
    "ZRANDMEMBER",
    "XRANGE", "XREVRANGE", "XLEN", "XREAD", "XINFO", "XPENDING",
    "PFCOUNT", "GEOPOS", "GEODIST", "GEOHASH", "GEOSEARCH", "GEORADIUS_RO", "GEORADIUSBYMEMBER_RO",
];

/// 要特别标出的危险命令:清库、换库,以及 KEYS(不管模式是什么都要遍历整个库,大库上会长时间阻塞实例)。
const DANGEROUS_COMMANDS: &[&str] = &["FLUSHALL", "FLUSHDB", "SWAPDB", "KEYS"];

/// 出现即无法静态判断会执行什么的标识符:动态加载代码、绕开 `redis.call` 名字的取法。
const DYNAMIC_IDENTS: &[&str] = &["loadstring", "load", "dofile", "loadfile", "getfenv", "setfenv", "rawget", "_G"];

/// 脚本的静态检查结果。命令名一律大写,按第一次出现的顺序去重。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    /// 字面量调用到的全部命令
    pub commands: Vec<String>,
    /// 其中的写命令
    pub writes: Vec<String>,
    /// 其中的危险命令(FLUSHALL / FLUSHDB / SWAPDB / KEYS)
    pub dangerous: Vec<String>,
    /// 其中既不在只读清单也不在写清单里的(按可能写入处理)
    pub unknown: Vec<String>,
    /// 有无法静态判断的调用(命令名不是字面量、`redis.call` 被转存、动态加载代码)
    pub dynamic: bool,
    /// 可能写入:有写命令、未知命令或动态调用
    pub may_write: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Punct(u8),
}

/// `[` 之后若是长括号(`[[`、`[==[`)返回等号个数与开头的总长度。
fn long_bracket(b: &[u8], i: usize) -> Option<(usize, usize)> {
    if b.get(i) != Some(&b'[') {
        return None;
    }
    let mut j = i + 1;
    while b.get(j) == Some(&b'=') {
        j += 1;
    }
    (b.get(j) == Some(&b'[')).then_some((j - i - 1, j + 1 - i))
}

/// 从 `from` 起找长括号的结尾 `]==]`,返回内容结束处与结尾之后的位置;没闭合时到文本末尾。
fn long_end(b: &[u8], from: usize, level: usize) -> (usize, usize) {
    let mut k = from;
    while k < b.len() {
        if b[k] == b']' {
            let mut j = k + 1;
            while j < b.len() && b[j] == b'=' && j - k - 1 < level {
                j += 1;
            }
            if j - k - 1 == level && b.get(j) == Some(&b']') {
                return (k, j + 1);
            }
        }
        k += 1;
    }
    (b.len(), b.len())
}

/// 解出短字符串里常见的转义(命令名只会是 ASCII,够用)。
fn unescape(raw: &[u8]) -> String {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] != b'\\' || i + 1 >= raw.len() {
            out.push(raw[i]);
            i += 1;
            continue;
        }
        let c = raw[i + 1];
        i += 2;
        match c {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'x' => {
                let hex = raw.get(i..i + 2).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
                if let Some(v) = hex {
                    out.push(v);
                    i += 2;
                }
            }
            b'0'..=b'9' => {
                let mut v: u32 = (c - b'0') as u32;
                let mut n = 1;
                while n < 3 && i < raw.len() && raw[i].is_ascii_digit() {
                    v = v * 10 + (raw[i] - b'0') as u32;
                    i += 1;
                    n += 1;
                }
                out.push(v.min(255) as u8);
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 粗粒度的 Lua 词法:只区分标识符、字符串、标点;跳过注释与数字。
fn tokens(src: &str) -> Vec<Tok> {
    let b = src.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'-' && b.get(i + 1) == Some(&b'-') {
            // 注释:长注释 --[[ ]] / --[==[ ]==],否则到行尾
            if let Some((level, open)) = long_bracket(b, i + 2) {
                i = long_end(b, i + 2 + open, level).1;
            } else {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
        } else if c == b'\'' || c == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < b.len() && b[j] != c && b[j] != b'\n' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            let end = j.min(b.len());
            out.push(Tok::Str(unescape(&b[start..end])));
            i = (j + 1).min(b.len()).max(end);
        } else if let Some((level, open)) = long_bracket(b, i) {
            let (end, next) = long_end(b, i + open, level);
            out.push(Tok::Str(String::from_utf8_lossy(&b[i + open..end]).into_owned()));
            i = next;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(Tok::Ident(src[start..i].to_string()));
        } else if c.is_ascii_digit() {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                i += 1;
            }
        } else {
            out.push(Tok::Punct(c));
            i += 1;
        }
    }
    out
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

/// 静态检查脚本里调用的命令。
pub fn analyze(script: &str) -> Analysis {
    let toks = tokens(script);
    let mut a = Analysis::default();
    let ident = |i: usize| match toks.get(i) {
        Some(Tok::Ident(s)) => Some(s.as_str()),
        _ => None,
    };
    let punct = |i: usize, p: u8| toks.get(i) == Some(&Tok::Punct(p));
    for i in 0..toks.len() {
        let Some(name) = ident(i) else { continue };
        // 字段访问(`t.load`、`obj:redis`)不是全局名字
        let field = i > 0 && (punct(i - 1, b'.') || punct(i - 1, b':'));
        if field {
            continue;
        }
        if DYNAMIC_IDENTS.contains(&name) {
            a.dynamic = true;
            continue;
        }
        if name != "redis" && name != "server" {
            continue;
        }
        if punct(i + 1, b'[') {
            // redis['call'] / redis[x]:按动态处理
            a.dynamic = true;
            continue;
        }
        if !punct(i + 1, b'.') || !matches!(ident(i + 2), Some("call" | "pcall")) {
            continue;
        }
        // redis.call('SET', …) 或 redis.call 'GET'(Lua 的单字符串实参调用)
        let lit = match (toks.get(i + 3), toks.get(i + 4)) {
            (Some(Tok::Punct(b'(')), Some(Tok::Str(s))) if matches!(toks.get(i + 5), Some(Tok::Punct(b',' | b')'))) => Some(s),
            (Some(Tok::Str(s)), _) => Some(s),
            _ => None,
        };
        let Some(cmd) = lit else {
            a.dynamic = true;
            continue;
        };
        let cmd = cmd.trim().to_ascii_uppercase();
        if cmd.is_empty() {
            a.dynamic = true;
            continue;
        }
        push_unique(&mut a.commands, &cmd);
        if WRITE_COMMANDS.contains(&cmd.as_str()) {
            push_unique(&mut a.writes, &cmd);
        } else if !READ_COMMANDS.contains(&cmd.as_str()) {
            push_unique(&mut a.unknown, &cmd);
        }
        if DANGEROUS_COMMANDS.contains(&cmd.as_str()) {
            push_unique(&mut a.dangerous, &cmd);
        }
    }
    a.may_write = a.dynamic || !a.writes.is_empty() || !a.unknown.is_empty();
    a
}

// ---------------------------------------------------------------- 集群槽

/// CRC16-CCITT(XMODEM):Redis Cluster 计算槽用的那个。
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// key 所在的槽(0..16384)。有非空的 hash tag(第一个 `{` 与其后第一个 `}` 之间)时只算 tag。
pub fn key_slot(key: &[u8]) -> u16 {
    let part = match key.iter().position(|&c| c == b'{') {
        Some(open) => match key[open + 1..].iter().position(|&c| c == b'}') {
            Some(len) if len > 0 => &key[open + 1..open + 1 + len],
            _ => key,
        },
        None => key,
    };
    crc16(part) % 16384
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeySlot {
    pub key: String,
    pub slot: u16,
}

/// KEYS 各自的槽与是否同槽(没有或只有一个 key 时视为同槽)。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotCheck {
    pub slots: Vec<KeySlot>,
    pub same: bool,
}

pub fn check_slots(keys: &[String]) -> SlotCheck {
    let slots: Vec<KeySlot> = keys.iter().map(|k| KeySlot { key: k.clone(), slot: key_slot(k.as_bytes()) }).collect();
    let same = slots.windows(2).all(|w| w[0].slot == w[1].slot);
    SlotCheck { slots, same }
}

// ---------------------------------------------------------------- 结果

/// 单个字符串最多保留的字节数(查看时还要做格式识别,与 key 查看的字符串上限一致)。
pub const MAX_STRING_BYTES: usize = 1 << 20;
/// 整个结果最多保留的字节数(所有字符串、错误文本合计)。
pub const MAX_RESULT_BYTES: usize = 4 << 20;
/// 每个数组最多保留的元素数。
pub const MAX_ITEMS: usize = 1000;
/// 整个结果最多保留的节点数(所有层合计)。
pub const MAX_NODES: usize = 10_000;
/// 保留的嵌套层数;更深的数组只记元素个数。
pub const MAX_DEPTH: usize = 16;

/// 读 EVAL 回复的上限:每个顶层帧共享字节预算与节点预算(只限单个值不够,嵌套时每层保留量会相乘)。
pub const RESULT_LIMITS: FrameLimits = FrameLimits {
    cap: MAX_STRING_BYTES,
    slack: MAX_RESULT_BYTES - MAX_STRING_BYTES,
    kept: MAX_ITEMS,
    nodes: MAX_NODES,
    depth: MAX_DEPTH,
    skip_deep: true,
};

/// 脚本返回值(RESP2:Lua 的 table 是数组、true 是 1、false / nil 是 nil、{ok=…} 是状态、{err=…} 是错误)。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LuaValue {
    Nil,
    /// 整数用文本:超出 2^53 的值在 JS 里会丢精度
    Int { text: String },
    Status { text: String },
    Error { text: String },
    /// `text` 为保留下来的部分(不是合法 UTF-8 时为替换过的文本,原始字节在 `b64`);`len` 为原长
    Str {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        b64: Option<String>,
        stored: usize,
        len: usize,
    },
    /// `len` 为原有元素个数,比 `items` 多出来的是没保留的
    Array { items: Vec<LuaValue>, len: usize },
}

/// 字节 → 文本。截断处切开的不完整 UTF-8 字符只丢掉尾巴,不算二进制。
fn bytes_value(data: Vec<u8>, len: usize) -> LuaValue {
    use base64::Engine;
    let stored = data.len();
    match String::from_utf8(data) {
        Ok(text) => LuaValue::Str { text, b64: None, stored, len },
        Err(e) => {
            let valid = e.utf8_error().valid_up_to();
            let bytes = e.into_bytes();
            if stored < len && std::str::from_utf8(&bytes[valid..]).is_err() && bytes.len() - valid < 4 {
                // 只是尾巴上半个字符
                let text = String::from_utf8_lossy(&bytes[..valid]).into_owned();
                return LuaValue::Str { text, b64: None, stored, len };
            }
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            LuaValue::Str { text: String::from_utf8_lossy(&bytes).into_owned(), b64: Some(b64), stored, len }
        }
    }
}

/// 有界帧 → 返回值;`truncated` 记下有没有被截掉的部分。
pub fn to_value(f: Frame, truncated: &mut bool) -> LuaValue {
    match f {
        Frame::Simple(b) => {
            *truncated |= b.data.len() < b.len;
            LuaValue::Status { text: String::from_utf8_lossy(&b.data).into_owned() }
        }
        Frame::Error(e) => LuaValue::Error { text: e },
        Frame::Int(n) => LuaValue::Int { text: n.to_string() },
        Frame::Bulk(None) | Frame::Array(None) => LuaValue::Nil,
        Frame::Bulk(Some(b)) => {
            *truncated |= b.data.len() < b.len;
            bytes_value(b.data, b.len)
        }
        Frame::Array(Some(items)) => {
            let mut out = Vec::with_capacity(items.len());
            let mut len = 0;
            for it in items {
                match it {
                    Frame::Skipped(n) => {
                        *truncated = true;
                        len += n;
                    }
                    other => {
                        len += 1;
                        out.push(to_value(other, truncated));
                    }
                }
            }
            LuaValue::Array { items: out, len }
        }
        // 只会作为数组元素出现,上面已处理
        Frame::Skipped(n) => {
            *truncated = true;
            LuaValue::Array { items: vec![], len: n }
        }
    }
}

/// 顶层错误回复的分类,界面据此给出说明(托管 Redis 常禁用 EVAL、ACL 没有权限等)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorHint {
    /// 服务器不认 EVAL(被 rename-command 禁用、托管服务不支持)
    Disabled,
    /// ACL 没有执行权限
    NoPerm,
    /// NOSCRIPT(脚本缓存里没有;个别代理对 EVAL 也这样回)
    NoScript,
    /// 编译错误(语法)
    Compile,
    /// 运行时错误
    Runtime,
    /// 被 SCRIPT KILL 终止
    Killed,
    /// 集群:KEYS 不在同一槽
    CrossSlot,
    /// 集群:key 不在所连节点(MOVED / ASK)
    Moved,
    /// 连的是只读副本
    ReadOnly,
    /// 有别的脚本正在运行
    Busy,
    /// 内存不足,拒绝写入
    Oom,
    /// 脚本里调用了不允许在脚本中执行的命令
    NotAllowed,
}

pub fn classify_error(msg: &str) -> Option<ErrorHint> {
    let m = msg.to_ascii_uppercase();
    let starts = |p: &str| m.starts_with(p);
    Some(if starts("NOSCRIPT") {
        ErrorHint::NoScript
    } else if starts("NOPERM") || m.contains("NO PERMISSIONS") {
        ErrorHint::NoPerm
    } else if m.contains("UNKNOWN COMMAND") || m.contains("COMMAND NOT ALLOWED") || m.contains("COMMAND IS DISABLED")
        || m.contains("NOT SUPPORTED")
    {
        ErrorHint::Disabled
    } else if m.contains("NOT ALLOWED FROM SCRIPT") {
        ErrorHint::NotAllowed
    } else if m.contains("ERROR COMPILING SCRIPT") {
        ErrorHint::Compile
    } else if m.contains("KILLED BY USER") {
        ErrorHint::Killed
    } else if starts("CROSSSLOT") {
        ErrorHint::CrossSlot
    } else if starts("MOVED") || starts("ASK ") {
        ErrorHint::Moved
    } else if starts("READONLY") {
        ErrorHint::ReadOnly
    } else if starts("BUSY") {
        ErrorHint::Busy
    } else if starts("OOM") {
        ErrorHint::Oom
    } else if m.contains("ERROR RUNNING SCRIPT") || m.contains("USER_SCRIPT:") || m.contains("@USER_SCRIPT") {
        ErrorHint::Runtime
    } else {
        return None;
    })
}

/// 一次执行:脚本、KEYS、ARGV。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalRequest {
    pub script: String,
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalOutcome {
    pub value: LuaValue,
    /// 顶层是错误回复时的分类(认不出为 None)
    pub hint: Option<ErrorHint>,
    pub elapsed_ms: u64,
    /// 结果里有被截掉的部分(超长字符串、超多元素、过深嵌套)
    pub truncated: bool,
    /// 在哪个库上执行的
    pub db: i64,
}

/// 脚本源码的长度上限(编辑器里的脚本,防止误粘贴的大段文本整个发给服务器)。
pub const MAX_SCRIPT_BYTES: usize = 1 << 20;
/// KEYS + ARGV 的总个数上限。
pub const MAX_PARAMS: usize = 1000;

/// EVAL 的参数:`EVAL script numkeys key… arg…`,逐个作为 bulk 发出(二进制安全,不拼字符串)。
pub fn eval_args(req: &EvalRequest) -> Vec<Vec<u8>> {
    let mut args: Vec<Vec<u8>> = Vec::with_capacity(3 + req.keys.len() + req.args.len());
    args.push(b"EVAL".to_vec());
    args.push(req.script.as_bytes().to_vec());
    args.push(req.keys.len().to_string().into_bytes());
    args.extend(req.keys.iter().map(|k| k.as_bytes().to_vec()));
    args.extend(req.args.iter().map(|a| a.as_bytes().to_vec()));
    args
}

fn qfail(detail: String) -> AppError {
    AppError::new(ErrorKind::Query, t!("core.err.redis_command_failed")).with_detail(detail)
}

/// 专用连接:建连、认证、SELECT 连接设置里的库。
async fn open(p: &LiveParams, name: &str) -> AppResult<(live::Reader, live::Writer)> {
    let (mut rd, mut w) = live::open_with(p, name, RESULT_LIMITS).await?;
    if p.db != 0 {
        let db = p.db.to_string();
        live::send(&mut w, &[b"SELECT", db.as_bytes()]).await.map_err(|e| qfail(e.to_string()))?;
        if let Frame::Error(e) = rd.read().await.map_err(|e| qfail(e.to_string()))? {
            return Err(qfail(e));
        }
    }
    Ok((rd, w))
}

/// 在专用连接上执行一次 EVAL,等到脚本结束(没有时限:长脚本由用户决定 SCRIPT KILL 或放弃等待)。
pub async fn eval(p: &LiveParams, req: &EvalRequest) -> AppResult<EvalOutcome> {
    if req.script.trim().is_empty() {
        return Err(AppError::new(ErrorKind::Query, t!("core.err.redis_eval_empty")));
    }
    if req.script.len() > MAX_SCRIPT_BYTES {
        return Err(AppError::new(ErrorKind::Query, t!("core.err.redis_eval_too_long", max = MAX_SCRIPT_BYTES >> 10)));
    }
    if req.keys.len() + req.args.len() > MAX_PARAMS {
        return Err(AppError::new(ErrorKind::Query, t!("core.err.redis_eval_too_many_params", max = MAX_PARAMS)));
    }
    let name = format!("datum-eval-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let (mut rd, mut w) = open(p, &name).await?;
    let args = eval_args(req);
    let refs: Vec<&[u8]> = args.iter().map(|a| a.as_slice()).collect();
    let started = Instant::now();
    live::send(&mut w, &refs).await.map_err(|e| qfail(e.to_string()))?;
    let frame = rd.read().await.map_err(|e| qfail(e.to_string()))?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let _ = tokio::time::timeout(Duration::from_secs(1), w.shutdown()).await;
    let hint = match &frame {
        Frame::Error(e) => classify_error(e),
        _ => None,
    };
    let mut truncated = false;
    let value = to_value(frame, &mut truncated);
    Ok(EvalOutcome { value, hint, elapsed_ms, truncated, db: p.db })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum KillStatus {
    /// 已终止
    Killed,
    /// 服务器上没有正在运行的脚本(已经结束了)
    NotBusy,
    /// 脚本已经写过数据,不能 KILL(只能 SHUTDOWN NOSAVE)
    Unkillable,
    /// 其他错误(权限、命令被禁用…),见 `message`
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KillResult {
    pub status: KillStatus,
    pub message: String,
}

/// SCRIPT KILL 的回复最多等多久:服务器要在脚本跑满 busy-reply-threshold(默认 5 秒)后才处理别的客户端。
const KILL_TIMEOUT: Duration = Duration::from_secs(15);

pub fn kill_status(reply: &Frame) -> KillResult {
    match reply {
        Frame::Error(e) => {
            let up = e.to_ascii_uppercase();
            let status = if up.starts_with("NOTBUSY") {
                KillStatus::NotBusy
            } else if up.starts_with("UNKILLABLE") {
                KillStatus::Unkillable
            } else {
                KillStatus::Failed
            };
            KillResult { status, message: e.clone() }
        }
        Frame::Simple(b) => KillResult { status: KillStatus::Killed, message: String::from_utf8_lossy(&b.data).into_owned() },
        other => KillResult { status: KillStatus::Failed, message: format!("{other:?}") },
    }
}

/// 另开一条专用连接发 SCRIPT KILL(终止的是服务器上当前正在运行的脚本)。
pub async fn script_kill(p: &LiveParams) -> AppResult<KillResult> {
    let fut = async {
        let (mut rd, mut w) = live::open_with(p, "datum-script-kill", RESULT_LIMITS).await?;
        live::send(&mut w, &[b"SCRIPT", b"KILL"]).await.map_err(|e| qfail(e.to_string()))?;
        let reply = rd.read().await.map_err(|e| qfail(e.to_string()))?;
        let _ = tokio::time::timeout(Duration::from_secs(1), w.shutdown()).await;
        Ok(kill_status(&reply))
    };
    match tokio::time::timeout(KILL_TIMEOUT, fut).await {
        Ok(r) => r,
        Err(_) => Err(qfail(t!("core.err.connect_timeout").to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_write_commands_case_insensitively() {
        let a = analyze("local v = redis.call('get', KEYS[1])\nredis.call(\"Set\", KEYS[1], ARGV[1])\nreturn redis.pcall('HSET', 'h', 'f', v)");
        assert_eq!(a.commands, vec!["GET", "SET", "HSET"]);
        assert_eq!(a.writes, vec!["SET", "HSET"]);
        assert!(a.dangerous.is_empty() && a.unknown.is_empty() && !a.dynamic);
        assert!(a.may_write);
    }

    #[test]
    fn read_only_script_is_not_a_write() {
        let a = analyze("return redis.call('GET', KEYS[1])");
        assert_eq!(a.commands, vec!["GET"]);
        assert!(!a.may_write);
        // server.call 是 7.0 的别名;单字符串实参的调用写法
        let a = analyze("return server.call 'DBSIZE'");
        assert_eq!(a.commands, vec!["DBSIZE"]);
        assert!(!a.may_write);
    }

    #[test]
    fn comments_and_strings_do_not_count() {
        let a = analyze("-- redis.call('FLUSHALL')\n--[[ redis.call('DEL', 'x') ]]\n--[==[ redis.call('SET') ]] ]==]\nlocal s = \"redis.call('DEL')\"\nlocal l = [[redis.call('SET')]]\nreturn redis.call('PING')");
        assert_eq!(a.commands, vec!["PING"]);
        assert!(!a.may_write, "{a:?}");
    }

    #[test]
    fn dynamic_command_names_may_write() {
        for src in [
            "return redis.call(ARGV[1], KEYS[1])",
            "return redis.call(unpack(ARGV))",
            "local c = redis.call\nreturn c('GET', 'x')",
            "return redis['call']('GET', 'x')",
            "return loadstring(ARGV[1])()",
            "return redis.call('GE' .. 'T', 'x')",
            "return _G.redis.call('GET', 'x')",
        ] {
            let a = analyze(src);
            assert!(a.dynamic && a.may_write, "{src}: {a:?}");
        }
    }

    #[test]
    fn unknown_commands_may_write() {
        let a = analyze("return redis.call('JSON.SET', KEYS[1], '$', ARGV[1])");
        assert_eq!(a.unknown, vec!["JSON.SET"]);
        assert!(a.writes.is_empty() && a.may_write);
    }

    #[test]
    fn flags_dangerous_commands() {
        let a = analyze("redis.call('flushall')\nredis.call('FlushDB', 'ASYNC')\nfor _, k in ipairs(redis.call('keys', '*')) do end");
        assert_eq!(a.dangerous, vec!["FLUSHALL", "FLUSHDB", "KEYS"]);
        // KEYS 本身只读,但仍要标出来
        assert_eq!(a.writes, vec!["FLUSHALL", "FLUSHDB"]);
        assert!(!a.unknown.contains(&"KEYS".to_string()));
    }

    #[test]
    fn field_named_load_is_not_dynamic() {
        let a = analyze("local t = cjson.decode(ARGV[1])\nlocal x = t.load\nreturn redis.call('GET', x)");
        assert!(!a.dynamic, "{a:?}");
    }

    #[test]
    fn crc16_and_slots_match_known_vectors() {
        // Redis Cluster 规范附带的测试向量
        assert_eq!(crc16(b"123456789"), 0x31C3);
        assert_eq!(key_slot(b"foo"), 12182);
        assert_eq!(key_slot(b"bar"), 5061);
        assert_eq!(key_slot(b"hello"), 866);
        assert_eq!(key_slot(b""), 0);
        // hash tag:只算 {} 里的部分;空 tag 时算整个 key
        assert_eq!(key_slot(b"{user1000}.following"), key_slot(b"{user1000}.followers"));
        assert_eq!(key_slot(b"{user1000}.following"), key_slot(b"user1000"));
        assert_eq!(key_slot(b"foo{}{bar}"), key_slot(b"foo{}{bar}"));
        assert_ne!(key_slot(b"foo{}{bar}"), key_slot(b"bar"));
        assert_eq!(key_slot(b"foo{{bar}}zap"), key_slot(b"{bar"));
        assert_eq!(key_slot(b"foo{bar}{zap}"), key_slot(b"bar"));
    }

    #[test]
    fn checks_keys_share_a_slot() {
        let keys = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(check_slots(&keys(&[])).same);
        assert!(check_slots(&keys(&["a"])).same);
        assert!(check_slots(&keys(&["{u1}:a", "{u1}:b"])).same);
        let c = check_slots(&keys(&["foo", "bar"]));
        assert!(!c.same);
        assert_eq!(c.slots[0], KeySlot { key: "foo".into(), slot: 12182 });
    }

    #[test]
    fn eval_args_are_separate_bulks() {
        let req = EvalRequest { script: "return 1".into(), keys: vec!["k 1".into()], args: vec!["a b".into(), "".into()] };
        let args = eval_args(&req);
        assert_eq!(args, vec![b"EVAL".to_vec(), b"return 1".to_vec(), b"1".to_vec(), b"k 1".to_vec(), b"a b".to_vec(), b"".to_vec()]);
    }

    #[test]
    fn classifies_server_errors() {
        assert_eq!(classify_error("ERR unknown command 'EVAL', with args beginning with: 'return 1' '0'"), Some(ErrorHint::Disabled));
        assert_eq!(classify_error("NOPERM User default has no permissions to run the 'eval' command"), Some(ErrorHint::NoPerm));
        assert_eq!(classify_error("NOSCRIPT No matching script."), Some(ErrorHint::NoScript));
        assert_eq!(classify_error("ERR Error compiling script (new function): user_script:1: '=' expected near 'x'"), Some(ErrorHint::Compile));
        assert_eq!(classify_error("ERR user_script:1: Script attempted to access nonexistent global variable 'x' script: 1a2b"), Some(ErrorHint::Runtime));
        assert_eq!(classify_error("ERR Script killed by user with SCRIPT KILL..."), Some(ErrorHint::Killed));
        assert_eq!(classify_error("CROSSSLOT Keys in request don't hash to the same slot"), Some(ErrorHint::CrossSlot));
        assert_eq!(classify_error("MOVED 12182 127.0.0.1:7001"), Some(ErrorHint::Moved));
        assert_eq!(classify_error("READONLY You can't write against a read only replica."), Some(ErrorHint::ReadOnly));
        assert_eq!(classify_error("BUSY Redis is busy running a script."), Some(ErrorHint::Busy));
        assert_eq!(classify_error("ERR This Redis command is not allowed from script"), Some(ErrorHint::NotAllowed));
        assert_eq!(classify_error("ERR something else"), None);
    }

    #[test]
    fn kill_replies() {
        assert_eq!(kill_status(&Frame::Simple(live::Bytes { data: b"OK".to_vec(), len: 2 })).status, KillStatus::Killed);
        assert_eq!(kill_status(&Frame::Error("NOTBUSY No scripts in execution right now.".into())).status, KillStatus::NotBusy);
        assert_eq!(
            kill_status(&Frame::Error("UNKILLABLE Sorry the script already executed write commands against the dataset.".into())).status,
            KillStatus::Unkillable
        );
        assert_eq!(kill_status(&Frame::Error("NOPERM no".into())).status, KillStatus::Failed);
    }

    async fn read_value(input: &[u8]) -> (LuaValue, bool) {
        let mut rd = live::FrameReader::with_limits(input, RESULT_LIMITS);
        let f = rd.read().await.unwrap();
        let mut truncated = false;
        let v = to_value(f, &mut truncated);
        (v, truncated)
    }

    #[tokio::test]
    async fn converts_resp_types() {
        let (v, tr) = read_value(b"*6\r\n:42\r\n$5\r\nhello\r\n$-1\r\n+OK\r\n-ERR inner\r\n*1\r\n*0\r\n").await;
        assert!(!tr);
        assert_eq!(
            v,
            LuaValue::Array {
                items: vec![
                    LuaValue::Int { text: "42".into() },
                    LuaValue::Str { text: "hello".into(), b64: None, stored: 5, len: 5 },
                    LuaValue::Nil,
                    LuaValue::Status { text: "OK".into() },
                    LuaValue::Error { text: "ERR inner".into() },
                    LuaValue::Array { items: vec![LuaValue::Array { items: vec![], len: 0 }], len: 1 },
                ],
                len: 6,
            }
        );
        // 非 UTF-8 带原始字节
        let (v, _) = read_value(b"$2\r\n\xff\x00\r\n").await;
        let LuaValue::Str { b64, .. } = v else { panic!() };
        assert_eq!(b64.as_deref(), Some("/wA="));
        // 大整数不丢精度
        let (v, _) = read_value(b":9007199254740993\r\n").await;
        assert_eq!(v, LuaValue::Int { text: "9007199254740993".into() });
    }

    fn count(v: &LuaValue) -> (usize, usize) {
        // (节点数, 保留的字节数)
        match v {
            LuaValue::Array { items, .. } => items.iter().map(count).fold((1, 0), |a, b| (a.0 + b.0, a.1 + b.1)),
            LuaValue::Str { stored, .. } => (1, *stored),
            LuaValue::Error { text } | LuaValue::Status { text } => (1, text.len()),
            _ => (1, 0),
        }
    }

    #[tokio::test]
    async fn caps_items_per_array_and_keeps_stream_in_sync() {
        let mut input = format!("*{}\r\n", MAX_ITEMS + 500).into_bytes();
        for i in 0..MAX_ITEMS + 500 {
            input.extend(format!(":{i}\r\n").as_bytes());
        }
        input.extend(b":7\r\n");
        let mut rd = live::FrameReader::with_limits(&input[..], RESULT_LIMITS);
        let mut tr = false;
        let v = to_value(rd.read().await.unwrap(), &mut tr);
        let LuaValue::Array { items, len } = v else { panic!() };
        assert!(tr);
        assert_eq!((items.len(), len), (MAX_ITEMS, MAX_ITEMS + 500));
        assert_eq!(rd.read().await.unwrap(), Frame::Int(7), "后续帧要照常对齐");
    }

    #[tokio::test]
    async fn nested_results_share_one_budget() {
        // 3 层、每层 1000 个元素、每个都是 1 KB 的字符串:不设整体预算时要保留 10⁹ 个节点、1 TB
        // 这里只造前面一部分(读到预算用完后其余读走丢掉),验证保留量有整体上界
        let s = format!("$1024\r\n{}\r\n", "x".repeat(1024));
        let lvl1 = format!("*1000\r\n{}", s.repeat(1000));
        let lvl2 = format!("*20\r\n{}", lvl1.repeat(20));
        let mut input = format!("*2\r\n{}", lvl2.repeat(2)).into_bytes();
        input.extend(b":7\r\n");
        let mut rd = live::FrameReader::with_limits(&input[..], RESULT_LIMITS);
        let mut tr = false;
        let v = to_value(rd.read().await.unwrap(), &mut tr);
        let (nodes, bytes) = count(&v);
        assert!(tr);
        assert!(nodes <= MAX_NODES + 1, "nodes {nodes}");
        assert!(bytes <= MAX_RESULT_BYTES, "bytes {bytes}");
        assert_eq!(rd.read().await.unwrap(), Frame::Int(7));
    }

    #[tokio::test]
    async fn many_errors_count_against_the_budget() {
        let e = format!("-ERR {}\r\n", "e".repeat(4000));
        let mut input = format!("*{}\r\n", MAX_ITEMS).into_bytes();
        input.extend(e.repeat(MAX_ITEMS).as_bytes());
        let (v, _) = read_value(&input).await;
        assert!(count(&v).1 <= MAX_RESULT_BYTES);
    }

    #[tokio::test]
    async fn too_deep_nesting_is_skipped_not_an_error() {
        let depth = MAX_DEPTH + 50;
        let mut input = Vec::new();
        for _ in 0..depth {
            input.extend(b"*2\r\n:1\r\n");
        }
        input.extend(b":2\r\n");
        input.extend(b":7\r\n");
        let mut rd = live::FrameReader::with_limits(&input[..], RESULT_LIMITS);
        let mut tr = false;
        let mut v = to_value(rd.read().await.unwrap(), &mut tr);
        assert!(tr);
        let mut levels = 0;
        while let LuaValue::Array { mut items, len } = v {
            levels += 1;
            if items.len() < 2 {
                // 最深处只记个数
                assert!(items.is_empty() && len == 2, "{items:?} {len}");
                break;
            }
            v = items.pop().unwrap();
        }
        assert_eq!(levels, MAX_DEPTH + 1);
        assert_eq!(rd.read().await.unwrap(), Frame::Int(7));
    }

    #[test]
    fn truncated_utf8_tail_is_not_binary() {
        // "中" 的前两个字节,原长更长:截断切开了一个字符
        let v = bytes_value(vec![b'a', 0xE4, 0xB8], 10);
        assert_eq!(v, LuaValue::Str { text: "a".into(), b64: None, stored: 3, len: 10 });
    }
}
