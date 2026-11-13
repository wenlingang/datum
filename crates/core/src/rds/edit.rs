//! Redis key 的结构化编辑:按类型增删改元素、TTL、重命名、新建,以及 Stream 的分页浏览与消费组。
//!
//! 约定:
//! - 命令一律用 `cmd().arg()` 逐个传参(二进制安全),不拼命令文本;
//! - 只按字段 / 成员 / 下标改界面上显示出来的那部分,不整体读写 key:大 key 本来就只读了前 1000 个元素;
//! - 写之前核对类型:key 被删或换了类型时报错,不会让 HSET / SADD 之类把它悄悄重新建出来;
//! - 多步改动(改字段名、删列表元素)用 MULTI/EXEC 一次提交。多路复用连接被别的请求共用,不能 WATCH,
//!   所以「核对旧值」与事务之间仍有极短的窗口;换来的是不依赖 Lua(不少托管实例禁用 EVAL)。

use super::client::qerr;
use crate::error::{AppError, AppResult, ErrorKind};
use redis::aio::MultiplexedConnection;
use redis::Value;
use rust_i18n::t;
use serde::{Deserialize, Serialize};

/// 对一个已存在的 key 做的一次元素级改动(界面上的一次保存 / 删除)。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum KeyEdit {
    /// 整体替换字符串(保留原 TTL)
    StringSet { value: String },
    /// 新增字段;已存在时报错,不覆盖
    HashAdd { field: String, value: String },
    /// 改字段的值;`old_field` 与 `field` 不同时是改字段名(新名已存在时报错)
    HashSet { field: String, value: String, old_field: Option<String> },
    HashDel { field: String },
    /// 改指定下标的值;`old` 是界面上显示的旧值,与实际不符(列表被别人改过)时报错
    ListSet { index: i64, old: String, value: String },
    /// 从头部(LPUSHX)或尾部(RPUSHX)插入
    ListPush { value: String, head: bool },
    /// 删除指定下标的元素,`old` 同 ListSet
    ListDel { index: i64, old: String },
    SetAdd { member: String },
    SetDel { member: String },
    /// 新增成员;已存在时报错。分数是文本(允许 inf / -inf)
    ZsetAdd { member: String, score: String },
    /// 改分数;`old_member` 与 `member` 不同时是改成员名
    ZsetSet { member: String, score: String, old_member: Option<String> },
    ZsetDel { member: String },
    /// 追加条目;`id` 为空或 `*` 由服务端生成
    StreamAdd { id: String, fields: Vec<(String, String)> },
    StreamDel { id: String },
}

/// 一次改动的结果:`changed` 为实际改动的元素数(删除一个已不存在的元素为 0);追加 Stream 条目时带回 ID。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EditResult {
    pub changed: i64,
    pub id: Option<String>,
}

impl EditResult {
    fn n(changed: i64) -> Self {
        Self { changed, id: None }
    }
}

/// 新建 key:类型 + 初始元素 + 可选 TTL(秒)。
/// `items` 每项按类型:string `[值]`(只取第一项)、hash `[字段, 值]`、list / set `[元素]`、
/// zset `[成员, 分数]`、stream `[字段, 值]`(合成一个条目)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewKey {
    pub key: String,
    pub key_type: String,
    pub items: Vec<Vec<String>>,
    pub ttl: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StreamEntry {
    pub id: String,
    pub fields: Vec<(String, String)>,
}

/// Stream 的一页。`next` 是下一页的起点(含):正序时作为新的 start,倒序时作为新的 end;没有更多为 None。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StreamPage {
    pub entries: Vec<StreamEntry>,
    pub next: Option<String>,
    /// XLEN
    pub length: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StreamConsumer {
    pub name: String,
    pub pending: i64,
    pub idle_ms: i64,
    /// 7.2 起才有(距上次成功读取的时长)
    pub inactive_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StreamGroup {
    pub name: String,
    pub consumers: i64,
    pub pending: i64,
    pub last_delivered_id: String,
    /// 7.0 起才有;lag 在服务端算不出来时也为空
    pub entries_read: Option<i64>,
    pub lag: Option<i64>,
    /// XPENDING 摘要:待确认消息里最小 / 最大的 ID
    pub pending_min: Option<String>,
    pub pending_max: Option<String>,
    pub consumer_list: Vec<StreamConsumer>,
}

/// Stream 按 ID 范围取一页的参数:`start` / `end` 为 XRANGE 的区间(`-` / `+` 为两端),`rev` 为倒序。
#[derive(Debug, Clone, Deserialize)]
pub struct StreamRange {
    pub start: String,
    pub end: String,
    pub count: usize,
    pub rev: bool,
}

/// 一页最多取的 Stream 条目数(与查看集合类 key 的上限一致)。
pub const MAX_STREAM_PAGE: usize = super::client::MAX_ELEMENTS;

fn invalid(msg: impl Into<String>) -> AppError {
    AppError::new(ErrorKind::Edit, msg)
}

/// 核对 key 的当前类型:不存在或换了类型都报错。
async fn expect_type(con: &mut MultiplexedConnection, key: &str, want: &str) -> AppResult<()> {
    let actual: String = redis::cmd("TYPE").arg(key).query_async(con).await.map_err(qerr)?;
    if actual == want {
        Ok(())
    } else if actual == "none" {
        Err(invalid(t!("core.err.redis_key_gone", key = key)))
    } else {
        Err(invalid(t!("core.err.redis_type_changed", key = key, actual = actual, expected = want)))
    }
}

/// 分数:Redis 认的浮点文本(含 inf / -inf);NaN 与空串拒绝。原样(去空白)传给服务端,不经 f64 往返丢精度。
fn check_score(s: &str) -> AppResult<&str> {
    let s = s.trim();
    match s.parse::<f64>() {
        Ok(v) if !v.is_nan() => Ok(s),
        _ => Err(invalid(t!("core.err.redis_bad_score", score = s))),
    }
}

/// 列表下标处的当前值须与界面上显示的一致,否则说明列表已被改动(插入 / 删除会让下标整体错位)。
async fn expect_list_item(con: &mut MultiplexedConnection, key: &str, index: i64, old: &str) -> AppResult<()> {
    let cur: Option<Vec<u8>> = redis::cmd("LINDEX").arg(key).arg(index).query_async(con).await.map_err(qerr)?;
    if cur.as_deref() == Some(old.as_bytes()) {
        Ok(())
    } else {
        Err(invalid(t!("core.err.redis_list_changed", index = index)))
    }
}

pub async fn apply(con: &mut MultiplexedConnection, key: &str, edit: &KeyEdit) -> AppResult<EditResult> {
    match edit {
        KeyEdit::StringSet { value } => {
            expect_type(con, key, "string").await?;
            // SET 会清掉 TTL:读出剩余毫秒,与 SET 放进同一事务里补回(KEEPTTL 要 6.0,这样老版本也行)
            let pttl: i64 = redis::cmd("PTTL").arg(key).query_async(con).await.map_err(qerr)?;
            let mut p = redis::pipe();
            p.atomic().cmd("SET").arg(key).arg(value.as_bytes()).arg("XX").ignore();
            if pttl > 0 {
                p.cmd("PEXPIRE").arg(key).arg(pttl).ignore();
            }
            let () = p.query_async(con).await.map_err(qerr)?;
            Ok(EditResult::n(1))
        }
        KeyEdit::HashAdd { field, value } => {
            expect_type(con, key, "hash").await?;
            let n: i64 = redis::cmd("HSETNX").arg(key).arg(field.as_bytes()).arg(value.as_bytes()).query_async(con).await.map_err(qerr)?;
            if n == 0 {
                return Err(invalid(t!("core.err.redis_field_exists", name = field)));
            }
            Ok(EditResult::n(1))
        }
        KeyEdit::HashSet { field, value, old_field } => {
            expect_type(con, key, "hash").await?;
            match old_field.as_deref() {
                Some(old) if old != field => {
                    let exists: bool = redis::cmd("HEXISTS").arg(key).arg(field.as_bytes()).query_async(con).await.map_err(qerr)?;
                    if exists {
                        return Err(invalid(t!("core.err.redis_field_exists", name = field)));
                    }
                    let () = redis::pipe()
                        .atomic()
                        .cmd("HSET").arg(key).arg(field.as_bytes()).arg(value.as_bytes()).ignore()
                        .cmd("HDEL").arg(key).arg(old.as_bytes()).ignore()
                        .query_async(con)
                        .await
                        .map_err(qerr)?;
                }
                _ => {
                    let _: i64 = redis::cmd("HSET").arg(key).arg(field.as_bytes()).arg(value.as_bytes()).query_async(con).await.map_err(qerr)?;
                }
            }
            Ok(EditResult::n(1))
        }
        KeyEdit::HashDel { field } => {
            expect_type(con, key, "hash").await?;
            let n: i64 = redis::cmd("HDEL").arg(key).arg(field.as_bytes()).query_async(con).await.map_err(qerr)?;
            Ok(EditResult::n(n))
        }
        KeyEdit::ListSet { index, old, value } => {
            expect_type(con, key, "list").await?;
            expect_list_item(con, key, *index, old).await?;
            let () = redis::cmd("LSET").arg(key).arg(*index).arg(value.as_bytes()).query_async(con).await.map_err(qerr)?;
            Ok(EditResult::n(1))
        }
        KeyEdit::ListPush { value, head } => {
            // *PUSHX 只在 key 存在时插入:被删掉的列表不会因此重新出现
            let n: i64 = redis::cmd(if *head { "LPUSHX" } else { "RPUSHX" })
                .arg(key)
                .arg(value.as_bytes())
                .query_async(con)
                .await
                .map_err(qerr)?;
            if n == 0 {
                return Err(invalid(t!("core.err.redis_key_gone", key = key)));
            }
            Ok(EditResult::n(1))
        }
        KeyEdit::ListDel { index, old } => {
            expect_type(con, key, "list").await?;
            expect_list_item(con, key, *index, old).await?;
            // 按下标删:先把这一格换成独一无二的占位值,再 LREM 掉这个占位值,两步在同一事务里。
            // 直接按值 LREM 会删掉第一个相同的值,列表里有重复值时删的未必是选中的那一格。
            let tomb = format!("__datum_deleted__{}", uuid::Uuid::new_v4());
            let () = redis::pipe()
                .atomic()
                .cmd("LSET").arg(key).arg(*index).arg(&tomb).ignore()
                .cmd("LREM").arg(key).arg(1).arg(&tomb).ignore()
                .query_async(con)
                .await
                .map_err(qerr)?;
            Ok(EditResult::n(1))
        }
        KeyEdit::SetAdd { member } => {
            expect_type(con, key, "set").await?;
            let n: i64 = redis::cmd("SADD").arg(key).arg(member.as_bytes()).query_async(con).await.map_err(qerr)?;
            if n == 0 {
                return Err(invalid(t!("core.err.redis_member_exists", name = member)));
            }
            Ok(EditResult::n(1))
        }
        KeyEdit::SetDel { member } => {
            expect_type(con, key, "set").await?;
            let n: i64 = redis::cmd("SREM").arg(key).arg(member.as_bytes()).query_async(con).await.map_err(qerr)?;
            Ok(EditResult::n(n))
        }
        KeyEdit::ZsetAdd { member, score } => {
            let score = check_score(score)?;
            expect_type(con, key, "zset").await?;
            let n: i64 = redis::cmd("ZADD").arg(key).arg("NX").arg(score).arg(member.as_bytes()).query_async(con).await.map_err(qerr)?;
            if n == 0 {
                return Err(invalid(t!("core.err.redis_member_exists", name = member)));
            }
            Ok(EditResult::n(1))
        }
        KeyEdit::ZsetSet { member, score, old_member } => {
            let score = check_score(score)?;
            expect_type(con, key, "zset").await?;
            match old_member.as_deref() {
                Some(old) if old != member => {
                    let cur: Option<String> = redis::cmd("ZSCORE").arg(key).arg(member.as_bytes()).query_async(con).await.map_err(qerr)?;
                    if cur.is_some() {
                        return Err(invalid(t!("core.err.redis_member_exists", name = member)));
                    }
                    let () = redis::pipe()
                        .atomic()
                        .cmd("ZADD").arg(key).arg(score).arg(member.as_bytes()).ignore()
                        .cmd("ZREM").arg(key).arg(old.as_bytes()).ignore()
                        .query_async(con)
                        .await
                        .map_err(qerr)?;
                }
                _ => {
                    let _: i64 = redis::cmd("ZADD").arg(key).arg(score).arg(member.as_bytes()).query_async(con).await.map_err(qerr)?;
                }
            }
            Ok(EditResult::n(1))
        }
        KeyEdit::ZsetDel { member } => {
            expect_type(con, key, "zset").await?;
            let n: i64 = redis::cmd("ZREM").arg(key).arg(member.as_bytes()).query_async(con).await.map_err(qerr)?;
            Ok(EditResult::n(n))
        }
        KeyEdit::StreamAdd { id, fields } => {
            if fields.is_empty() {
                return Err(invalid(t!("core.err.redis_no_items")));
            }
            expect_type(con, key, "stream").await?;
            let id = if id.trim().is_empty() { "*" } else { id.trim() };
            let mut c = redis::cmd("XADD");
            c.arg(key).arg(id);
            for (f, v) in fields {
                c.arg(f.as_bytes()).arg(v.as_bytes());
            }
            let new_id: String = c.query_async(con).await.map_err(qerr)?;
            Ok(EditResult { changed: 1, id: Some(new_id) })
        }
        KeyEdit::StreamDel { id } => {
            expect_type(con, key, "stream").await?;
            let n: i64 = redis::cmd("XDEL").arg(key).arg(id).query_async(con).await.map_err(qerr)?;
            Ok(EditResult::n(n))
        }
    }
}

/// 设置(`Some(秒)`,EXPIRE)或移除(`None`,PERSIST)过期时间。
pub async fn set_ttl(con: &mut MultiplexedConnection, key: &str, seconds: Option<i64>) -> AppResult<()> {
    match seconds {
        Some(s) if s > 0 => {
            let n: i64 = redis::cmd("EXPIRE").arg(key).arg(s).query_async(con).await.map_err(qerr)?;
            if n == 0 {
                return Err(invalid(t!("core.err.redis_key_gone", key = key)));
            }
            Ok(())
        }
        Some(_) => Err(invalid(t!("core.err.redis_bad_ttl"))),
        None => {
            let n: i64 = redis::cmd("PERSIST").arg(key).query_async(con).await.map_err(qerr)?;
            if n == 0 {
                // 0 既可能是本来就没有 TTL,也可能是 key 不在了
                let exists: i64 = redis::cmd("EXISTS").arg(key).query_async(con).await.map_err(qerr)?;
                if exists == 0 {
                    return Err(invalid(t!("core.err.redis_key_gone", key = key)));
                }
            }
            Ok(())
        }
    }
}

/// 重命名(RENAMENX):目标已存在时报错,不覆盖。集群里两个 key 须在同一槽位,否则服务端报 CROSSSLOT。
pub async fn rename(con: &mut MultiplexedConnection, key: &str, new_key: &str) -> AppResult<()> {
    if new_key.is_empty() {
        return Err(invalid(t!("core.err.redis_empty_key")));
    }
    if new_key == key {
        return Ok(());
    }
    match redis::cmd("RENAMENX").arg(key).arg(new_key).query_async::<i64>(con).await {
        Ok(1) => Ok(()),
        Ok(_) => Err(invalid(t!("core.err.redis_key_exists", key = new_key))),
        Err(e) if e.to_string().to_ascii_lowercase().contains("no such key") => {
            Err(invalid(t!("core.err.redis_key_gone", key = key)))
        }
        Err(e) => Err(qerr(e)),
    }
}

/// 新建 key;已存在时报错。字符串用 SET NX 一步完成;其余类型先 EXISTS 再在一个事务里写入 + 设 TTL
/// (检查与写入之间有极短窗口,同上不用 WATCH)。
pub async fn create(con: &mut MultiplexedConnection, spec: &NewKey) -> AppResult<()> {
    let key = spec.key.as_str();
    if key.is_empty() {
        return Err(invalid(t!("core.err.redis_empty_key")));
    }
    if matches!(spec.ttl, Some(s) if s <= 0) {
        return Err(invalid(t!("core.err.redis_bad_ttl")));
    }
    let exists_err = || invalid(t!("core.err.redis_key_exists", key = key));
    let items = &spec.items;
    // 每项须有该类型需要的列数;空的初值没有意义(除了空字符串)
    let width = match spec.key_type.as_str() {
        "string" | "list" | "set" => 1,
        "hash" | "zset" | "stream" => 2,
        other => return Err(invalid(t!("core.err.redis_bad_type", kind = other))),
    };
    if items.is_empty() || items.iter().any(|it| it.len() < width) {
        return Err(invalid(t!("core.err.redis_no_items")));
    }
    if spec.key_type == "string" {
        let mut c = redis::cmd("SET");
        c.arg(key).arg(items[0][0].as_bytes()).arg("NX");
        if let Some(s) = spec.ttl {
            c.arg("EX").arg(s);
        }
        let ok: Option<String> = c.query_async(con).await.map_err(qerr)?;
        return if ok.is_some() { Ok(()) } else { Err(exists_err()) };
    }
    let exists: i64 = redis::cmd("EXISTS").arg(key).query_async(con).await.map_err(qerr)?;
    if exists > 0 {
        return Err(exists_err());
    }
    let mut c = match spec.key_type.as_str() {
        "hash" => redis::cmd("HSET"),
        "list" => redis::cmd("RPUSH"),
        "set" => redis::cmd("SADD"),
        "zset" => redis::cmd("ZADD"),
        _ => redis::cmd("XADD"),
    };
    c.arg(key);
    if spec.key_type == "stream" {
        c.arg("*");
    }
    for it in items {
        match spec.key_type.as_str() {
            "zset" => {
                c.arg(check_score(&it[1])?).arg(it[0].as_bytes());
            }
            "hash" | "stream" => {
                c.arg(it[0].as_bytes()).arg(it[1].as_bytes());
            }
            _ => {
                c.arg(it[0].as_bytes());
            }
        }
    }
    let mut p = redis::pipe();
    p.atomic().add_command(c).ignore();
    if let Some(s) = spec.ttl {
        p.cmd("EXPIRE").arg(key).arg(s).ignore();
    }
    let () = p.query_async(con).await.map_err(qerr)?;
    Ok(())
}

fn text(v: &Value) -> String {
    match v {
        Value::BulkString(b) => String::from_utf8_lossy(b).into_owned(),
        Value::SimpleString(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Double(d) => d.to_string(),
        Value::VerbatimString { text, .. } => text.clone(),
        _ => String::new(),
    }
}

fn int(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Int(i) => Some(*i),
        Value::Nil => None,
        other => text(other).parse().ok(),
    }
}

/// 扁平的 `[k1, v1, k2, v2…]`(RESP2)或 Map(RESP3)转成键值对。
fn pairs(v: &Value) -> Vec<(String, &Value)> {
    match v {
        Value::Array(a) => a.chunks(2).filter(|c| c.len() == 2).map(|c| (text(&c[0]), &c[1])).collect(),
        Value::Map(m) => m.iter().map(|(k, v)| (text(k), v)).collect(),
        _ => vec![],
    }
}

fn get<'a>(p: &'a [(String, &'a Value)], name: &str) -> Option<&'a Value> {
    p.iter().find(|(k, _)| k == name).map(|(_, v)| *v)
}

/// 解析 XRANGE / XREVRANGE 的回复:`[[id, [f1, v1, …]], …]`。
fn parse_entries(v: &Value) -> Vec<StreamEntry> {
    let Value::Array(items) = v else {
        return vec![];
    };
    items
        .iter()
        .filter_map(|e| {
            let Value::Array(parts) = e else {
                return None;
            };
            let id = text(parts.first()?);
            let fields = match parts.get(1) {
                Some(f) => pairs(f).into_iter().map(|(k, v)| (k, text(v))).collect(),
                None => vec![],
            };
            Some(StreamEntry { id, fields })
        })
        .collect()
}

/// 按 ID 范围取一页:正序 XRANGE start end,倒序 XREVRANGE end start。多取一条判断是否还有下一页,
/// 那一条的 ID 就是下一页的起点(不用「(id」排他区间,那要 6.2)。
pub async fn stream_range(con: &mut MultiplexedConnection, key: &str, q: &StreamRange) -> AppResult<StreamPage> {
    expect_type(con, key, "stream").await?;
    let count = q.count.clamp(1, MAX_STREAM_PAGE);
    let rev = q.rev;
    let (a, b) = if rev { (&q.end, &q.start) } else { (&q.start, &q.end) };
    let v: Value = redis::cmd(if rev { "XREVRANGE" } else { "XRANGE" })
        .arg(key)
        .arg(a)
        .arg(b)
        .arg("COUNT")
        .arg(count + 1)
        .query_async(con)
        .await
        .map_err(qerr)?;
    let mut entries = parse_entries(&v);
    let next = if entries.len() > count { entries.pop().map(|e| e.id) } else { None };
    let length: i64 = redis::cmd("XLEN").arg(key).query_async(con).await.map_err(qerr)?;
    Ok(StreamPage { entries, next, length })
}

/// 消费组概况:XINFO GROUPS,每组再取 XINFO CONSUMERS 与 XPENDING 摘要(不取逐条待确认明细)。
pub async fn stream_groups(con: &mut MultiplexedConnection, key: &str) -> AppResult<Vec<StreamGroup>> {
    expect_type(con, key, "stream").await?;
    let v: Value = redis::cmd("XINFO").arg("GROUPS").arg(key).query_async(con).await.map_err(qerr)?;
    let Value::Array(groups) = v else {
        return Ok(vec![]);
    };
    let mut out = vec![];
    for g in &groups {
        let p = pairs(g);
        let name = get(&p, "name").map(text).unwrap_or_default();
        let cv: Value = redis::cmd("XINFO").arg("CONSUMERS").arg(key).arg(&name).query_async(con).await.map_err(qerr)?;
        let consumer_list = match &cv {
            Value::Array(cs) => cs
                .iter()
                .map(|c| {
                    let cp = pairs(c);
                    StreamConsumer {
                        name: get(&cp, "name").map(text).unwrap_or_default(),
                        pending: int(get(&cp, "pending")).unwrap_or(0),
                        idle_ms: int(get(&cp, "idle")).unwrap_or(0),
                        inactive_ms: int(get(&cp, "inactive")),
                    }
                })
                .collect(),
            _ => vec![],
        };
        // XPENDING 摘要:[总数, 最小 ID, 最大 ID, [[消费者, 数量]…]];没有待确认时后三项为 nil
        let pv: Value = redis::cmd("XPENDING").arg(key).arg(&name).query_async(con).await.map_err(qerr)?;
        let (pending_min, pending_max) = match &pv {
            Value::Array(a) => (
                a.get(1).filter(|x| !matches!(x, Value::Nil)).map(text),
                a.get(2).filter(|x| !matches!(x, Value::Nil)).map(text),
            ),
            _ => (None, None),
        };
        out.push(StreamGroup {
            name,
            consumers: int(get(&p, "consumers")).unwrap_or(0),
            pending: int(get(&p, "pending")).unwrap_or(0),
            last_delivered_id: get(&p, "last-delivered-id").map(text).unwrap_or_default(),
            entries_read: int(get(&p, "entries-read")),
            lag: int(get(&p, "lag")),
            pending_min,
            pending_max,
            consumer_list,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_deserialize_from_camel_case_json() {
        let e: KeyEdit = serde_json::from_str(r#"{"op":"hashSet","field":"b","value":"v","oldField":"a"}"#).unwrap();
        assert!(matches!(e, KeyEdit::HashSet { ref field, ref old_field, .. } if field == "b" && old_field.as_deref() == Some("a")));
        let e: KeyEdit = serde_json::from_str(r#"{"op":"listDel","index":3,"old":"x"}"#).unwrap();
        assert!(matches!(e, KeyEdit::ListDel { index: 3, .. }));
        let e: KeyEdit = serde_json::from_str(r#"{"op":"streamAdd","id":"*","fields":[["a","1"],["b","2"]]}"#).unwrap();
        assert!(matches!(e, KeyEdit::StreamAdd { ref fields, .. } if fields.len() == 2));
    }

    #[test]
    fn scores_accept_redis_floats_only() {
        assert_eq!(check_score(" 1.5 ").unwrap(), "1.5");
        assert!(check_score("-inf").is_ok());
        assert!(check_score("+inf").is_ok());
        assert!(check_score("1e3").is_ok());
        assert!(check_score("").is_err());
        assert!(check_score("nan").is_err());
        assert!(check_score("abc").is_err());
    }

    #[test]
    fn parses_stream_entries_and_flat_pairs() {
        let bulk = |s: &str| Value::BulkString(s.as_bytes().to_vec());
        let v = Value::Array(vec![
            Value::Array(vec![bulk("1-0"), Value::Array(vec![bulk("a"), bulk("1"), bulk("b"), bulk("2")])]),
            Value::Array(vec![bulk("2-0"), Value::Array(vec![bulk("c"), bulk("3")])]),
        ]);
        let e = parse_entries(&v);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].id, "1-0");
        assert_eq!(e[0].fields, vec![("a".to_string(), "1".to_string()), ("b".to_string(), "2".to_string())]);
        let m = Value::Map(vec![(bulk("name"), bulk("g")), (bulk("pending"), Value::Int(2))]);
        let p = pairs(&m);
        assert_eq!(int(get(&p, "pending")), Some(2));
        assert_eq!(get(&p, "name").map(text).as_deref(), Some("g"));
    }
}
