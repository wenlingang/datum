//! 数据对比与同步:比较两个库里一张表的**数据**(同一种关系库),生成让目标表与源表一致的 SQL。
//!
//! 引擎:两边都按键(主键或用户选的唯一键)排序、键集分页(keyset)分块读取,在内存里归并:
//! - 只读会话 + 服务端限时(每块单独限时),PG 走辅助会话,不受编辑器会话影响;
//! - 文本键按字节序排(PG `COLLATE "C"`、MySQL 转成 utf8mb4 的二进制串、SQLite `COLLATE BINARY`),
//!   与这里的比较器一致;每读一行都核对是否严格递增,对不上(枚举、带时区的时间等排序规则不同)就报错停下,
//!   不会给出错的结果;
//! - 键列有 NULL 的行跳过(NULL 无法当键对齐);
//! - 值比较做归一:数值按数学值(`1.0` = `1`),二进制按字节,JSON 按结构,文本原样(尾随空格算差异),NULL 与空串不同;
//! - 每边最多读 `max_rows` 行(超出时只对比到较短一边读到的最后一个键为止,报告里标明);
//!   差异行只保留前 `max_diffs` 条,计数始终准确。
//!
//! 后台执行:[`Jobs::start`] 起任务,UI 轮询 [`Jobs::progress`],可随时 [`Jobs::cancel`](在块与块之间生效)。
//! 同步脚本 [`script`]:INSERT(只在源表)按批合并、UPDATE 只改变化的列、DELETE(只在目标表)默认注释掉。

use crate::core::connection::DbKind;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::pg::browse::TableDetail;
use crate::service;
use crate::sql::build::{ident, literal};
use crate::sql::Dialect;
use crate::AppState;
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

/// 每边默认最多读的行数。
pub const DEFAULT_MAX_ROWS: u64 = 1_000_000;
/// 默认最多保留的差异行数(计数不受限)。
pub const DEFAULT_MAX_DIFFS: usize = 10_000;
/// 每块读取的行数。
pub const DEFAULT_CHUNK: usize = 2000;
/// 每块查询的服务端限时。
pub const CHUNK_TIMEOUT_MS: u32 = 30_000;
/// 合并成一条 INSERT 的行数。
const INSERT_BATCH: usize = 100;

// ---------------------------------------------------------------- 输入

/// 对比的一边:连接 + 表名。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Side {
    pub conn_id: String,
    pub table: String,
}

fn default_max_rows() -> u64 {
    DEFAULT_MAX_ROWS
}
fn default_max_diffs() -> usize {
    DEFAULT_MAX_DIFFS
}
fn default_chunk() -> usize {
    DEFAULT_CHUNK
}

/// 一次对比的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spec {
    pub kind: DbKind,
    pub source: Side,
    pub target: Side,
    /// 键列;空表示用源表主键
    #[serde(default)]
    pub key: Vec<String>,
    /// 两边都加上的 WHERE 条件(不含 `WHERE`)
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default = "default_max_rows")]
    pub max_rows: u64,
    #[serde(default = "default_max_diffs")]
    pub max_diffs: usize,
    #[serde(default = "default_chunk")]
    pub chunk: usize,
}

impl Spec {
    pub fn new(kind: DbKind, source: Side, target: Side) -> Self {
        Self {
            kind,
            source,
            target,
            key: vec![],
            filter: None,
            max_rows: DEFAULT_MAX_ROWS,
            max_diffs: DEFAULT_MAX_DIFFS,
            chunk: DEFAULT_CHUNK,
        }
    }
}

// ---------------------------------------------------------------- 结构检查

/// 唯一索引(无主键时可选作键)。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UniqueKey {
    pub name: String,
    pub columns: Vec<String>,
}

/// 两张表的结构对照:可用的键与列。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    /// 源表主键(按主键内顺序)
    pub primary_key: Vec<String>,
    /// 目标表主键
    pub target_primary_key: Vec<String>,
    /// 源表的唯一索引(只含普通列,两边都有的列)
    pub unique_keys: Vec<UniqueKey>,
    /// 两边都有的列(源表顺序),只比较这些
    pub common: Vec<String>,
    pub only_source: Vec<String>,
    pub only_target: Vec<String>,
}

fn split_cols(s: &str) -> Vec<String> {
    s.split(',').map(|c| c.trim().trim_matches(|ch| ch == '"' || ch == '`').to_string()).filter(|c| !c.is_empty()).collect()
}

fn primary_key(d: &TableDetail) -> Vec<String> {
    if let Some(ix) = d.indexes.iter().find(|i| i.is_primary) {
        let cols = split_cols(&ix.columns);
        if !cols.is_empty() && cols.iter().all(|c| d.columns.iter().any(|x| &x.name == c)) {
            return cols;
        }
    }
    d.columns.iter().filter(|c| c.is_pk).map(|c| c.name.clone()).collect()
}

/// 由两边的表结构得出对照(纯函数)。
pub fn inspect_details(source: &TableDetail, target: &TableDetail) -> Inspection {
    let has = |d: &TableDetail, n: &str| d.columns.iter().any(|c| c.name == n);
    let common: Vec<String> = source.columns.iter().filter(|c| has(target, &c.name)).map(|c| c.name.clone()).collect();
    let only_source = source.columns.iter().filter(|c| !has(target, &c.name)).map(|c| c.name.clone()).collect();
    let only_target = target.columns.iter().filter(|c| !has(source, &c.name)).map(|c| c.name.clone()).collect();
    let mut unique_keys: Vec<UniqueKey> = vec![];
    for ix in source.indexes.iter().filter(|i| i.is_unique && !i.is_primary) {
        let cols = split_cols(&ix.columns);
        if cols.is_empty() || !cols.iter().all(|c| common.contains(c)) {
            continue;
        }
        if unique_keys.iter().any(|u| u.columns == cols) {
            continue;
        }
        unique_keys.push(UniqueKey { name: ix.name.clone(), columns: cols });
    }
    Inspection {
        primary_key: primary_key(source),
        target_primary_key: primary_key(target),
        unique_keys,
        common,
        only_source,
        only_target,
    }
}

/// 读两张表的结构并对照。
pub async fn inspect(state: &AppState, kind: &DbKind, source: &Side, target: &Side) -> AppResult<Inspection> {
    let (s, t) = details(state, kind, source, target).await?;
    Ok(inspect_details(&s, &t))
}

async fn details(state: &AppState, kind: &DbKind, source: &Side, target: &Side) -> AppResult<(TableDetail, TableDetail)> {
    let s = service::table_detail(state, kind, &source.conn_id, &source.table).await?;
    let t = service::table_detail(state, kind, &target.conn_id, &target.table).await?;
    if s.columns.is_empty() {
        return Err(AppError::new(ErrorKind::NotFound, t!("core.ddiff.no_table", table = source.table)));
    }
    if t.columns.is_empty() {
        return Err(AppError::new(ErrorKind::NotFound, t!("core.ddiff.no_table", table = target.table)));
    }
    Ok((s, t))
}

// ---------------------------------------------------------------- 列类别与归一

/// 列的比较方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ColClass {
    /// 数值:按数学值比较,脚本里不加引号
    Number,
    /// 二进制:读成十六进制,按字节比较,脚本里写成二进制字面量
    Binary,
    /// JSON:按结构比较
    Json,
    /// 其他(文本、日期时间、布尔……):原样比较
    Text,
}

fn is_number_type(t: &str, d: Dialect) -> bool {
    let t = t.trim().to_ascii_lowercase();
    if d == Dialect::Sqlite {
        // SQLite 的类型亲和性:含 INT / REAL / FLOA / DOUB / NUMERIC / DECIMAL
        return ["int", "real", "floa", "doub", "numeric", "decimal"].iter().any(|k| t.contains(k));
    }
    let base = t.split(|c: char| c == '(' || c == ' ').next().unwrap_or("");
    matches!(
        base,
        "smallint" | "integer" | "int" | "int2" | "int4" | "int8" | "bigint" | "tinyint" | "mediumint" | "numeric"
            | "decimal" | "real" | "double" | "float" | "float4" | "float8" | "smallserial" | "serial" | "bigserial"
            | "dec" | "fixed"
    ) || t.starts_with("double precision")
}

fn is_binary_type(t: &str, d: Dialect) -> bool {
    let t = t.trim().to_ascii_lowercase();
    match d {
        Dialect::Pg => t == "bytea",
        Dialect::Mysql => {
            let base = t.split(|c: char| c == '(' || c == ' ').next().unwrap_or("");
            matches!(base, "binary" | "varbinary" | "blob" | "tinyblob" | "mediumblob" | "longblob")
        }
        Dialect::Sqlite => t.contains("blob"),
    }
}

fn is_json_type(t: &str) -> bool {
    let t = t.trim().to_ascii_lowercase();
    t == "json" || t == "jsonb"
}

fn is_mysql_bit(t: &str) -> bool {
    let t = t.trim().to_ascii_lowercase();
    t == "bit" || t.starts_with("bit(")
}

/// 文本类(需要按字节序排序的键列)。
fn is_collatable(t: &str, d: Dialect) -> bool {
    let t = t.trim().to_ascii_lowercase();
    match d {
        Dialect::Pg => t == "text" || t.starts_with("character") || t.starts_with("varchar") || t.starts_with("char") || t == "name",
        Dialect::Mysql => {
            let base = t.split(|c: char| c == '(' || c == ' ').next().unwrap_or("");
            matches!(base, "char" | "varchar" | "text" | "tinytext" | "mediumtext" | "longtext" | "enum" | "set")
        }
        // SQLite:除了数值与二进制,都按文本比较(显式 COLLATE BINARY 去掉列上的 NOCASE 等)
        Dialect::Sqlite => !is_number_type(&t, d) && !t.contains("blob"),
    }
}

fn class_of(src: &str, dst: &str, d: Dialect) -> ColClass {
    if is_binary_type(src, d) || is_binary_type(dst, d) {
        ColClass::Binary
    } else if is_json_type(src) || is_json_type(dst) {
        ColClass::Json
    } else if is_number_type(src, d) || is_number_type(dst, d) || (d == Dialect::Mysql && (is_mysql_bit(src) || is_mysql_bit(dst))) {
        ColClass::Number
    } else {
        ColClass::Text
    }
}

/// 规范化的十进制数:值 = 0.d1d2d3… × 10^exp;零为 digits 空。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Num {
    neg: bool,
    digits: Vec<u8>,
    exp: i64,
}

impl Num {
    /// 解析十进制数文本(可带符号、小数点、指数);NaN / Infinity 等返回 None。
    pub fn parse(s: &str) -> Option<Num> {
        let s = s.trim();
        let (neg, rest) = match s.as_bytes().first()? {
            b'-' => (true, &s[1..]),
            b'+' => (false, &s[1..]),
            _ => (false, s),
        };
        let (mant, exp_part) = match rest.find(['e', 'E']) {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        let exp10: i64 = match exp_part {
            Some(e) => e.parse().ok()?,
            None => 0,
        };
        let (int, frac) = match mant.find('.') {
            Some(i) => (&mant[..i], &mant[i + 1..]),
            None => (mant, ""),
        };
        if int.is_empty() && frac.is_empty() {
            return None;
        }
        if !int.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let all: Vec<u8> = int.bytes().chain(frac.bytes()).map(|b| b - b'0').collect();
        let lead = all.iter().take_while(|&&d| d == 0).count();
        let mut digits: Vec<u8> = all[lead..].to_vec();
        while digits.last() == Some(&0) {
            digits.pop();
        }
        if digits.is_empty() {
            return Some(Num { neg: false, digits, exp: 0 });
        }
        let exp = int.len() as i64 - lead as i64 + exp10;
        Some(Num { neg, digits, exp })
    }

    fn cmp_abs(&self, o: &Num) -> Ordering {
        match (self.digits.is_empty(), o.digits.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        self.exp.cmp(&o.exp).then_with(|| self.digits.cmp(&o.digits))
    }
}

impl Ord for Num {
    fn cmp(&self, o: &Self) -> Ordering {
        match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => self.cmp_abs(o),
            (true, true) => o.cmp_abs(self),
        }
    }
}

impl PartialOrd for Num {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// 只由数字、可选符号与小数点组成(可以不加引号写进 SQL)。
fn plain_number(s: &str) -> bool {
    let b = s.strip_prefix('-').unwrap_or(s);
    !b.is_empty() && b.bytes().all(|c| c.is_ascii_digit() || c == b'.') && b.bytes().filter(|&c| c == b'.').count() <= 1 && b != "."
}

fn json_eq(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value as J;
    match (a, b) {
        (J::Number(x), J::Number(y)) => match (Num::parse(&x.to_string()), Num::parse(&y.to_string())) {
            (Some(x), Some(y)) => x == y,
            _ => x == y,
        },
        (J::Array(x), J::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q)),
        (J::Object(x), J::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w))),
        _ => a == b,
    }
}

/// 一个单元格两边的值是否相同(按列类别归一)。
pub fn same_value(class: ColClass, a: Option<&str>, b: Option<&str>) -> bool {
    let (a, b) = match (a, b) {
        (None, None) => return true,
        (Some(a), Some(b)) => (a, b),
        _ => return false,
    };
    if a == b {
        return true;
    }
    match class {
        ColClass::Number => match (Num::parse(a), Num::parse(b)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
        ColClass::Binary => a.eq_ignore_ascii_case(b),
        ColClass::Json => match (serde_json::from_str::<serde_json::Value>(a), serde_json::from_str::<serde_json::Value>(b)) {
            (Ok(x), Ok(y)) => json_eq(&x, &y),
            _ => false,
        },
        ColClass::Text => false,
    }
}

/// 键值的排序:数值键按数学值,其余按字节序(与 SQL 里的排序规则一致);
/// 数值列里混进不是数字的值时(SQLite 动态类型)数字排在文本前,同 SQLite。
fn cmp_key_part(class: ColClass, a: &str, b: &str) -> Ordering {
    match class {
        ColClass::Number => match (Num::parse(a), Num::parse(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => a.as_bytes().cmp(b.as_bytes()),
        },
        ColClass::Binary => a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()),
        _ => a.as_bytes().cmp(b.as_bytes()),
    }
}

// ---------------------------------------------------------------- SQL

/// 参与对比的一列。
#[derive(Debug, Clone)]
struct Col {
    name: String,
    class: ColClass,
    /// 源表 / 目标表上用作排序键时要不要按字节序
    collate: [bool; 2],
}

fn select_expr(c: &Col, d: Dialect, col_type: &str) -> String {
    let q = ident(&c.name, d);
    match (c.class, d) {
        (ColClass::Binary, Dialect::Pg) => format!("encode({q}, 'hex')"),
        (ColClass::Binary, Dialect::Mysql) => format!("HEX({q})"),
        (ColClass::Binary, Dialect::Sqlite) => format!("hex({q})"),
        (ColClass::Number, Dialect::Mysql) if is_mysql_bit(col_type) => format!("CAST({q} AS UNSIGNED)"),
        _ => q,
    }
}

/// 排序 / 键集条件里用的键表达式。
fn key_expr(c: &Col, d: Dialect, side: usize) -> String {
    let q = ident(&c.name, d);
    if !c.collate[side] {
        return q;
    }
    match d {
        Dialect::Pg => format!("{q} COLLATE \"C\""),
        Dialect::Mysql => format!("CAST(CONVERT({q} USING utf8mb4) AS BINARY)"),
        Dialect::Sqlite => format!("{q} COLLATE BINARY"),
    }
}

/// 值写进 SQL 的样子。
fn value_sql(class: ColClass, v: Option<&str>, d: Dialect) -> String {
    let Some(v) = v else { return "NULL".into() };
    match class {
        ColClass::Binary => {
            let hex: String = v.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_ascii_uppercase();
            match d {
                Dialect::Pg => format!("'\\x{hex}'::bytea"),
                _ => format!("X'{hex}'"),
            }
        }
        ColClass::Number if plain_number(v) => v.to_string(),
        _ => literal(v, d),
    }
}

/// 一边的分块查询(键集分页)。
fn chunk_sql(
    d: Dialect,
    table: &str,
    cols: &[Col],
    types: &[String],
    key_idx: &[usize],
    side: usize,
    filter: Option<&str>,
    after: Option<&[Option<String>]>,
    limit: usize,
) -> String {
    let select: Vec<String> = cols.iter().zip(types).map(|(c, t)| select_expr(c, d, t)).collect();
    let mut conds: Vec<String> = vec![];
    if let Some(f) = filter {
        conds.push(format!("({f})"));
    }
    for &k in key_idx {
        conds.push(format!("{} IS NOT NULL", ident(&cols[k].name, d)));
    }
    if let Some(last) = after {
        // (k1, k2, …) > (v1, v2, …) 展开成 OR 链,三种库都能用
        let mut ors: Vec<String> = vec![];
        for i in 0..key_idx.len() {
            let mut ands: Vec<String> = vec![];
            for (j, &k) in key_idx.iter().enumerate().take(i + 1) {
                let c = &cols[k];
                let lit = value_sql(c.class, last[j].as_deref(), d);
                let op = if j == i { ">" } else { "=" };
                ands.push(format!("{} {op} {lit}", key_expr(c, d, side)));
            }
            ors.push(format!("({})", ands.join(" AND ")));
        }
        conds.push(format!("({})", ors.join(" OR ")));
    }
    let order: Vec<String> = key_idx.iter().map(|&k| key_expr(&cols[k], d, side)).collect();
    let mut sql = format!("SELECT {} FROM {}", select.join(", "), ident(table, d));
    if !conds.is_empty() {
        sql.push_str(&format!(" WHERE {}", conds.join(" AND ")));
    }
    sql.push_str(&format!(" ORDER BY {} LIMIT {limit}", order.join(", ")));
    sql
}

// ---------------------------------------------------------------- 结果

/// 差异类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffKind {
    /// 只在源表(同步时 INSERT)
    OnlySource,
    /// 只在目标表(同步时 DELETE)
    OnlyTarget,
    /// 两边都有但值不同(同步时 UPDATE)
    Different,
}

/// 一条差异行;值与 [`Report::columns`] 对齐(二进制列为十六进制)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffRow {
    pub kind: DiffKind,
    pub key: Vec<Option<String>>,
    pub source: Option<Vec<Option<String>>>,
    pub target: Option<Vec<Option<String>>>,
    /// 值不同的列下标(Different 才有)
    pub changed: Vec<usize>,
}

/// 对比报告。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub source_table: String,
    pub target_table: String,
    /// 参与比较的列(两边都有的列,源表顺序)
    pub columns: Vec<String>,
    pub classes: Vec<Option<ColClass>>,
    pub key: Vec<String>,
    pub only_source_columns: Vec<String>,
    pub only_target_columns: Vec<String>,
    pub identical: u64,
    pub only_source: u64,
    pub only_target: u64,
    pub different: u64,
    pub source_rows: u64,
    pub target_rows: u64,
    /// 差异行(最多 max_diffs 条)
    pub rows: Vec<DiffRow>,
    /// 差异总数超过了保留上限
    pub truncated: bool,
    /// 有一边读到了行数上限,只对比了前面一段
    pub capped: bool,
    pub max_rows: u64,
    pub cancelled: bool,
    #[serde(skip)]
    pub dialect: Option<Dialect>,
}

impl Report {
    pub fn total_diffs(&self) -> u64 {
        self.only_source + self.only_target + self.different
    }
}

/// 后台任务的进度。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub source_rows: u64,
    pub target_rows: u64,
    pub done: bool,
    pub cancelled: bool,
    pub error: Option<String>,
    /// 完成后的报告
    pub report: Option<Report>,
}

/// 一次对比任务(可在别的线程取消、读进度)。
#[derive(Default)]
pub struct Job {
    cancel: AtomicBool,
    source_rows: AtomicU64,
    target_rows: AtomicU64,
    state: Mutex<Progress>,
}

impl Job {
    pub fn cancel(&self) {
        self.cancel.store(true, AtomicOrdering::Relaxed);
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(AtomicOrdering::Relaxed)
    }
    pub fn progress(&self) -> Progress {
        let mut p = self.state.lock().unwrap().clone();
        p.source_rows = self.source_rows.load(AtomicOrdering::Relaxed);
        p.target_rows = self.target_rows.load(AtomicOrdering::Relaxed);
        p
    }
    fn finish(&self, r: AppResult<Report>) {
        let mut p = self.state.lock().unwrap();
        p.done = true;
        match r {
            Ok(rep) => {
                p.cancelled = rep.cancelled;
                p.report = Some(rep);
            }
            Err(e) => p.error = Some(e.display()),
        }
    }
}

/// 进行中 / 已完成的对比任务(按任务 id)。
#[derive(Default)]
pub struct Jobs {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
}

impl Jobs {
    /// 后台开始对比,返回任务 id(需在 tokio 运行时里调用)。
    pub fn start(&self, state: Arc<AppState>, spec: Spec) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let job = Arc::new(Job::default());
        self.jobs.lock().unwrap().insert(id.clone(), job.clone());
        tokio::spawn(async move {
            let r = run(&state, &spec, &job).await;
            job.finish(r);
        });
        id
    }
    pub fn progress(&self, id: &str) -> Option<Progress> {
        self.jobs.lock().unwrap().get(id).map(|j| j.progress())
    }
    pub fn cancel(&self, id: &str) {
        if let Some(j) = self.jobs.lock().unwrap().get(id) {
            j.cancel();
        }
    }
    /// 丢弃任务(进行中的会先取消)。
    pub fn close(&self, id: &str) {
        if let Some(j) = self.jobs.lock().unwrap().remove(id) {
            j.cancel();
        }
    }
    /// 已完成任务的同步脚本。
    pub fn script(&self, id: &str, include_deletes: bool) -> AppResult<String> {
        let job = self.jobs.lock().unwrap().get(id).cloned();
        let rep = job.and_then(|j| j.state.lock().unwrap().report.clone());
        let rep = rep.ok_or_else(|| AppError::new(ErrorKind::NotFound, t!("core.ddiff.no_report")))?;
        Ok(script(&rep, include_deletes))
    }
}

// ---------------------------------------------------------------- 引擎

/// 一边的读取游标。
struct Cursor {
    side: usize,
    buf: VecDeque<Vec<Option<String>>>,
    last: Option<Vec<Option<String>>>,
    read: u64,
    done: bool,
    capped: bool,
}

impl Cursor {
    fn new(side: usize) -> Self {
        Self { side, buf: VecDeque::new(), last: None, read: 0, done: false, capped: false }
    }
}

struct Ctx<'a> {
    state: &'a AppState,
    kind: DbKind,
    d: Dialect,
    cols: Vec<Col>,
    types: [Vec<String>; 2],
    key_idx: Vec<usize>,
    tables: [String; 2],
    conns: [String; 2],
    filter: Option<String>,
    chunk: usize,
    max_rows: u64,
}

fn cmp_keys(ctx: &Ctx, a: &[Option<String>], b: &[Option<String>]) -> Ordering {
    for &k in &ctx.key_idx {
        let o = cmp_key_part(ctx.cols[k].class, a[k].as_deref().unwrap_or(""), b[k].as_deref().unwrap_or(""));
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

fn key_of(ctx: &Ctx, row: &[Option<String>]) -> Vec<Option<String>> {
    ctx.key_idx.iter().map(|&k| row[k].clone()).collect()
}

async fn refill(ctx: &Ctx<'_>, c: &mut Cursor, job: &Job) -> AppResult<()> {
    if !c.buf.is_empty() || c.done {
        return Ok(());
    }
    let remaining = ctx.max_rows.saturating_sub(c.read);
    if remaining == 0 {
        c.done = true;
        c.capped = true;
        return Ok(());
    }
    let limit = (ctx.chunk as u64).min(remaining) as usize;
    let after: Option<Vec<Option<String>>> = c.last.as_ref().map(|r| key_of(ctx, r));
    let sql = chunk_sql(
        ctx.d,
        &ctx.tables[c.side],
        &ctx.cols,
        &ctx.types[c.side],
        &ctx.key_idx,
        c.side,
        ctx.filter.as_deref(),
        after.as_deref(),
        limit,
    );
    let r = service::query_meta_read_only_timeout(ctx.state, &ctx.kind, &ctx.conns[c.side], &sql, limit as u32, CHUNK_TIMEOUT_MS)
        .await
        .map_err(|e| {
            let which = if c.side == 0 { t!("core.ddiff.source") } else { t!("core.ddiff.target") };
            AppError::new(ErrorKind::Query, t!("core.ddiff.read_failed", side = which)).with_detail(e.display())
        })?;
    let n = r.rows.len();
    for row in r.rows {
        if row.len() != ctx.cols.len() {
            return Err(AppError::new(ErrorKind::Query, t!("core.err.query_failed")));
        }
        // 每一行都必须严格大于上一行:排序规则对不上或键不唯一时停下,不给出错的结果
        if let Some(prev) = c.buf.back().or(c.last.as_ref()) {
            match cmp_keys(ctx, prev, &row) {
                Ordering::Less => {}
                Ordering::Equal => return Err(AppError::new(ErrorKind::Query, t!("core.ddiff.dup_key"))),
                Ordering::Greater => return Err(AppError::new(ErrorKind::Query, t!("core.ddiff.order_mismatch"))),
            }
        }
        c.buf.push_back(row);
    }
    c.read += n as u64;
    if c.side == 0 {
        job.source_rows.store(c.read, AtomicOrdering::Relaxed);
    } else {
        job.target_rows.store(c.read, AtomicOrdering::Relaxed);
    }
    if n < limit {
        c.done = true;
    } else if c.read >= ctx.max_rows {
        // 读满上限:不知道后面还有没有,按读到上限处理
        c.done = true;
        c.capped = true;
    }
    Ok(())
}

fn pop(c: &mut Cursor) -> Vec<Option<String>> {
    let row = c.buf.pop_front().expect("buffer not empty");
    c.last = Some(row.clone());
    row
}

/// 执行一次对比(在调用方的任务里;`job` 用来报进度与取消)。
pub async fn run(state: &AppState, spec: &Spec, job: &Job) -> AppResult<Report> {
    let d = Dialect::from_kind(&spec.kind).ok_or_else(|| AppError::new(ErrorKind::Query, t!("core.err.redis_unsupported")))?;
    if let Some(f) = &spec.filter {
        if f.contains(';') {
            return Err(AppError::new(ErrorKind::Query, t!("core.ddiff.bad_filter")));
        }
    }
    let (sd, td) = details(state, &spec.kind, &spec.source, &spec.target).await?;
    let ins = inspect_details(&sd, &td);
    let key: Vec<String> = if spec.key.is_empty() { ins.primary_key.clone() } else { spec.key.clone() };
    if key.is_empty() {
        return Err(AppError::new(ErrorKind::Query, t!("core.ddiff.no_key")));
    }
    if let Some(k) = key.iter().find(|k| !ins.common.contains(k)) {
        return Err(AppError::new(ErrorKind::Query, t!("core.ddiff.key_missing", column = k)));
    }
    let type_of = |td: &TableDetail, n: &str| td.columns.iter().find(|c| c.name == n).map(|c| c.data_type.clone()).unwrap_or_default();
    let cols: Vec<Col> = ins
        .common
        .iter()
        .map(|n| {
            let (st, tt) = (type_of(&sd, n), type_of(&td, n));
            let class = class_of(&st, &tt, d);
            let is_key = key.contains(n);
            let coll = |t: &str| is_key && class == ColClass::Text && is_collatable(t, d);
            Col { name: n.clone(), class, collate: [coll(&st), coll(&tt)] }
        })
        .collect();
    let types = [
        cols.iter().map(|c| type_of(&sd, &c.name)).collect::<Vec<_>>(),
        cols.iter().map(|c| type_of(&td, &c.name)).collect::<Vec<_>>(),
    ];
    let key_idx: Vec<usize> = key.iter().map(|k| ins.common.iter().position(|c| c == k).expect("checked")).collect();
    let ctx = Ctx {
        state,
        kind: spec.kind,
        d,
        cols,
        types,
        key_idx,
        tables: [spec.source.table.clone(), spec.target.table.clone()],
        conns: [spec.source.conn_id.clone(), spec.target.conn_id.clone()],
        filter: spec.filter.as_ref().map(|f| f.trim().to_string()).filter(|f| !f.is_empty()),
        chunk: spec.chunk.max(1),
        max_rows: spec.max_rows.max(1),
    };
    let mut rep = Report {
        source_table: spec.source.table.clone(),
        target_table: spec.target.table.clone(),
        columns: ins.common.clone(),
        classes: ctx.cols.iter().map(|c| Some(c.class)).collect(),
        key: key.clone(),
        only_source_columns: ins.only_source.clone(),
        only_target_columns: ins.only_target.clone(),
        max_rows: ctx.max_rows,
        dialect: Some(d),
        ..Default::default()
    };
    let max_diffs = spec.max_diffs;
    let keep = |rep: &mut Report, row: DiffRow| {
        if rep.rows.len() < max_diffs {
            rep.rows.push(row);
        } else {
            rep.truncated = true;
        }
    };
    let (mut s, mut t) = (Cursor::new(0), Cursor::new(1));
    loop {
        if job.cancelled() {
            rep.cancelled = true;
            break;
        }
        if (s.buf.is_empty() && !s.done) && (t.buf.is_empty() && !t.done) {
            let (a, b) = tokio::join!(refill(&ctx, &mut s, job), refill(&ctx, &mut t, job));
            a?;
            b?;
        } else {
            refill(&ctx, &mut s, job).await?;
            refill(&ctx, &mut t, job).await?;
        }
        // 某一边读到上限、缓冲也用完了:后面的行没法对齐,停在这里
        if (s.capped && s.buf.is_empty()) || (t.capped && t.buf.is_empty()) {
            rep.capped = true;
            break;
        }
        // 一次处理到某一边缓冲用完为止,再回去补
        loop {
            let ord = match (s.buf.front(), t.buf.front()) {
                (None, None) => break,
                // 另一边缓冲空了:还能再读就先回去补;读到上限就停(后面的行没法对齐)
                (Some(_), None) if !t.done || t.capped => break,
                (None, Some(_)) if !s.done || s.capped => break,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(a), Some(b)) => cmp_keys(&ctx, a, b),
            };
            match ord {
                Ordering::Less => {
                    let row = pop(&mut s);
                    rep.only_source += 1;
                    let key = key_of(&ctx, &row);
                    keep(&mut rep, DiffRow { kind: DiffKind::OnlySource, key, source: Some(row), target: None, changed: vec![] });
                }
                Ordering::Greater => {
                    let row = pop(&mut t);
                    rep.only_target += 1;
                    let key = key_of(&ctx, &row);
                    keep(&mut rep, DiffRow { kind: DiffKind::OnlyTarget, key, source: None, target: Some(row), changed: vec![] });
                }
                Ordering::Equal => {
                    let (a, b) = (pop(&mut s), pop(&mut t));
                    let changed: Vec<usize> = (0..ctx.cols.len())
                        .filter(|&i| !same_value(ctx.cols[i].class, a[i].as_deref(), b[i].as_deref()))
                        .collect();
                    if changed.is_empty() {
                        rep.identical += 1;
                    } else {
                        rep.different += 1;
                        let key = key_of(&ctx, &a);
                        keep(&mut rep, DiffRow { kind: DiffKind::Different, key, source: Some(a), target: Some(b), changed });
                    }
                }
            }
        }
        if s.done && t.done && s.buf.is_empty() && t.buf.is_empty() {
            rep.capped = s.capped || t.capped;
            break;
        }
    }
    rep.source_rows = s.read;
    rep.target_rows = t.read;
    Ok(rep)
}

// ---------------------------------------------------------------- 同步脚本

fn comment_text(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// 让目标表与源表一致的脚本:DELETE(默认注释)→ UPDATE → INSERT。
pub fn script(rep: &Report, include_deletes: bool) -> String {
    let d = rep.dialect.unwrap_or(Dialect::Sqlite);
    let table = ident(&rep.target_table, d);
    let class = |i: usize| rep.classes.get(i).copied().flatten().unwrap_or(ColClass::Text);
    let key_idx: Vec<usize> = rep.key.iter().filter_map(|k| rep.columns.iter().position(|c| c == k)).collect();
    let where_key = |row: &[Option<String>]| -> String {
        key_idx
            .iter()
            .map(|&k| format!("{} = {}", ident(&rep.columns[k], d), value_sql(class(k), row[k].as_deref(), d)))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let mut out: Vec<String> = vec![format!(
        "-- {}",
        comment_text(&t!("core.ddiff.header", source = rep.source_table, target = rep.target_table))
    )];
    if rep.total_diffs() == 0 {
        out.push(format!("-- {}", t!("core.ddiff.identical")));
        return out.join("\n") + "\n";
    }
    if rep.truncated {
        out.push(format!("-- {}", t!("core.ddiff.script_truncated", kept = rep.rows.len(), total = rep.total_diffs())));
    }
    if rep.capped {
        out.push(format!("-- {}", t!("core.ddiff.script_capped", n = rep.max_rows)));
    }
    if !rep.only_target_columns.is_empty() {
        out.push(format!("-- {}", comment_text(&t!("core.ddiff.script_target_cols", cols = rep.only_target_columns.join(", ")))));
    }

    let deletes: Vec<&DiffRow> = rep.rows.iter().filter(|r| r.kind == DiffKind::OnlyTarget).collect();
    if !deletes.is_empty() {
        out.push(String::new());
        if include_deletes {
            out.push(format!("-- {}", t!("core.ddiff.section_delete", n = deletes.len())));
        } else {
            out.push(format!("-- {}", t!("core.ddiff.section_delete_commented", n = deletes.len())));
        }
        for r in deletes {
            let stmt = format!("DELETE FROM {table} WHERE {};", where_key(r.target.as_deref().unwrap_or(&[])));
            out.push(if include_deletes { stmt } else { format!("-- {stmt}") });
        }
    }

    let updates: Vec<&DiffRow> = rep.rows.iter().filter(|r| r.kind == DiffKind::Different).collect();
    if !updates.is_empty() {
        out.push(String::new());
        out.push(format!("-- {}", t!("core.ddiff.section_update", n = updates.len())));
        for r in updates {
            let src = r.source.as_deref().unwrap_or(&[]);
            let sets: Vec<String> = r
                .changed
                .iter()
                .map(|&i| format!("{} = {}", ident(&rep.columns[i], d), value_sql(class(i), src[i].as_deref(), d)))
                .collect();
            // 键用目标表那一行的值(数值键两边写法可能不同,如 1.0 / 1)
            let key_row = r.target.as_deref().unwrap_or(src);
            out.push(format!("UPDATE {table} SET {} WHERE {};", sets.join(", "), where_key(key_row)));
        }
    }

    let inserts: Vec<&DiffRow> = rep.rows.iter().filter(|r| r.kind == DiffKind::OnlySource).collect();
    if !inserts.is_empty() {
        out.push(String::new());
        out.push(format!("-- {}", t!("core.ddiff.section_insert", n = inserts.len())));
        let cols: Vec<String> = rep.columns.iter().map(|c| ident(c, d)).collect();
        for batch in inserts.chunks(INSERT_BATCH) {
            let values: Vec<String> = batch
                .iter()
                .map(|r| {
                    let src = r.source.as_deref().unwrap_or(&[]);
                    let vals: Vec<String> = (0..rep.columns.len()).map(|i| value_sql(class(i), src.get(i).cloned().flatten().as_deref(), d)).collect();
                    format!("  ({})", vals.join(", "))
                })
                .collect();
            out.push(format!("INSERT INTO {table} ({}) VALUES\n{};", cols.join(", "), values.join(",\n")));
        }
    }
    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_parse_and_compare() {
        assert_eq!(Num::parse("1.0"), Num::parse("1"));
        assert_eq!(Num::parse("1.50"), Num::parse("1.5"));
        assert_eq!(Num::parse("-0"), Num::parse("0.000"));
        assert_eq!(Num::parse("1e3"), Num::parse("1000"));
        assert_eq!(Num::parse("0.001"), Num::parse("1E-3"));
        assert!(Num::parse("NaN").is_none());
        assert!(Num::parse("1.2.3").is_none());
        assert!(Num::parse("").is_none());
        let n = |s: &str| Num::parse(s).unwrap();
        assert!(n("9007199254740993") > n("9007199254740992"));
        assert!(n("-5") < n("-4"));
        assert!(n("-0.5") < n("0"));
        assert!(n("10") > n("9.99"));
        assert!(n("0.1") < n("0.11"));
        assert!(n("100") > n("99"));
    }

    #[test]
    fn values_normalize_by_class() {
        assert!(same_value(ColClass::Number, Some("1.0"), Some("1")));
        assert!(!same_value(ColClass::Number, Some("1.01"), Some("1")));
        assert!(!same_value(ColClass::Text, Some("a "), Some("a")));
        assert!(!same_value(ColClass::Text, Some(""), None));
        assert!(same_value(ColClass::Binary, Some("abff"), Some("ABFF")));
        assert!(same_value(ColClass::Json, Some(r#"{"a":1,"b":[1,2]}"#), Some(r#"{"b": [1, 2], "a": 1.0}"#)));
        assert!(!same_value(ColClass::Json, Some(r#"{"a":1}"#), Some(r#"{"a":"1"}"#)));
    }

    #[test]
    fn keys_sort_like_the_database() {
        assert_eq!(cmp_key_part(ColClass::Number, "9", "10"), Ordering::Less);
        assert_eq!(cmp_key_part(ColClass::Text, "B", "a"), Ordering::Less);
        assert_eq!(cmp_key_part(ColClass::Number, "5", "abc"), Ordering::Less);
    }

    #[test]
    fn literals_per_dialect() {
        assert_eq!(value_sql(ColClass::Binary, Some("00ff"), Dialect::Pg), "'\\x00FF'::bytea");
        assert_eq!(value_sql(ColClass::Binary, Some("00ff"), Dialect::Mysql), "X'00FF'");
        assert_eq!(value_sql(ColClass::Number, Some("-1.5"), Dialect::Mysql), "-1.5");
        assert_eq!(value_sql(ColClass::Number, Some("NaN"), Dialect::Pg), "'NaN'");
        assert_eq!(value_sql(ColClass::Text, Some("it's"), Dialect::Sqlite), "'it''s'");
        assert_eq!(value_sql(ColClass::Text, None, Dialect::Sqlite), "NULL");
        assert_eq!(value_sql(ColClass::Text, Some("a\\b"), Dialect::Mysql), "'a\\\\b'");
    }

    #[test]
    fn keyset_sql_expands_composite_keys() {
        let cols = vec![
            Col { name: "a".into(), class: ColClass::Number, collate: [false, false] },
            Col { name: "b".into(), class: ColClass::Text, collate: [true, true] },
            Col { name: "v".into(), class: ColClass::Binary, collate: [false, false] },
        ];
        let types = vec!["integer".to_string(), "text".to_string(), "bytea".to_string()];
        let last = vec![Some("1".to_string()), Some("x".to_string())];
        let sql = chunk_sql(Dialect::Pg, "t", &cols, &types, &[0, 1], 0, Some("a > 0"), Some(&last), 50);
        assert_eq!(
            sql,
            "SELECT \"a\", \"b\", encode(\"v\", 'hex') FROM \"t\" WHERE (a > 0) AND \"a\" IS NOT NULL AND \"b\" IS NOT NULL \
             AND ((\"a\" > 1) OR (\"a\" = 1 AND \"b\" COLLATE \"C\" > 'x')) ORDER BY \"a\", \"b\" COLLATE \"C\" LIMIT 50"
        );
    }

    #[test]
    fn classes() {
        assert_eq!(class_of("numeric(10,2)", "numeric(10,2)", Dialect::Pg), ColClass::Number);
        assert_eq!(class_of("double precision", "double precision", Dialect::Pg), ColClass::Number);
        assert_eq!(class_of("bytea", "bytea", Dialect::Pg), ColClass::Binary);
        assert_eq!(class_of("jsonb", "json", Dialect::Pg), ColClass::Json);
        assert_eq!(class_of("varbinary(16)", "varbinary(16)", Dialect::Mysql), ColClass::Binary);
        assert_eq!(class_of("bit(1)", "bit(1)", Dialect::Mysql), ColClass::Number);
        assert_eq!(class_of("int unsigned", "int", Dialect::Mysql), ColClass::Number);
        assert_eq!(class_of("BLOB", "BLOB", Dialect::Sqlite), ColClass::Binary);
        assert_eq!(class_of("VARCHAR(10)", "TEXT", Dialect::Sqlite), ColClass::Text);
        assert_eq!(class_of("timestamp without time zone", "timestamp without time zone", Dialect::Pg), ColClass::Text);
    }
}
