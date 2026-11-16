//! 索引建议:从一条慢查询出发提出候选索引,并在建任何东西之前证明它们有用。
//!
//! 1. 规则([`rules`]):解析语句([`shape`]),按等值 → 范围 → 排序的顺序拼复合索引,去掉已有索引能覆盖的。
//! 2. AI(可选,[`ai`]):把 SQL、表结构与当前执行计划(不含行数据;生产库未允许发数据时字面量打码)
//!    交给模型,回来的表名 / 列名在本地逐个校验,与规则候选合并并标明来源。
//! 3. 验证([`verify`]),全程只读或回滚、服务端限时,不在库里留下任何对象:
//!    - PG:装了 hypopg 时在只读事务里建「假想索引」,比较前后执行计划的总成本、是否用上索引;
//!    - SQLite:连接可写、表不大时 `BEGIN; CREATE INDEX; EXPLAIN QUERY PLAN; ROLLBACK`;
//!    - MySQL / MariaDB 没有安全的假想索引:只给规则候选,标「未验证」,仍给出基线计划(标出全表扫描)。
//!
//! 真正建索引只经编辑器:「在编辑器中打开」生成 `CREATE INDEX CONCURRENTLY`(PG)/
//! `ALTER TABLE … ADD INDEX …, ALGORITHM=INPLACE, LOCK=NONE`(MySQL),执行时照常过风险审查。

pub mod ai;
pub mod rules;
pub mod shape;
mod verify;

use crate::core::connection::DbKind;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::explain::Plan;
use crate::service;
use crate::sql::Dialect;
use crate::AppState;
use rules::TableMeta;
use rust_i18n::t;
use serde::Serialize;
use shape::{Shape, TableRef};
use std::collections::HashMap;

/// SQLite 用真实索引验证时,表的行数上限(建索引要扫全表、持写锁)。
pub const SQLITE_MAX_ROWS: u64 = 200_000;
/// 验证里每条语句的限时。
pub const VERIFY_TIMEOUT_MS: u32 = 10_000;

/// 索引的一列。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IndexCol {
    pub name: String,
    pub desc: bool,
}

/// 候选的来源。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Rule,
    Ai,
}

/// 一个候选索引。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// 真实表名
    pub table: String,
    /// 语句里的表名原文(带 schema 与引号),DDL 里照用
    pub table_sql: String,
    pub columns: Vec<IndexCol>,
    pub reason: String,
    pub source: Source,
    /// 建议的索引名
    pub name: String,
    /// 普通的建索引语句(展示与复制)
    pub ddl: String,
    /// 放进编辑器的语句:PG 用 CONCURRENTLY,MySQL 用在线 DDL
    pub editor_sql: String,
}

impl Candidate {
    pub fn new(t: &TableRef, columns: Vec<IndexCol>, reason: String, source: Source, d: Dialect) -> Self {
        let raw_cols: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
        let name = index_name(&t.name, &raw_cols);
        let mut c = Candidate {
            table: t.name.clone(),
            table_sql: t.raw.clone(),
            columns,
            reason,
            source,
            name,
            ddl: String::new(),
            editor_sql: String::new(),
        };
        let name = ident_min(&c.name, d);
        let cols = c.col_list(d);
        c.ddl = format!("CREATE INDEX {name} ON {} ({cols});", c.table_sql);
        c.editor_sql = match d {
            Dialect::Pg => format!("CREATE INDEX CONCURRENTLY {name} ON {} ({cols});", c.table_sql),
            Dialect::Mysql => format!("ALTER TABLE {} ADD INDEX {name} ({cols}), ALGORITHM=INPLACE, LOCK=NONE;", c.table_sql),
            Dialect::Sqlite => c.ddl.clone(),
        };
        c
    }

    pub fn cols(&self) -> Vec<String> {
        self.columns.iter().map(|c| c.name.clone()).collect()
    }

    /// `a, b DESC`
    pub fn col_list(&self, d: Dialect) -> String {
        self.columns
            .iter()
            .map(|c| format!("{}{}", ident_min(&c.name, d), if c.desc { " DESC" } else { "" }))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// `idx_表_列…`:只留 ASCII 字母数字,其余换成下划线,最长 60 个字符。
fn index_name(table: &str, cols: &[&str]) -> String {
    let raw = format!("idx_{}_{}", table, cols.join("_")).to_lowercase();
    let mut out = String::new();
    for ch in raw.chars() {
        let c = if ch.is_ascii_alphanumeric() { ch } else { '_' };
        if c == '_' && out.ends_with('_') {
            continue;
        }
        out.push(c);
    }
    let mut out = out.trim_end_matches('_').to_string();
    out.truncate(60);
    out.trim_end_matches('_').to_string()
}

const RESERVED: &[&str] = &[
    "all", "analyse", "analyze", "and", "any", "as", "asc", "both", "by", "case", "check", "column", "constraint",
    "create", "cross", "current_date", "current_time", "current_timestamp", "current_user", "default", "desc",
    "distinct", "do", "else", "end", "except", "false", "fetch", "for", "foreign", "from", "full", "grant", "group",
    "having", "in", "index", "inner", "intersect", "interval", "into", "is", "join", "key", "keys", "leading", "left",
    "like", "limit", "natural", "not", "null", "offset", "on", "only", "or", "order", "outer", "primary", "range",
    "rank", "read", "references", "right", "rows", "select", "session_user", "set", "some", "table", "then", "to",
    "trailing", "true", "union", "unique", "user", "using", "values", "when", "where", "window", "with", "write",
];

/// 标识符:不需要引号时原样写(读起来清爽),否则按方言加引号。
pub(crate) fn ident_min(s: &str, d: Dialect) -> String {
    let mut chars = s.chars();
    let head_ok = chars.next().is_some_and(|c| c == '_' || c.is_ascii_lowercase() || (d != Dialect::Pg && c.is_ascii_uppercase()));
    let rest_ok = s.chars().all(|c| c == '_' || c.is_ascii_digit() || c.is_ascii_lowercase() || (d != Dialect::Pg && c.is_ascii_uppercase()));
    if head_ok && rest_ok && !RESERVED.contains(&s.to_ascii_lowercase().as_str()) {
        s.to_string()
    } else {
        crate::sql::build::ident(s, d)
    }
}

/// 验证结论。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// 用上了索引且成本明显下降(SQLite:少了全表扫描或临时排序)
    Helps,
    /// 用上了,但没有明显收益
    NoGain,
    /// 计划没有用这个索引
    NotUsed,
    /// 没有验证(表太大等),原因在 message
    Skipped,
    Error,
}

/// 一次验证的结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Verify {
    pub verdict: Verdict,
    pub cost_before: Option<f64>,
    pub cost_after: Option<f64>,
    /// 成本下降的百分比(负数是变贵)
    pub improvement_pct: Option<f64>,
    pub uses_index: bool,
    pub full_scans_before: usize,
    pub full_scans_after: usize,
    pub sorts_before: usize,
    pub sorts_after: usize,
    /// 加上索引后的执行计划(假想索引名已换成建议的索引名)
    pub plan: Option<Plan>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateResult {
    #[serde(flatten)]
    pub candidate: Candidate,
    /// 未验证时为 None
    pub verify: Option<Verify>,
}

/// 几个有收益的候选同时加上的效果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Combo {
    pub names: Vec<String>,
    /// 计划里实际用上的
    pub used: Vec<String>,
    pub verify: Verify,
}

/// 验证方式。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// PG 假想索引(hypopg)
    Hypopg,
    /// SQLite 在回滚的事务里建真实索引
    SqliteTxn,
    Unverified,
}

/// AI 候选的情况。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiOutcome {
    /// 合并进来的候选数
    pub added: usize,
    /// 校验不过(表 / 列不存在、已被覆盖)而丢掉的,原样描述
    pub dropped: Vec<String>,
    pub error: Option<String>,
    /// 生产库未允许发数据:SQL 与计划里的字面量已打码
    pub masked: bool,
}

/// 一次索引建议的结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Advice {
    pub sql: String,
    pub mode: Mode,
    /// 验证方式的说明(为什么没验证、怎么装 hypopg 等)
    pub note: String,
    /// hypopg 可用但没装:装扩展的语句(放进编辑器,不自动执行)
    pub install_sql: Option<String>,
    /// 现在的执行计划
    pub baseline: Option<Plan>,
    pub baseline_error: Option<String>,
    pub candidates: Vec<CandidateResult>,
    pub combo: Option<Combo>,
    pub ai: Option<AiOutcome>,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub with_ai: bool,
    pub sqlite_max_rows: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self { with_ai: false, sqlite_max_rows: SQLITE_MAX_ROWS }
    }
}

fn err(key: &str) -> AppError {
    AppError::new(ErrorKind::Query, t!(key))
}

/// 语句里还有占位符(`$1`、`?`、`:name`):先填值再分析。
pub fn has_placeholders(sql: &str, d: Dialect) -> bool {
    shape::has_params(sql, d)
}

/// 取语句涉及的真实表的结构。读不到的表(别的 schema、视图等)不出现在结果里。
async fn load_metas(state: &AppState, kind: &DbKind, id: &str, shape: &Shape) -> HashMap<String, TableMeta> {
    let mut out = HashMap::new();
    for t in shape.blocks.iter().flat_map(|b| &b.tables) {
        let key = t.name.to_lowercase();
        if out.contains_key(&key) || (t.schema.is_none() && shape.ctes.iter().any(|c| c.eq_ignore_ascii_case(&t.name))) {
            continue;
        }
        // 表结构只按默认 schema / 当前库取;别的 schema 的表不给候选
        if t.schema.as_deref().is_some_and(|s| !(*kind == DbKind::Pg && s.eq_ignore_ascii_case("public")) && !(*kind == DbKind::Sqlite && s.eq_ignore_ascii_case("main"))) {
            continue;
        }
        let Ok(detail) = service::table_detail(state, kind, id, &t.name).await else { continue };
        if detail.columns.is_empty() {
            continue;
        }
        out.insert(
            key,
            TableMeta {
                columns: detail.columns.iter().map(|c| c.name.clone()).collect(),
                indexes: detail
                    .indexes
                    .iter()
                    .map(|i| (i.name.clone(), i.columns.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect()))
                    .collect(),
            },
        );
    }
    out
}

/// 给一条语句出索引建议并验证。只接受一条 SELECT / UPDATE / DELETE(可带 WITH),且不能带占位符。
pub async fn advise(state: &AppState, kind: &DbKind, id: &str, sql: &str, opts: &Options) -> AppResult<Advice> {
    let d = Dialect::from_kind(kind).ok_or_else(|| err("core.advisor.err.unsupported_db"))?;
    let sql = sql.trim().trim_end_matches(';').trim().to_string();
    if sql.is_empty() {
        return Err(err("core.advisor.err.unsupported_stmt"));
    }
    if has_placeholders(&sql, d) {
        return Err(err("core.advisor.err.params"));
    }
    let shape = shape::parse(&sql, d).ok_or_else(|| err("core.advisor.err.unsupported_stmt"))?;
    let metas = load_metas(state, kind, id, &shape).await;
    let mut cands = rules::candidates(&shape, &metas, d);
    let baseline = verify::baseline(state, kind, id, &sql).await;
    let ai = if opts.with_ai {
        Some(ai::suggest(state, kind, id, &sql, &shape, &metas, baseline.as_ref().ok(), &mut cands, d).await)
    } else {
        None
    };
    let out = verify::run(state, kind, id, &sql, &cands, opts).await;
    let (baseline, baseline_error) = match (out.baseline, baseline) {
        (Some(p), _) => (Some(p), None),
        (None, Ok(p)) => (Some(p), None),
        (None, Err(e)) => (None, Some(e.display())),
    };
    let mut results: Vec<CandidateResult> =
        cands.into_iter().zip(out.verifies).map(|(candidate, verify)| CandidateResult { candidate, verify }).collect();
    sort_results(&mut results);
    Ok(Advice { sql, mode: out.mode, note: out.note, install_sql: out.install_sql, baseline, baseline_error, candidates: results, combo: out.combo, ai })
}

/// 有收益的排前面(收益大的在前),其次是没收益、没用上、没验证的。
fn sort_results(v: &mut [CandidateResult]) {
    let rank = |r: &CandidateResult| match r.verify.as_ref().map(|v| v.verdict) {
        Some(Verdict::Helps) => 0,
        None => 1,
        Some(Verdict::NoGain) => 2,
        Some(Verdict::NotUsed) => 3,
        Some(Verdict::Skipped) => 4,
        Some(Verdict::Error) => 5,
    };
    let gain = |r: &CandidateResult| {
        r.verify.as_ref().map(|v| v.improvement_pct.unwrap_or(0.0) + (v.full_scans_before.saturating_sub(v.full_scans_after) * 10 + v.sorts_before.saturating_sub(v.sorts_after)) as f64).unwrap_or(0.0)
    };
    v.sort_by(|a, b| rank(a).cmp(&rank(b)).then(gain(b).partial_cmp(&gain(a)).unwrap_or(std::cmp::Ordering::Equal)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_names_are_sanitized_and_bounded() {
        assert_eq!(index_name("orders", &["status", "created_at"]), "idx_orders_status_created_at");
        assert_eq!(index_name("Order Items", &["Order Id"]), "idx_order_items_order_id");
        assert!(index_name(&"t".repeat(80), &["a"]).len() <= 60);
    }

    #[test]
    fn identifiers_quoted_only_when_needed() {
        assert_eq!(ident_min("status", Dialect::Pg), "status");
        assert_eq!(ident_min("Status", Dialect::Pg), "\"Status\"");
        assert_eq!(ident_min("Status", Dialect::Mysql), "Status");
        assert_eq!(ident_min("order", Dialect::Mysql), "`order`");
        assert_eq!(ident_min("a b", Dialect::Sqlite), "\"a b\"");
    }

    #[test]
    fn placeholders_detected_per_dialect() {
        assert!(has_placeholders("select * from t where a = $1", Dialect::Pg));
        assert!(has_placeholders("select * from t where a = :p1", Dialect::Pg));
        assert!(!has_placeholders("select * from t where a::int = 1 and j ? 'k'", Dialect::Pg));
        assert!(has_placeholders("select * from t where a = ?", Dialect::Mysql));
        assert!(!has_placeholders("select * from t where a = '?'", Dialect::Sqlite));
    }
}
