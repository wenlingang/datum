//! 写操作留痕(写日志):Datum 执行过的每次写操作按连接记进 `journal/<连接 id>.jsonl`,
//! 一行一条 JSON,只追加。带着撤销用的反向语句与改动前的行(可在设置里关掉),重启之后也能撤销。
//!
//! - 追加:以 0600 打开、O_APPEND 写一整行再 fsync;上次写到一半(崩溃、断电)留下的半行,
//!   读取时跳过,下次追加前先补一个换行,不会和新行粘在一起。
//! - 「已撤销」不改旧行:撤销本身也是一条记录(`undoes` 指向被撤销的记录),读取时反推。
//! - 保留:每个连接只留最近 N 天(默认 90)且不超过 50 MB;超了就重写文件,先丢超龄的,再从最旧的丢起。
//! - 文件里有 SQL 原文与改动前的整行数据:目录 0700、文件 0600,单条 SQL 与撤销数据都有大小上限。

use crate::error::{AppError, AppResult, ErrorKind};
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// 单条语句原文的上限(字节):超出截断并标记。
pub const MAX_SQL_BYTES: usize = 64 * 1024;
/// 错误信息的上限(字节)。
pub const MAX_ERROR_BYTES: usize = 4 * 1024;
/// 一条记录里撤销数据(反向语句 + 快照)序列化后的上限:超出就只记语句,不留行数据。
pub const MAX_UNDO_BYTES: usize = 8 * 1024 * 1024;
/// 每个连接的文件上限;超出后重写到 [`COMPACT_TARGET_BYTES`] 以内(留出余量,不至于每次追加都重写)。
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const COMPACT_TARGET_BYTES: u64 = 40 * 1024 * 1024;
/// 快照里给界面预览的行数。
pub const PREVIEW_ROWS: usize = 20;
/// 列表里每条 SQL 只带开头这么多字节(完整原文用 [`Journal::get`] 取)。
pub const LIST_SQL_BYTES: usize = 2 * 1024;
/// 列表一次最多返回的条数。
pub const MAX_LIST: usize = 2000;
pub const DEFAULT_RETENTION_DAYS: u32 = 90;
const DAY_MS: i64 = 24 * 3600 * 1000;

/// 写日志的设置(全局,`journal_settings.json`)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct JournalSettings {
    /// 记不记写操作
    pub enabled: bool,
    /// 是否保存撤销用的行数据(反向语句、改动前的行);关掉只记语句
    pub store_rows: bool,
    /// 保留天数(1..=3650)
    pub retention_days: u32,
}

impl Default for JournalSettings {
    fn default() -> Self {
        Self { enabled: true, store_rows: true, retention_days: DEFAULT_RETENTION_DAYS }
    }
}

/// 一条写语句改动前的行(撤销预览与撤销前核对用)。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UndoSnapshot {
    /// 表名原文
    pub table: String,
    /// true:DELETE(撤销是插回去);false:UPDATE(撤销是改回去)
    pub delete: bool,
    pub columns: Vec<String>,
    /// 前 [`PREVIEW_ROWS`] 行,给人看的值
    pub rows: Vec<Vec<Option<String>>>,
    /// 快照的总行数
    pub total: usize,
    /// 主键列;空表示表没有主键(无法逐行核对)
    pub primary_key: Vec<String>,
    /// 每行的主键值(SQL 字面量),与 primary_key 对应
    pub keys: Vec<Vec<String>>,
}

/// 记录里的撤销数据:反向语句(按执行顺序)+ 快照。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UndoData {
    pub statements: Vec<String>,
    pub rows: usize,
    pub snapshots: Vec<UndoSnapshot>,
}

/// 一条写操作记录。字段缺失时按默认值读(旧版本 / 手改过的文件不至于整行作废)。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct JournalEntry {
    pub id: String,
    /// Unix 毫秒
    pub ts: i64,
    /// 写入时的进程会话:与当前不同就是「上次运行」的记录
    pub session: String,
    pub conn_id: String,
    pub conn_name: String,
    /// local / dev / staging / prod
    pub env: String,
    /// pg / mysql / sqlite / redis
    pub kind: String,
    pub database: String,
    /// editor / ai / grid / undo / import / alter / redis / lua
    pub source: String,
    /// 一次执行多条语句时同属一批
    pub batch: Option<String>,
    pub sql: String,
    /// sql 超过上限被截断
    pub truncated: bool,
    pub affected: Option<u64>,
    pub ok: bool,
    pub error: Option<String>,
    pub elapsed_ms: u64,
    pub undo: Option<UndoData>,
    /// 不能撤销的原因(有撤销数据时为空)
    pub undo_reason: Option<String>,
    /// 这条是撤销:撤销的是哪几条记录
    pub undoes: Vec<String>,
}

/// 列表里的一条:不带撤销数据本身,只带摘要。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalSummary {
    pub id: String,
    pub ts: i64,
    pub conn_id: String,
    pub conn_name: String,
    pub env: String,
    pub kind: String,
    pub database: String,
    pub source: String,
    pub batch: Option<String>,
    /// 开头一段 SQL
    pub sql: String,
    pub truncated: bool,
    pub affected: Option<u64>,
    pub ok: bool,
    pub error: Option<String>,
    pub elapsed_ms: u64,
    /// 有撤销数据时为会恢复的行数
    pub undo_rows: Option<usize>,
    pub undo_reason: Option<String>,
    pub undoes: Vec<String>,
    /// 被哪条撤销记录撤销了
    pub undone_by: Option<String>,
    pub previous_session: bool,
}

/// 单条记录的完整内容 + 撤销前要提醒的情况。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalDetail {
    pub entry: JournalEntry,
    pub undone_by: Option<String>,
    pub previous_session: bool,
    /// 之后又记了几条改到同一张表的写操作(撤销会把它们的改动一并覆盖)
    pub later_writes: usize,
}

/// 列表筛选。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct JournalFilter {
    pub source: Option<String>,
    /// "ok" | "error"
    pub status: Option<String>,
    /// 在 SQL、错误信息里找(不分大小写)
    pub text: Option<String>,
    /// 时间范围(Unix 毫秒,含)
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub limit: Option<usize>,
}

impl JournalFilter {
    fn matches(&self, e: &JournalEntry) -> bool {
        if self.source.as_deref().is_some_and(|s| !s.is_empty() && s != e.source) {
            return false;
        }
        match self.status.as_deref() {
            Some("ok") if !e.ok => return false,
            Some("error") if e.ok => return false,
            _ => {}
        }
        if self.from.is_some_and(|f| e.ts < f) || self.to.is_some_and(|t| e.ts > t) {
            return false;
        }
        if let Some(q) = self.text.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            let q = q.to_lowercase();
            let hit = e.sql.to_lowercase().contains(&q) || e.error.as_deref().is_some_and(|x| x.to_lowercase().contains(&q));
            if !hit {
                return false;
            }
        }
        true
    }
}

/// 本进程的会话 id(进程内固定)。
pub fn session_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

/// 截到不超过 `max` 字节的字符边界;返回是否截断。
fn cap(s: &mut String, max: usize) -> bool {
    if s.len() <= max {
        return false;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    true
}

fn head(s: &str, max: usize) -> String {
    let mut out = s.to_string();
    cap(&mut out, max);
    out
}

/// 连接 id → 文件名:只留字母数字与 `-_`,其余转成十六进制(id 正常是 uuid,防的是手改的配置)。
fn file_stem(conn_id: &str) -> String {
    let safe = !conn_id.is_empty() && conn_id.len() <= 128 && conn_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if safe {
        conn_id.to_string()
    } else {
        conn_id.bytes().map(|b| format!("{b:02x}")).collect::<String>().chars().take(200).collect()
    }
}

fn io_err(e: std::io::Error) -> AppError {
    AppError::new(ErrorKind::Config, t!("core.journal.write_failed")).with_detail(e.to_string())
}

/// 改到的表(给「之后又改了同一张表」用):撤销快照的表名,否则从语句里认。
fn tables_of(e: &JournalEntry) -> Vec<String> {
    let norm = |t: &str| crate::sql::impact::unquote(t.rsplit('.').next().unwrap_or(t)).to_lowercase();
    let mut out: Vec<String> = e.undo.iter().flat_map(|u| u.snapshots.iter().map(|s| norm(&s.table))).collect();
    if out.is_empty() {
        for stmt in crate::sql::stmt::statements(&e.sql) {
            for d in [crate::sql::Dialect::Pg, crate::sql::Dialect::Mysql] {
                if let Some(q) = crate::sql::impact::impact_query(&stmt, d) {
                    out.push(norm(&q.table));
                    break;
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 写日志:配置目录 + `journal/` 子目录。
pub struct Journal {
    dir: PathBuf,
    settings_path: PathBuf,
}

impl Journal {
    pub fn new(config_dir: &Path) -> Self {
        Self { dir: config_dir.join("journal"), settings_path: config_dir.join("journal_settings.json") }
    }

    pub fn path(&self, conn_id: &str) -> PathBuf {
        self.dir.join(format!("{}.jsonl", file_stem(conn_id)))
    }

    /// 设置;缺失或读不了用默认值(默认开启)。
    pub fn settings(&self) -> JournalSettings {
        let mut s: JournalSettings = std::fs::read(&self.settings_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        s.retention_days = s.retention_days.clamp(1, 3650);
        s
    }

    pub fn save_settings(&self, s: &JournalSettings) -> AppResult<JournalSettings> {
        let mut s = s.clone();
        s.retention_days = s.retention_days.clamp(1, 3650);
        let json = serde_json::to_vec_pretty(&s).map_err(|e| AppError::new(ErrorKind::Config, t!("core.journal.write_failed")).with_detail(e.to_string()))?;
        crate::core::fsutil::write_private(&self.settings_path, &json).map_err(io_err)?;
        Ok(s)
    }

    /// 追加一条;写日志关掉时不写,返回 None。id / 时间 / 会话为空时补上。
    /// 「不保存行数据」时去掉撤销数据(只记语句),并注明原因。
    pub fn append(&self, mut entry: JournalEntry) -> AppResult<Option<String>> {
        let settings = self.settings();
        if !settings.enabled || entry.conn_id.is_empty() {
            return Ok(None);
        }
        if entry.id.is_empty() {
            entry.id = uuid::Uuid::new_v4().to_string();
        }
        if entry.ts == 0 {
            entry.ts = chrono::Utc::now().timestamp_millis();
        }
        if entry.session.is_empty() {
            entry.session = session_id().to_string();
        }
        if cap(&mut entry.sql, MAX_SQL_BYTES) {
            entry.truncated = true;
        }
        if let Some(err) = entry.error.as_mut() {
            cap(err, MAX_ERROR_BYTES);
        }
        if let Some(u) = &entry.undo {
            if u.statements.is_empty() {
                entry.undo = None;
            } else if !settings.store_rows {
                entry.undo = None;
                entry.undo_reason = Some(t!("core.journal.rows_not_stored").to_string());
            } else if serde_json::to_vec(u).map(|b| b.len()).unwrap_or(usize::MAX) > MAX_UNDO_BYTES {
                entry.undo = None;
                entry.undo_reason = Some(t!("core.journal.undo_too_large").to_string());
            }
        }
        if entry.undo.is_some() {
            entry.undo_reason = None;
        }
        let mut line = serde_json::to_vec(&entry)
            .map_err(|e| AppError::new(ErrorKind::Config, t!("core.journal.write_failed")).with_detail(e.to_string()))?;
        line.push(b'\n');
        let path = self.path(&entry.conn_id);
        append_line(&path, &line).map_err(io_err)?;
        self.maybe_compact(&path, settings.retention_days)?;
        Ok(Some(entry.id))
    }

    /// 读出全部记录(按写入顺序);坏行(写到一半的最后一行、手改坏的行)跳过。
    pub fn read_all(&self, conn_id: &str) -> Vec<JournalEntry> {
        read_entries(&self.path(conn_id))
    }

    /// 被撤销的记录 id → 撤销它的记录 id。
    fn undone_map(all: &[JournalEntry]) -> std::collections::HashMap<String, String> {
        let mut m = std::collections::HashMap::new();
        for e in all.iter().filter(|e| e.ok) {
            for id in &e.undoes {
                m.entry(id.clone()).or_insert_with(|| e.id.clone());
            }
        }
        m
    }

    /// 列表(最新在前),按筛选条件。
    pub fn list(&self, conn_id: &str, filter: &JournalFilter) -> Vec<JournalSummary> {
        let all = self.read_all(conn_id);
        let undone = Self::undone_map(&all);
        let cutoff = self.cutoff();
        let limit = filter.limit.unwrap_or(MAX_LIST).min(MAX_LIST);
        all.iter()
            .rev()
            .filter(|e| e.ts >= cutoff && filter.matches(e))
            .take(limit)
            .map(|e| JournalSummary {
                id: e.id.clone(),
                ts: e.ts,
                conn_id: e.conn_id.clone(),
                conn_name: e.conn_name.clone(),
                env: e.env.clone(),
                kind: e.kind.clone(),
                database: e.database.clone(),
                source: e.source.clone(),
                batch: e.batch.clone(),
                sql: head(&e.sql, LIST_SQL_BYTES),
                truncated: e.truncated,
                affected: e.affected,
                ok: e.ok,
                error: e.error.clone(),
                elapsed_ms: e.elapsed_ms,
                undo_rows: e.undo.as_ref().map(|u| u.rows),
                undo_reason: e.undo_reason.clone(),
                undoes: e.undoes.clone(),
                undone_by: undone.get(&e.id).cloned(),
                previous_session: e.session != session_id(),
            })
            .collect()
    }

    /// 单条完整记录。
    pub fn get(&self, conn_id: &str, id: &str) -> Option<JournalDetail> {
        let all = self.read_all(conn_id);
        let pos = all.iter().position(|e| e.id == id)?;
        let entry = all[pos].clone();
        let undone_by = Self::undone_map(&all).get(id).cloned();
        let tables = tables_of(&entry);
        let later_writes = all[pos + 1..]
            .iter()
            .filter(|e| e.ok && !e.undoes.iter().any(|u| u == id))
            .filter(|e| tables_of(e).iter().any(|t| tables.contains(t)))
            .count();
        let previous_session = entry.session != session_id();
        Some(JournalDetail { entry, undone_by, previous_session, later_writes })
    }

    /// 还能撤销的记录:id → 会恢复的行数(查询历史重启后靠它恢复「撤销」按钮)。
    pub fn undoable(&self, conn_id: &str) -> Vec<(String, usize)> {
        let all = self.read_all(conn_id);
        let undone = Self::undone_map(&all);
        let cutoff = self.cutoff();
        all.iter()
            .filter(|e| e.ok && e.ts >= cutoff && !undone.contains_key(&e.id))
            .filter_map(|e| e.undo.as_ref().map(|u| (e.id.clone(), u.rows)))
            .collect()
    }

    /// 删掉一个连接的写日志(删除连接、清空写日志)。
    pub fn clear(&self, conn_id: &str) -> AppResult<()> {
        match std::fs::remove_file(self.path(conn_id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(e)),
        }
    }

    /// 删掉全部连接的写日志。
    pub fn clear_all(&self) -> AppResult<()> {
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(e)),
        }
    }

    /// 一个连接的写日志占多少字节(没有为 0)。
    pub fn size(&self, conn_id: &str) -> u64 {
        std::fs::metadata(self.path(conn_id)).map(|m| m.len()).unwrap_or(0)
    }

    fn cutoff(&self) -> i64 {
        chrono::Utc::now().timestamp_millis() - self.settings().retention_days as i64 * DAY_MS
    }

    /// 超过大小上限,或最旧的一条已超龄:重写文件。
    fn maybe_compact(&self, path: &Path, retention_days: u32) -> AppResult<()> {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let cutoff = chrono::Utc::now().timestamp_millis() - retention_days as i64 * DAY_MS;
        let stale = first_ts(path).is_some_and(|ts| ts < cutoff);
        if size > MAX_FILE_BYTES || stale {
            compact(path, cutoff, COMPACT_TARGET_BYTES)?;
        }
        Ok(())
    }

    /// 立即按保留规则整理(测试与设置里改了保留天数后用)。
    pub fn compact_now(&self, conn_id: &str) -> AppResult<()> {
        let cutoff = self.cutoff();
        compact(&self.path(conn_id), cutoff, COMPACT_TARGET_BYTES)
    }

    /// 导出(按筛选):`md` 变更记录 / `csv`。
    pub fn export(&self, conn_id: &str, filter: &JournalFilter, format: &str) -> String {
        let all = self.read_all(conn_id);
        let undone = Self::undone_map(&all);
        let cutoff = self.cutoff();
        let rows: Vec<&JournalEntry> = all.iter().filter(|e| e.ts >= cutoff && filter.matches(e)).collect();
        let time = |ts: i64| {
            chrono::DateTime::from_timestamp_millis(ts)
                .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default()
        };
        let status = |e: &JournalEntry| {
            if !e.ok {
                t!("core.journal.status_error").to_string()
            } else if undone.contains_key(&e.id) {
                t!("core.journal.status_undone").to_string()
            } else {
                t!("core.journal.status_ok").to_string()
            }
        };
        if format == "csv" {
            let mut w = csv::WriterBuilder::new().from_writer(vec![]);
            let _ = w.write_record(["time", "connection", "env", "database", "source", "status", "affected", "elapsed_ms", "sql", "error"]);
            for e in rows {
                let _ = w.write_record([
                    time(e.ts),
                    e.conn_name.clone(),
                    e.env.clone(),
                    e.database.clone(),
                    e.source.clone(),
                    status(e),
                    e.affected.map(|n| n.to_string()).unwrap_or_default(),
                    e.elapsed_ms.to_string(),
                    e.sql.clone(),
                    e.error.clone().unwrap_or_default(),
                ]);
            }
            return String::from_utf8(w.into_inner().unwrap_or_default()).unwrap_or_default();
        }
        let name = rows.first().map(|e| e.conn_name.clone()).unwrap_or_default();
        let mut out = format!("# {}\n\n", t!("core.journal.export_title", name = name));
        let mut day = String::new();
        for e in rows {
            let t = time(e.ts);
            let d = t.get(..10).unwrap_or("").to_string();
            if d != day {
                out.push_str(&format!("## {d}\n\n"));
                day = d;
            }
            let affected = e.affected.map(|n| format!(" · {}", t!("core.journal.export_affected", n = n))).unwrap_or_default();
            out.push_str(&format!(
                "### {} · {} · {}{}\n\n",
                t.get(11..).unwrap_or(""),
                e.source,
                status(e),
                affected
            ));
            let fence = if e.sql.contains("```") { "````" } else { "```" };
            let lang = if e.kind == "redis" { "" } else { "sql" };
            out.push_str(&format!("{fence}{lang}\n{}\n{fence}\n\n", e.sql.trim_end()));
            if let Some(err) = &e.error {
                out.push_str(&format!("> {}\n\n", err.replace('\n', " ")));
            }
        }
        out
    }
}

/// 以 0600 追加一整行并 fsync;文件末尾不是换行(上次写到一半)时先补一个。
fn append_line(path: &Path, line: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            let mut b = std::fs::DirBuilder::new();
            b.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                b.mode(0o700);
            }
            b.create(parent)?;
        }
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    // 文件可能是别的途径建的:确保仅本用户可读写
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    let len = f.metadata()?.len();
    let mut buf = Vec::with_capacity(line.len() + 1);
    if len > 0 {
        let mut last = [0u8; 1];
        f.seek(SeekFrom::Start(len - 1))?;
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            buf.push(b'\n');
        }
    }
    buf.extend_from_slice(line);
    f.write_all(&buf)?;
    f.sync_all()
}

fn read_entries(path: &Path) -> Vec<JournalEntry> {
    let Ok(bytes) = std::fs::read(path) else {
        return vec![];
    };
    bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_slice::<JournalEntry>(l).ok())
        .filter(|e| !e.id.is_empty())
        .collect()
}

/// 第一条记录的时间(只读第一行)。
fn first_ts(path: &Path) -> Option<i64> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    let mut r = std::io::BufReader::new(f);
    let mut line = Vec::new();
    // 第一行可能很长(带撤销数据),但总要读完才能解析
    r.read_until(b'\n', &mut line).ok()?;
    #[derive(Deserialize)]
    struct Ts {
        ts: i64,
    }
    serde_json::from_slice::<Ts>(&line).ok().map(|t| t.ts)
}

/// 重写:丢掉早于 `cutoff` 的,再从最旧的丢起,直到不超过 `target` 字节。
fn compact(path: &Path, cutoff: i64, target: u64) -> AppResult<()> {
    if !path.exists() {
        return Ok(());
    }
    let lines: Vec<Vec<u8>> = read_entries(path)
        .into_iter()
        .filter(|e| e.ts >= cutoff)
        .filter_map(|e| serde_json::to_vec(&e).ok())
        .collect();
    let mut total: u64 = lines.iter().map(|l| l.len() as u64 + 1).sum();
    let mut skip = 0;
    while total > target && skip < lines.len() {
        total -= lines[skip].len() as u64 + 1;
        skip += 1;
    }
    let mut out = Vec::with_capacity(total as usize);
    for l in &lines[skip..] {
        out.extend_from_slice(l);
        out.push(b'\n');
    }
    crate::core::fsutil::write_private(path, &out).map_err(io_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> (PathBuf, Journal) {
        let dir = std::env::temp_dir().join(format!("datum-journal-{tag}-{}", uuid::Uuid::new_v4()));
        (dir.clone(), Journal::new(&dir))
    }

    fn entry(conn: &str, sql: &str) -> JournalEntry {
        JournalEntry {
            conn_id: conn.into(),
            conn_name: "local".into(),
            env: "local".into(),
            kind: "sqlite".into(),
            source: "editor".into(),
            sql: sql.into(),
            ok: true,
            affected: Some(1),
            ..Default::default()
        }
    }

    fn undo_data(rows: usize) -> UndoData {
        UndoData {
            statements: vec!["UPDATE t SET a = '1' WHERE id = 1".into()],
            rows,
            snapshots: vec![UndoSnapshot { table: "t".into(), columns: vec!["id".into(), "a".into()], total: rows, ..Default::default() }],
        }
    }

    #[test]
    fn append_then_list_newest_first() {
        let (dir, j) = temp("order");
        let a = j.append(entry("c1", "UPDATE t SET a = 1")).unwrap().unwrap();
        let b = j.append(entry("c1", "DELETE FROM t")).unwrap().unwrap();
        j.append(entry("c2", "DELETE FROM other")).unwrap();
        let list = j.list("c1", &JournalFilter::default());
        assert_eq!(list.iter().map(|e| e.id.clone()).collect::<Vec<_>>(), vec![b, a]);
        assert!(!list[0].previous_session);
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, j) = temp("perm");
        j.append(entry("c1", "DELETE FROM t")).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&j.path("c1")), 0o600);
        assert_eq!(mode(&dir.join("journal")), 0o700);
        // 别的途径改宽了权限:下次追加收紧
        std::fs::set_permissions(j.path("c1"), std::fs::Permissions::from_mode(0o644)).unwrap();
        j.append(entry("c1", "DELETE FROM t")).unwrap();
        assert_eq!(mode(&j.path("c1")), 0o600);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn truncated_last_line_is_skipped_and_next_append_starts_fresh() {
        let (dir, j) = temp("trunc");
        j.append(entry("c1", "UPDATE t SET a = 1")).unwrap();
        // 模拟写到一半崩溃:末尾半行
        let mut f = std::fs::OpenOptions::new().append(true).open(j.path("c1")).unwrap();
        f.write_all(br#"{"id":"half","ts":1,"sql":"UPDA"#).unwrap();
        drop(f);
        assert_eq!(j.list("c1", &JournalFilter::default()).len(), 1);
        j.append(entry("c1", "DELETE FROM t")).unwrap();
        let list = j.list("c1", &JournalFilter::default());
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].sql, "DELETE FROM t");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn disabled_writes_nothing_and_rows_off_drops_undo() {
        let (dir, j) = temp("settings");
        j.save_settings(&JournalSettings { enabled: false, ..Default::default() }).unwrap();
        assert_eq!(j.append(entry("c1", "DELETE FROM t")).unwrap(), None);
        assert!(!j.path("c1").exists());

        j.save_settings(&JournalSettings { enabled: true, store_rows: false, retention_days: 90 }).unwrap();
        let mut e = entry("c1", "UPDATE t SET a = 2");
        e.undo = Some(undo_data(1));
        let id = j.append(e).unwrap().unwrap();
        let d = j.get("c1", &id).unwrap();
        assert!(d.entry.undo.is_none());
        assert!(d.entry.undo_reason.is_some());
        // 文件里也没有行数据
        let text = std::fs::read_to_string(j.path("c1")).unwrap();
        assert!(!text.contains("snapshots"), "{text}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn long_sql_is_capped_and_marked() {
        let (dir, j) = temp("cap");
        let big = format!("UPDATE t SET a = '{}'", "é".repeat(MAX_SQL_BYTES));
        let id = j.append(entry("c1", &big)).unwrap().unwrap();
        let d = j.get("c1", &id).unwrap();
        assert!(d.entry.truncated);
        assert!(d.entry.sql.len() <= MAX_SQL_BYTES);
        // 列表里只有开头
        assert!(j.list("c1", &JournalFilter::default())[0].sql.len() <= LIST_SQL_BYTES);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rotation_drops_old_entries_by_age() {
        let (dir, j) = temp("age");
        j.save_settings(&JournalSettings { retention_days: 10, ..Default::default() }).unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let mut old = entry("c1", "DELETE FROM old");
        old.ts = now - 11 * DAY_MS;
        old.id = "old".into();
        let mut recent = entry("c1", "DELETE FROM recent");
        recent.ts = now - 9 * DAY_MS;
        recent.id = "recent".into();
        // 直接写进文件(append 会立刻整理,先看读取时也会过滤)
        let lines = [serde_json::to_string(&old).unwrap(), serde_json::to_string(&recent).unwrap()];
        std::fs::create_dir_all(dir.join("journal")).unwrap();
        std::fs::write(j.path("c1"), lines.join("\n") + "\n").unwrap();
        assert_eq!(j.list("c1", &JournalFilter::default()).len(), 1);
        // 下一次追加发现最旧的超龄:重写文件
        j.append(entry("c1", "DELETE FROM new")).unwrap();
        let all = j.read_all("c1");
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|e| e.id != "old"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rotation_by_size_drops_oldest_first() {
        let (dir, j) = temp("size");
        let path = j.path("c1");
        std::fs::create_dir_all(dir.join("journal")).unwrap();
        let mut text = String::new();
        let now = chrono::Utc::now().timestamp_millis();
        for i in 0..10 {
            let mut e = entry("c1", &format!("UPDATE t SET a = '{}' -- {i}", "x".repeat(1000)));
            e.id = format!("e{i}");
            e.ts = now - (10 - i) * 1000;
            text.push_str(&serde_json::to_string(&e).unwrap());
            text.push('\n');
        }
        std::fs::write(&path, &text).unwrap();
        // 目标 5 KB:只留最新的几条
        compact(&path, 0, 5 * 1024).unwrap();
        let all = j.read_all("c1");
        assert!(!all.is_empty() && all.len() < 10, "{}", all.len());
        assert_eq!(all.last().unwrap().id, "e9");
        assert!(all.iter().all(|e| e.id != "e0"));
        assert!(std::fs::metadata(&path).unwrap().len() <= 5 * 1024);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn undo_records_mark_the_original_as_undone() {
        let (dir, j) = temp("undone");
        let mut e = entry("c1", "UPDATE t SET a = 2 WHERE id = 1");
        e.undo = Some(undo_data(1));
        let id = j.append(e).unwrap().unwrap();
        assert_eq!(j.undoable("c1"), vec![(id.clone(), 1)]);
        // 之后又改了同一张表
        j.append(entry("c1", "UPDATE t SET a = 3 WHERE id = 1")).unwrap();
        j.append(entry("c1", "UPDATE other SET a = 3")).unwrap();
        assert_eq!(j.get("c1", &id).unwrap().later_writes, 1);
        let mut u = entry("c1", "UPDATE t SET a = '1' WHERE id = 1");
        u.source = "undo".into();
        u.undoes = vec![id.clone()];
        let uid = j.append(u).unwrap().unwrap();
        let list = j.list("c1", &JournalFilter::default());
        let orig = list.iter().find(|x| x.id == id).unwrap();
        assert_eq!(orig.undone_by.as_deref(), Some(uid.as_str()));
        assert!(j.undoable("c1").is_empty());
        assert_eq!(j.get("c1", &id).unwrap().undone_by.as_deref(), Some(uid.as_str()));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn filters_and_export() {
        let (dir, j) = temp("filter");
        j.append(entry("c1", "UPDATE orders SET status = 'paid'")).unwrap();
        let mut failed = entry("c1", "DELETE FROM missing");
        failed.ok = false;
        failed.error = Some("no such table: missing".into());
        failed.affected = None;
        j.append(failed).unwrap();
        let mut redis = entry("c1", "SET k v");
        redis.source = "redis".into();
        j.append(redis).unwrap();
        let f = |source: Option<&str>, status: Option<&str>, text: Option<&str>| JournalFilter {
            source: source.map(String::from),
            status: status.map(String::from),
            text: text.map(String::from),
            ..Default::default()
        };
        assert_eq!(j.list("c1", &f(Some("redis"), None, None)).len(), 1);
        assert_eq!(j.list("c1", &f(None, Some("error"), None)).len(), 1);
        assert_eq!(j.list("c1", &f(None, Some("ok"), None)).len(), 2);
        assert_eq!(j.list("c1", &f(None, None, Some("ORDERS"))).len(), 1);
        assert_eq!(j.list("c1", &f(None, None, Some("no such table"))).len(), 1);
        let future = JournalFilter { from: Some(chrono::Utc::now().timestamp_millis() + DAY_MS), ..Default::default() };
        assert!(j.list("c1", &future).is_empty());

        let md = j.export("c1", &f(None, Some("ok"), None), "md");
        assert!(md.contains("UPDATE orders SET status = 'paid'"), "{md}");
        assert!(!md.contains("DELETE FROM missing"), "{md}");
        let csv = j.export("c1", &JournalFilter::default(), "csv");
        assert_eq!(csv.lines().count(), 4, "{csv}");
        assert!(csv.contains("no such table: missing"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn clear_and_odd_conn_ids() {
        let (dir, j) = temp("clear");
        j.append(entry("../evil", "DELETE FROM t")).unwrap();
        // 不会写到 journal 目录外面
        assert!(j.path("../evil").starts_with(dir.join("journal")));
        assert_eq!(j.read_all("../evil").len(), 1);
        j.clear("../evil").unwrap();
        assert!(j.read_all("../evil").is_empty());
        j.clear("never-existed").unwrap();
        std::fs::remove_dir_all(dir).ok();
    }
}
