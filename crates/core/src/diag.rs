//! 关系库诊断:PostgreSQL / MySQL / SQLite 的会话、锁等待、长事务、慢 SQL、索引与空间。
//!
//! 每个库分成若干「分区」(section),各自是一条或几条只读查询,在数据库强制只读的会话里执行、
//! 服务端限时;结果统一成「列 + 行」,列名是稳定的英文键(界面按键翻译、按 `_bytes` 等后缀排版)。
//! 依赖的扩展 / 开关没开时(pg_stat_statements、performance_schema、dbstat)不报错,
//! 返回 `unavailable` 说明原因。
//!
//! 写操作(取消 / 终止会话、VACUUM、ANALYZE)走 [`act`],参数只接受数字或经转义的标识符。

use crate::core::connection::DbKind;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::my::charset;
use crate::pg::client::QueryResult;
use crate::service;
use crate::sql::alter::Server;
use crate::AppState;
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 诊断查询的服务端限时。
pub const DIAG_TIMEOUT_MS: u32 = 10_000;
/// 每个分区最多取多少行。
const MAX_ROWS: u32 = 500;

/// 诊断里能触发的写操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// PG:取消会话当前语句(pg_cancel_backend)
    Cancel,
    /// PG:终止会话(pg_terminate_backend)
    Terminate,
    /// MySQL:中止线程当前语句(KILL QUERY)
    KillQuery,
    /// MySQL:断开线程(KILL)
    Kill,
    /// PG:对一张表 VACUUM (ANALYZE)
    VacuumTable,
    /// MySQL:整理一张表(OPTIMIZE TABLE,回收空闲空间;InnoDB 上会重建表)
    OptimizeTable,
    /// SQLite:整库 VACUUM
    Vacuum,
    /// SQLite:整库 ANALYZE
    Analyze,
}

/// 分区的展示方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// 表格
    Table,
    /// 只有一行:按列显示成卡片
    Cards,
}

/// 分区说明(界面据此画标签、按钮)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SectionInfo {
    pub key: &'static str,
    pub layout: Layout,
    /// 每行可做的操作;参数取自 `target_columns` 指定的列
    pub row_actions: Vec<Action>,
    pub target_columns: Vec<&'static str>,
    /// 整库操作(按钮放在分区标题旁)
    pub db_actions: Vec<Action>,
    /// 开销大,不自动加载,点「运行」才查
    pub manual: bool,
}

impl SectionInfo {
    fn table(key: &'static str) -> Self {
        Self { key, layout: Layout::Table, row_actions: vec![], target_columns: vec![], db_actions: vec![], manual: false }
    }
    fn cards(key: &'static str) -> Self {
        Self { layout: Layout::Cards, ..Self::table(key) }
    }
    fn rows(mut self, actions: &[Action], columns: &[&'static str]) -> Self {
        self.row_actions = actions.to_vec();
        self.target_columns = columns.to_vec();
        self
    }
    fn db(mut self, actions: &[Action]) -> Self {
        self.db_actions = actions.to_vec();
        self
    }
    fn manual(mut self) -> Self {
        self.manual = true;
        self
    }
}

/// 某个库支持的诊断分区(按显示顺序)。Redis / Mongo 没有。
pub fn sections(kind: &DbKind) -> Vec<SectionInfo> {
    use Action::*;
    match kind {
        DbKind::Pg => vec![
            SectionInfo::cards("overview"),
            SectionInfo::table("sessions").rows(&[Cancel, Terminate], &["pid"]),
            SectionInfo::table("locks").rows(&[Cancel, Terminate], &["blocking_pid"]),
            SectionInfo::table("long_tx").rows(&[Cancel, Terminate], &["pid"]),
            SectionInfo::table("slow"),
            SectionInfo::table("unused_indexes"),
            SectionInfo::table("bloat").rows(&[VacuumTable], &["schema", "table"]),
        ],
        DbKind::Mysql => vec![
            SectionInfo::cards("overview"),
            SectionInfo::table("sessions").rows(&[KillQuery, Kill], &["id"]),
            SectionInfo::table("locks").rows(&[KillQuery, Kill], &["blocking_id"]),
            SectionInfo::table("long_tx").rows(&[KillQuery, Kill], &["id"]),
            SectionInfo::table("slow"),
            SectionInfo::table("unused_indexes"),
            SectionInfo::table("tables").rows(&[OptimizeTable], &["table"]),
            // 要扫 information_schema 的全部字符串列与索引,大库上不便宜:点「运行」才查
            SectionInfo::table("charset").manual(),
        ],
        DbKind::Sqlite => vec![
            SectionInfo::cards("overview").db(&[Vacuum, Analyze]),
            SectionInfo::table("tables"),
            SectionInfo::table("integrity").manual(),
        ],
        DbKind::Redis | DbKind::Mongo => vec![],
    }
}

/// 依赖没开时的说明:`reason` 是界面文案的键,`detail` 是数据库原话(可能没有)。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Unavailable {
    pub reason: &'static str,
    pub detail: Option<String>,
}

/// 一个分区的结果。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiagResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub unavailable: Option<Unavailable>,
    /// 整个分区的修复脚本(MySQL 字符集体检:转 utf8mb4),界面放进编辑器、不直接执行
    pub script: Option<String>,
}

impl From<QueryResult> for DiagResult {
    fn from(r: QueryResult) -> Self {
        Self { columns: r.columns, rows: r.rows, unavailable: None, script: None }
    }
}

fn unavailable(reason: &'static str, detail: Option<String>) -> DiagResult {
    DiagResult { unavailable: Some(Unavailable { reason, detail }), ..Default::default() }
}

fn unknown_section(key: &str) -> AppError {
    AppError::new(ErrorKind::Query, t!("core.err.diag_unknown", key = key))
}

async fn ro(state: &AppState, kind: &DbKind, id: &str, sql: &str) -> AppResult<QueryResult> {
    service::query_read_only_timeout(state, kind, id, sql, MAX_ROWS, DIAG_TIMEOUT_MS).await
}

/// 依次尝试几种写法(不同版本 / 分支的系统表不同),返回第一个成功的;都失败时返回最后一个错误。
async fn first_ok(state: &AppState, kind: &DbKind, id: &str, sqls: &[&str]) -> AppResult<QueryResult> {
    let mut last = None;
    for sql in sqls {
        match ro(state, kind, id, sql).await {
            Ok(r) => return Ok(r),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| unknown_section("")))
}

/// 查询一个分区。
pub async fn run(state: &AppState, kind: &DbKind, id: &str, key: &str) -> AppResult<DiagResult> {
    match kind {
        DbKind::Pg => pg_run(state, id, key).await,
        DbKind::Mysql => my_run(state, id, key).await,
        DbKind::Sqlite => sqlite_run(state, id, key).await,
        DbKind::Redis | DbKind::Mongo => Err(unknown_section(key)),
    }
}

// ---------- PostgreSQL ----------

const PG_OVERVIEW: &str = "\
SELECT current_setting('server_version') AS version,
  pg_database_size(current_database()) AS db_size_bytes,
  concat((SELECT count(*) FROM pg_stat_activity WHERE {CLIENT}), ' / ',
         current_setting('max_connections')) AS connections,
  (SELECT count(*) FROM pg_stat_activity WHERE {CLIENT} AND state = 'active') AS active,
  (SELECT count(*) FROM pg_stat_activity WHERE {CLIENT}
     AND state LIKE 'idle in transaction%') AS idle_in_tx,
  (SELECT round(100.0 * blks_hit / NULLIF(blks_hit + blks_read, 0), 2) FROM pg_stat_database
     WHERE datname = current_database()) AS cache_hit_pct,
  (SELECT deadlocks FROM pg_stat_database WHERE datname = current_database()) AS deadlocks,
  (SELECT age(datfrozenxid) FROM pg_database WHERE datname = current_database()) AS xid_age,
  date_trunc('second', now() - pg_postmaster_start_time())::text AS uptime,
  (SELECT to_char(stats_reset, 'YYYY-MM-DD HH24:MI') FROM pg_stat_database
     WHERE datname = current_database()) AS stats_reset";

const PG_SESSIONS: &str = "\
SELECT pid, usename AS \"user\", datname AS database, client_addr::text AS client,
  application_name AS app, state,
  date_trunc('second', now() - query_start)::text AS query_time,
  date_trunc('second', now() - xact_start)::text AS xact_time,
  NULLIF(concat_ws(':', wait_event_type, wait_event), '') AS wait,
  query
FROM pg_stat_activity
WHERE pid <> pg_backend_pid() AND {CLIENT}
ORDER BY (state = 'active') DESC, (state LIKE 'idle in transaction%') DESC, query_start NULLS LAST";

const PG_LOCKS: &str = "\
SELECT a.pid AS blocked_pid, a.usename AS \"user\",
  date_trunc('second', now() - a.query_start)::text AS waiting,
  b.pid AS blocking_pid, b.state AS blocking_state,
  date_trunc('second', now() - b.xact_start)::text AS blocking_xact_time,
  a.query AS blocked_query, b.query AS blocking_query
FROM pg_stat_activity a
CROSS JOIN LATERAL unnest(pg_blocking_pids(a.pid)) AS bp(pid)
JOIN pg_stat_activity b ON b.pid = bp.pid
ORDER BY a.query_start";

const PG_LONG_TX: &str = "\
SELECT pid, usename AS \"user\", datname AS database, state,
  date_trunc('second', now() - xact_start)::text AS xact_time,
  date_trunc('second', now() - state_change)::text AS state_time,
  backend_xmin::text AS xmin, query
FROM pg_stat_activity
WHERE xact_start IS NOT NULL AND pid <> pg_backend_pid() AND now() - xact_start > interval '30 seconds'
ORDER BY xact_start";

const PG_UNUSED_INDEXES: &str = "\
SELECT s.schemaname AS schema, s.relname AS \"table\", s.indexrelname AS index,
  pg_relation_size(s.indexrelid) AS size_bytes, s.idx_scan AS scans,
  pg_get_indexdef(s.indexrelid) AS definition
FROM pg_stat_user_indexes s JOIN pg_index i ON i.indexrelid = s.indexrelid
WHERE s.idx_scan = 0 AND NOT i.indisunique AND NOT i.indisprimary
  AND NOT EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = s.indexrelid)
ORDER BY pg_relation_size(s.indexrelid) DESC
LIMIT 100";

const PG_BLOAT: &str = "\
SELECT schemaname AS schema, relname AS \"table\", n_live_tup AS live_rows, n_dead_tup AS dead_rows,
  round(100.0 * n_dead_tup / NULLIF(n_live_tup + n_dead_tup, 0), 1) AS dead_pct,
  pg_total_relation_size(relid) AS size_bytes,
  to_char(greatest(last_vacuum, last_autovacuum), 'YYYY-MM-DD HH24:MI') AS last_vacuum,
  to_char(greatest(last_analyze, last_autoanalyze), 'YYYY-MM-DD HH24:MI') AS last_analyze
FROM pg_stat_user_tables
ORDER BY n_dead_tup DESC, pg_total_relation_size(relid) DESC
LIMIT 100";

/// 「客户端会话」的条件:PG 10 起有 backend_type;9.6 没有,排除后台的 autovacuum 进程。
fn pg_client_filter(version_num: u32) -> &'static str {
    if version_num >= 100_000 {
        "backend_type = 'client backend'"
    } else {
        "datid IS NOT NULL AND coalesce(query, '') NOT LIKE 'autovacuum:%'"
    }
}

async fn pg_version_num(state: &AppState, id: &str) -> u32 {
    ro(state, &DbKind::Pg, id, "SELECT current_setting('server_version_num')")
        .await
        .ok()
        .and_then(|r| r.rows.first().and_then(|r| r[0].clone()))
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000)
}

async fn pg_run(state: &AppState, id: &str, key: &str) -> AppResult<DiagResult> {
    let kind = DbKind::Pg;
    if matches!(key, "overview" | "sessions") {
        let base = if key == "overview" { PG_OVERVIEW } else { PG_SESSIONS };
        let sql = base.replace("{CLIENT}", pg_client_filter(pg_version_num(state, id).await));
        return Ok(ro(state, &kind, id, &sql).await?.into());
    }
    let sql = match key {
        "locks" => PG_LOCKS,
        "long_tx" => PG_LONG_TX,
        "unused_indexes" => PG_UNUSED_INDEXES,
        "bloat" => PG_BLOAT,
        "slow" => return pg_slow(state, id).await,
        _ => return Err(unknown_section(key)),
    };
    Ok(ro(state, &kind, id, sql).await?.into())
}

/// pg_stat_statements 的排行查询:PG 13 起列名带 `exec`;扩展装在哪个 schema 就查哪个。
fn pg_slow_sql(schema: &str, version_num: u32) -> String {
    let (total, mean, max) = if version_num >= 130000 {
        ("total_exec_time", "mean_exec_time", "max_exec_time")
    } else {
        ("total_time", "mean_time", "max_time")
    };
    let schema = format!("\"{}\"", schema.replace('"', "\"\""));
    format!(
        "SELECT calls, round({total}::numeric, 1) AS total_ms, round({mean}::numeric, 2) AS mean_ms, \
         round({max}::numeric, 1) AS max_ms, rows, \
         round(100.0 * shared_blks_hit / NULLIF(shared_blks_hit + shared_blks_read, 0), 1) AS hit_pct, query \
         FROM {schema}.pg_stat_statements \
         WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) \
         ORDER BY {total} DESC LIMIT 50"
    )
}

async fn pg_slow(state: &AppState, id: &str) -> AppResult<DiagResult> {
    let kind = DbKind::Pg;
    let probe = ro(
        state,
        &kind,
        id,
        "SELECT n.nspname, current_setting('server_version_num') FROM pg_extension e \
         JOIN pg_namespace n ON n.oid = e.extnamespace WHERE e.extname = 'pg_stat_statements'",
    )
    .await?;
    let Some(row) = probe.rows.first() else {
        return Ok(unavailable("pg_stat_statements_missing", None));
    };
    let schema = row[0].clone().unwrap_or_else(|| "public".into());
    let version: u32 = row[1].as_deref().and_then(|v| v.parse().ok()).unwrap_or(0);
    match ro(state, &kind, id, &pg_slow_sql(&schema, version)).await {
        Ok(r) => Ok(r.into()),
        // 建了扩展但没进 shared_preload_libraries:查询时才报错
        Err(e) if e.display().contains("shared_preload_libraries") => {
            Ok(unavailable("pg_stat_statements_not_loaded", Some(e.display())))
        }
        Err(e) => Err(e),
    }
}

// ---------- MySQL ----------

const MY_SESSIONS: &str = "\
SELECT ID AS `id`, USER AS `user`, HOST AS `host`, DB AS `database`, COMMAND AS `command`,
  TIME AS `seconds`, STATE AS `state`, INFO AS `query`
FROM information_schema.PROCESSLIST
WHERE ID <> CONNECTION_ID() AND COMMAND NOT IN ('Daemon', 'Binlog Dump', 'Binlog Dump GTID')
ORDER BY (COMMAND = 'Sleep'), TIME DESC";

/// 锁等待:优先用 sys 视图(5.7 / 8.0),没有 sys(MariaDB 等)退回 5.7 风格的 information_schema。
const MY_LOCKS: &[&str] = &[
    "SELECT waiting_pid AS `blocked_id`, wait_age_secs AS `waiting_seconds`, locked_table AS `table`, \
       locked_type AS `lock_type`, blocking_pid AS `blocking_id`, waiting_query AS `blocked_query`, \
       blocking_query AS `blocking_query` \
     FROM sys.innodb_lock_waits ORDER BY wait_age_secs DESC",
    "SELECT r.trx_mysql_thread_id AS `blocked_id`, TIMESTAMPDIFF(SECOND, r.trx_wait_started, NOW()) AS `waiting_seconds`, \
       NULL AS `table`, NULL AS `lock_type`, b.trx_mysql_thread_id AS `blocking_id`, \
       r.trx_query AS `blocked_query`, b.trx_query AS `blocking_query` \
     FROM information_schema.INNODB_LOCK_WAITS w \
     JOIN information_schema.INNODB_TRX b ON b.trx_id = w.blocking_trx_id \
     JOIN information_schema.INNODB_TRX r ON r.trx_id = w.requesting_trx_id \
     ORDER BY r.trx_wait_started",
];

const MY_LONG_TX: &str = "\
SELECT t.trx_mysql_thread_id AS `id`, p.USER AS `user`, t.trx_state AS `state`,
  TIMESTAMPDIFF(SECOND, t.trx_started, NOW()) AS `seconds`,
  t.trx_rows_locked AS `rows_locked`, t.trx_rows_modified AS `rows_modified`,
  p.COMMAND AS `command`, t.trx_query AS `query`
FROM information_schema.INNODB_TRX t
LEFT JOIN information_schema.PROCESSLIST p ON p.ID = t.trx_mysql_thread_id
WHERE t.trx_mysql_thread_id <> CONNECTION_ID()
ORDER BY t.trx_started";

/// 慢 SQL 排行。8.0 起有 QUERY_SAMPLE_TEXT(带真实值的样例语句,「在编辑器打开 / Explain」优先用它;
/// 界面不显示这一列);5.7 / MariaDB 没有这一列,退回只有摘要文本的写法。
const MY_SLOW: &[&str] = &[
    "SELECT COUNT_STAR AS `calls`, ROUND(SUM_TIMER_WAIT / 1e9, 1) AS `total_ms`, \
       ROUND(AVG_TIMER_WAIT / 1e9, 2) AS `mean_ms`, ROUND(MAX_TIMER_WAIT / 1e9, 1) AS `max_ms`, \
       SUM_ROWS_EXAMINED AS `rows_examined`, SUM_ROWS_SENT AS `rows_sent`, \
       SCHEMA_NAME AS `database`, DIGEST_TEXT AS `query`, QUERY_SAMPLE_TEXT AS `sample` \
     FROM performance_schema.events_statements_summary_by_digest \
     WHERE DIGEST_TEXT IS NOT NULL ORDER BY SUM_TIMER_WAIT DESC LIMIT 50",
    "SELECT COUNT_STAR AS `calls`, ROUND(SUM_TIMER_WAIT / 1e9, 1) AS `total_ms`, \
       ROUND(AVG_TIMER_WAIT / 1e9, 2) AS `mean_ms`, ROUND(MAX_TIMER_WAIT / 1e9, 1) AS `max_ms`, \
       SUM_ROWS_EXAMINED AS `rows_examined`, SUM_ROWS_SENT AS `rows_sent`, \
       SCHEMA_NAME AS `database`, DIGEST_TEXT AS `query` \
     FROM performance_schema.events_statements_summary_by_digest \
     WHERE DIGEST_TEXT IS NOT NULL ORDER BY SUM_TIMER_WAIT DESC LIMIT 50",
];
const MY_UNUSED_INDEXES: &str = "\
SELECT object_schema AS `schema`, object_name AS `table`, index_name AS `index`
FROM sys.schema_unused_indexes
WHERE object_schema = DATABASE()
ORDER BY object_name, index_name";

const MY_TABLES: &str = "\
SELECT TABLE_NAME AS `table`, ENGINE AS `engine`, TABLE_ROWS AS `rows_estimate`,
  DATA_LENGTH AS `data_bytes`, INDEX_LENGTH AS `index_bytes`, DATA_FREE AS `free_bytes`,
  DATE_FORMAT(UPDATE_TIME, '%Y-%m-%d %H:%i') AS `updated`
FROM information_schema.TABLES
WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE'
ORDER BY DATA_LENGTH + INDEX_LENGTH DESC
LIMIT 200";

/// 权限不足 / 系统库不存在:这类错误按「不可用」显示,而不是报错。
fn my_unavailable(e: &AppError) -> Option<DiagResult> {
    let text = e.display();
    let lower = text.to_ascii_lowercase();
    if lower.contains("access denied") || lower.contains("command denied") || lower.contains("privilege") {
        return Some(unavailable("privilege", Some(text)));
    }
    if lower.contains("doesn't exist") || lower.contains("unknown database") || lower.contains("unknown table") {
        return Some(unavailable("my_sys_missing", Some(text)));
    }
    None
}

async fn my_run(state: &AppState, id: &str, key: &str) -> AppResult<DiagResult> {
    let kind = DbKind::Mysql;
    let r = match key {
        "overview" => return my_overview(state, id).await,
        "sessions" => ro(state, &kind, id, MY_SESSIONS).await,
        "locks" => first_ok(state, &kind, id, MY_LOCKS).await,
        "long_tx" => ro(state, &kind, id, MY_LONG_TX).await,
        "slow" => {
            let on = ro(state, &kind, id, "SELECT @@performance_schema").await?;
            if on.rows.first().and_then(|r| r[0].as_deref()) != Some("1") {
                return Ok(unavailable("performance_schema_off", None));
            }
            first_ok(state, &kind, id, MY_SLOW).await
        }
        "unused_indexes" => {
            let on = ro(state, &kind, id, "SELECT @@performance_schema").await?;
            if on.rows.first().and_then(|r| r[0].as_deref()) != Some("1") {
                return Ok(unavailable("performance_schema_off", None));
            }
            ro(state, &kind, id, MY_UNUSED_INDEXES).await
        }
        "tables" => ro(state, &kind, id, MY_TABLES).await,
        "charset" => {
            return match my_charset(state, id).await {
                Ok(r) => Ok(r),
                Err(e) => my_unavailable(&e).ok_or(e),
            }
        }
        _ => return Err(unknown_section(key)),
    };
    match r {
        Ok(r) => Ok(r.into()),
        Err(e) => my_unavailable(&e).ok_or(e),
    }
}

/// 把 `SHOW GLOBAL STATUS` 的两列结果变成表。
fn status_map(r: &QueryResult) -> HashMap<String, String> {
    r.rows
        .iter()
        .filter_map(|row| Some((row.first()?.clone()?, row.get(1)?.clone().unwrap_or_default())))
        .collect()
}

/// 秒数 → `3d 4h` / `4h 5m` / `5m 6s`。
pub fn human_secs(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// MySQL 概览:由 `SHOW GLOBAL STATUS` 算出命中率、QPS 等(各版本都有,不依赖 performance_schema)。
fn my_overview_row(status: &HashMap<String, String>, version: &str, max_conn: &str, db_bytes: Option<String>) -> DiagResult {
    let num = |k: &str| status.get(k).and_then(|v| v.parse::<f64>().ok());
    let uptime = num("Uptime").unwrap_or(0.0);
    let hit = match (num("Innodb_buffer_pool_read_requests"), num("Innodb_buffer_pool_reads")) {
        (Some(req), Some(disk)) if req > 0.0 => Some(format!("{:.2}", 100.0 * (1.0 - disk / req))),
        _ => None,
    };
    let qps = num("Questions").filter(|_| uptime > 0.0).map(|q| format!("{:.1}", q / uptime));
    let pairs: Vec<(&str, Option<String>)> = vec![
        ("version", Some(version.to_string())),
        ("db_size_bytes", db_bytes),
        ("connections", status.get("Threads_connected").map(|c| format!("{c} / {max_conn}"))),
        ("running", status.get("Threads_running").cloned()),
        ("buffer_hit_pct", hit),
        ("qps", qps),
        ("slow_queries", status.get("Slow_queries").cloned()),
        ("aborted_connects", status.get("Aborted_connects").cloned()),
        ("uptime", Some(human_secs(uptime as u64))),
    ];
    DiagResult {
        columns: pairs.iter().map(|(k, _)| k.to_string()).collect(),
        rows: vec![pairs.into_iter().map(|(_, v)| v).collect()],
        unavailable: None,
        script: None,
    }
}

async fn my_overview(state: &AppState, id: &str) -> AppResult<DiagResult> {
    let kind = DbKind::Mysql;
    // 只取用到的几项:MariaDB 的状态变量超过 500 个,整张取会被行数上限截掉
    let status = status_map(
        &ro(
            state,
            &kind,
            id,
            "SHOW GLOBAL STATUS WHERE Variable_name IN ('Uptime', 'Threads_connected', 'Threads_running', \
             'Innodb_buffer_pool_read_requests', 'Innodb_buffer_pool_reads', 'Questions', 'Slow_queries', 'Aborted_connects')",
        )
        .await?,
    );
    let base = ro(
        state,
        &kind,
        id,
        "SELECT VERSION(), @@max_connections, \
         (SELECT SUM(DATA_LENGTH + INDEX_LENGTH) FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE())",
    )
    .await?;
    let row = base.rows.first().cloned().unwrap_or_default();
    let get = |i: usize| row.get(i).cloned().flatten();
    Ok(my_overview_row(&status, &get(0).unwrap_or_default(), &get(1).unwrap_or_default(), get(2)))
}

// ---------- SQLite ----------

/// 字符集体检读元数据时每条查询的行数上限(列、索引可能远多于界面的 500 行)。
const CHARSET_SCAN_ROWS: u32 = 20_000;

/// 体检范围:连接选了库就只看这个库,没选库时看全部用户库(排除系统库)。
fn charset_scope(db: Option<&str>) -> String {
    use crate::sql::build::literal;
    match db {
        Some(db) => format!("= {}", literal(db, crate::sql::Dialect::Mysql)),
        None => format!(
            "NOT IN ({})",
            charset::SYSTEM_SCHEMAS.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// MySQL 字符集 / 排序规则体检:从 information_schema 取库、表、字符串列、索引、外键与可用的 utf8mb4 规则,
/// 检查逻辑在 [`charset::check`]。information_schema 只返回有权限的对象,权限不够时结果会不全但不报错。
async fn my_charset(state: &AppState, id: &str) -> AppResult<DiagResult> {
    let kind = DbKind::Mysql;
    let scan = |sql: String| async move {
        service::query_read_only_timeout(state, &kind, id, &sql, CHARSET_SCAN_ROWS, DIAG_TIMEOUT_MS).await
    };
    let base = ro(state, &kind, id, "SELECT VERSION(), DATABASE()").await?;
    let row = base.rows.first().cloned().unwrap_or_default();
    let version = row.first().cloned().flatten().unwrap_or_default();
    let server = Server::parse(&version).unwrap_or(Server::mysql(8, 0, 0));
    let db = row.get(1).cloned().flatten().filter(|d| !d.is_empty());
    let scope = charset_scope(db.as_deref());

    let schemas = charset::schemas_from(
        &scan(format!(
            "SELECT SCHEMA_NAME, DEFAULT_COLLATION_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME {scope}"
        ))
        .await?,
    );
    let tables = charset::tables_from(
        &scan(format!(
            "SELECT TABLE_SCHEMA, TABLE_NAME, ENGINE, ROW_FORMAT, TABLE_COLLATION FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA {scope} AND TABLE_TYPE = 'BASE TABLE'"
        ))
        .await?,
    );
    let col_rows = scan(format!(
        "SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, CHARACTER_MAXIMUM_LENGTH, CHARACTER_SET_NAME, \
           COLLATION_NAME, IS_NULLABLE, COLUMN_DEFAULT, EXTRA, COLUMN_COMMENT \
         FROM information_schema.COLUMNS WHERE TABLE_SCHEMA {scope} AND CHARACTER_SET_NAME IS NOT NULL \
         ORDER BY TABLE_SCHEMA, TABLE_NAME, ORDINAL_POSITION"
    ))
    .await?;
    let truncated_columns = (col_rows.rows.len() >= CHARSET_SCAN_ROWS as usize).then_some(CHARSET_SCAN_ROWS as usize);
    // 视图的列也在 COLUMNS 里:只留基表的
    let base_tables: std::collections::HashSet<(String, String)> =
        tables.iter().map(|t| (t.schema.clone(), t.name.clone())).collect();
    let columns: Vec<charset::Column> = charset::columns_from(&col_rows)
        .into_iter()
        .filter(|c| base_tables.contains(&(c.schema.clone(), c.table.clone())))
        .collect();
    let indexes = charset::indexes_from(
        &scan(format!(
            "SELECT TABLE_SCHEMA, TABLE_NAME, INDEX_NAME, COLUMN_NAME, SUB_PART, NON_UNIQUE, INDEX_TYPE \
             FROM information_schema.STATISTICS WHERE TABLE_SCHEMA {scope} \
             ORDER BY TABLE_SCHEMA, TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX"
        ))
        .await?,
    );
    let fks = charset::fks_from(
        &scan(format!(
            "SELECT CONSTRAINT_NAME, TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, \
               REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
             FROM information_schema.KEY_COLUMN_USAGE \
             WHERE TABLE_SCHEMA {scope} AND REFERENCED_TABLE_NAME IS NOT NULL"
        ))
        .await?,
    );
    let colls = scan(
        "SELECT COLLATION_NAME, IS_DEFAULT FROM information_schema.COLLATIONS WHERE CHARACTER_SET_NAME = 'utf8mb4'".into(),
    )
    .await?;
    let mut utf8mb4_collations: Vec<String> =
        colls.rows.iter().filter_map(|r| r.first().cloned().flatten()).map(|c| c.to_ascii_lowercase()).collect();
    let utf8mb4_default = colls
        .rows
        .iter()
        .find(|r| r.get(1).cloned().flatten().is_some_and(|d| d.eq_ignore_ascii_case("yes")))
        .and_then(|r| r.first().cloned().flatten())
        .map(|c| c.to_ascii_lowercase());
    // MariaDB 10.10+ 的 uca1400 等规则按「完整名」登记在这里;别的版本没有这一列,查不到就算了
    if let Ok(full) = ro(
        state,
        &kind,
        id,
        "SELECT FULL_COLLATION_NAME FROM information_schema.COLLATION_CHARACTER_SET_APPLICABILITY \
         WHERE CHARACTER_SET_NAME = 'utf8mb4'",
    )
    .await
    {
        utf8mb4_collations.extend(full.rows.iter().filter_map(|r| r.first().cloned().flatten()).map(|c| c.to_ascii_lowercase()));
    }
    // 5.7 的 innodb_large_prefix(8.0 / MariaDB 10.3+ 已移除,查询报错即视为没有这个限制)
    let large_prefix = ro(state, &kind, id, "SELECT @@innodb_large_prefix")
        .await
        .ok()
        .and_then(|r| r.rows.first().and_then(|r| r.first().cloned().flatten()))
        .map(|v| v == "1" || v.eq_ignore_ascii_case("on"));

    let report = charset::check(&charset::Snapshot {
        server,
        multi_schema: db.is_none(),
        schemas,
        tables,
        columns,
        indexes,
        fks,
        utf8mb4_collations,
        utf8mb4_default,
        large_prefix,
        truncated_columns,
    });
    Ok(charset_result(report))
}

fn charset_result(r: charset::Report) -> DiagResult {
    let columns = ["severity", "issue", "object", "current", "suggested", "note", "fix"];
    let rows = r
        .findings
        .into_iter()
        .map(|f| {
            let s = |v: String| (!v.is_empty()).then_some(v);
            vec![
                Some(f.severity.code().to_string()),
                Some(f.issue.to_string()),
                s(f.object),
                s(f.current),
                s(f.suggested),
                s(f.note),
                f.fix,
            ]
        })
        .collect();
    DiagResult { columns: columns.map(String::from).to_vec(), rows, unavailable: None, script: r.script }
}

const SQLITE_OVERVIEW: &str = "\
SELECT p.page_count * s.page_size AS file_size_bytes, p.page_count AS pages, s.page_size AS page_size_bytes,
  f.freelist_count AS free_pages, f.freelist_count * s.page_size AS free_bytes,
  (SELECT journal_mode FROM pragma_journal_mode) AS journal_mode,
  CASE (SELECT auto_vacuum FROM pragma_auto_vacuum) WHEN 0 THEN 'none' WHEN 1 THEN 'full' WHEN 2 THEN 'incremental' END
    AS auto_vacuum,
  (SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%') AS tables,
  (SELECT count(*) FROM sqlite_master WHERE type = 'index') AS indexes,
  sqlite_version() AS version
FROM pragma_page_count AS p, pragma_page_size AS s, pragma_freelist_count AS f";

/// 每张表 / 索引占用的空间(dbstat 虚表,编译时没开就不可用)。
const SQLITE_TABLES: &str = "\
SELECT d.name AS name, COALESCE(m.type, 'table') AS type, m.tbl_name AS \"table\",
  SUM(d.pgsize) AS size_bytes, SUM(d.unused) AS unused_bytes, COUNT(*) AS pages
FROM dbstat AS d LEFT JOIN sqlite_master AS m ON m.name = d.name
GROUP BY d.name
ORDER BY SUM(d.pgsize) DESC
LIMIT 200";

async fn sqlite_run(state: &AppState, id: &str, key: &str) -> AppResult<DiagResult> {
    let kind = DbKind::Sqlite;
    match key {
        "overview" => Ok(ro(state, &kind, id, SQLITE_OVERVIEW).await?.into()),
        "tables" => match ro(state, &kind, id, SQLITE_TABLES).await {
            Ok(r) => Ok(r.into()),
            Err(e) if e.display().contains("dbstat") => Ok(unavailable("dbstat_missing", Some(e.display()))),
            Err(e) => Err(e),
        },
        // 最多报 100 条问题;健康时只有一行 ok
        "integrity" => {
            let r = ro(state, &kind, id, "SELECT integrity_check AS message FROM pragma_integrity_check(100)").await?;
            Ok(r.into())
        }
        _ => Err(unknown_section(key)),
    }
}

// ---------- 写操作 ----------

fn bad_arg() -> AppError {
    AppError::new(ErrorKind::Query, t!("core.err.diag_bad_arg"))
}

/// 会话 / 线程号:必须是正整数(拼进 SQL 前校验)。
fn session_id(args: &[String]) -> AppResult<u64> {
    args.first().and_then(|a| a.trim().parse::<u64>().ok()).filter(|n| *n > 0).ok_or_else(bad_arg)
}

fn pg_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// 一个操作对应的 SQL(不执行),便于确认框展示与测试。
pub fn action_sql(kind: &DbKind, action: Action, args: &[String]) -> AppResult<String> {
    use Action::*;
    match (kind, action) {
        (DbKind::Pg, Cancel) => Ok(format!("SELECT pg_cancel_backend({})", session_id(args)?)),
        (DbKind::Pg, Terminate) => Ok(format!("SELECT pg_terminate_backend({})", session_id(args)?)),
        (DbKind::Pg, VacuumTable) => match args {
            [schema, table] if !schema.is_empty() && !table.is_empty() => {
                Ok(format!("VACUUM (ANALYZE) {}.{}", pg_ident(schema), pg_ident(table)))
            }
            _ => Err(bad_arg()),
        },
        (DbKind::Mysql, KillQuery) => Ok(format!("KILL QUERY {}", session_id(args)?)),
        (DbKind::Mysql, Kill) => Ok(format!("KILL {}", session_id(args)?)),
        (DbKind::Mysql, OptimizeTable) => match args {
            [table] if !table.is_empty() => Ok(format!("OPTIMIZE TABLE {}", crate::sql::build::ident(table, crate::sql::Dialect::Mysql))),
            _ => Err(bad_arg()),
        },
        (DbKind::Sqlite, Vacuum) => Ok("VACUUM".into()),
        (DbKind::Sqlite, Analyze) => Ok("ANALYZE".into()),
        _ => Err(bad_arg()),
    }
}

/// 执行一个诊断操作。PG 的取消 / 终止返回 false(会话已结束或无权限)时报错。
pub async fn act(state: &AppState, kind: &DbKind, id: &str, action: Action, args: &[String]) -> AppResult<()> {
    let sql = action_sql(kind, action, args)?;
    let r = service::query(state, kind, id, &sql, Some(20)).await?;
    // OPTIMIZE TABLE 出错不抛异常,而是在结果里给一行 Msg_type = error
    if action == Action::OptimizeTable {
        let col = |name: &str| r.columns.iter().position(|c| c.eq_ignore_ascii_case(name));
        if let (Some(ty), Some(text)) = (col("Msg_type"), col("Msg_text")) {
            if let Some(row) = r.rows.iter().find(|row| row.get(ty).cloned().flatten().as_deref() == Some("error")) {
                return Err(AppError::new(ErrorKind::Query, row.get(text).cloned().flatten().unwrap_or_default()));
            }
        }
    }
    if matches!(action, Action::Cancel | Action::Terminate)
        && r.rows.first().and_then(|r| r.first()).and_then(|v| v.as_deref()) != Some("t")
    {
        return Err(AppError::new(ErrorKind::Query, t!("core.err.diag_signal_failed")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_relational_kind_has_sections_with_known_targets() {
        for kind in [DbKind::Pg, DbKind::Mysql, DbKind::Sqlite] {
            let s = sections(&kind);
            assert_eq!(s[0].key, "overview");
            assert_eq!(s[0].layout, Layout::Cards);
            for sec in &s {
                assert_eq!(sec.row_actions.is_empty(), sec.target_columns.is_empty(), "{}", sec.key);
            }
        }
        assert!(sections(&DbKind::Redis).is_empty());
    }

    #[test]
    fn action_sql_validates_arguments() {
        let a = |s: &str| vec![s.to_string()];
        assert_eq!(action_sql(&DbKind::Pg, Action::Terminate, &a("42")).unwrap(), "SELECT pg_terminate_backend(42)");
        assert_eq!(action_sql(&DbKind::Mysql, Action::KillQuery, &a(" 7 ")).unwrap(), "KILL QUERY 7");
        assert!(action_sql(&DbKind::Pg, Action::Cancel, &a("1; DROP TABLE x")).is_err());
        assert!(action_sql(&DbKind::Mysql, Action::Kill, &a("0")).is_err());
        assert!(action_sql(&DbKind::Mysql, Action::Kill, &[]).is_err());
        // 操作与库类型不匹配
        assert!(action_sql(&DbKind::Mysql, Action::Terminate, &a("1")).is_err());
        assert!(action_sql(&DbKind::Sqlite, Action::Kill, &a("1")).is_err());
        let v = action_sql(&DbKind::Pg, Action::VacuumTable, &["public".into(), "a\"b".into()]).unwrap();
        assert_eq!(v, "VACUUM (ANALYZE) \"public\".\"a\"\"b\"");
        assert!(action_sql(&DbKind::Pg, Action::VacuumTable, &a("public")).is_err());
        assert_eq!(action_sql(&DbKind::Mysql, Action::OptimizeTable, &a("a`b")).unwrap(), "OPTIMIZE TABLE `a``b`");
        assert!(action_sql(&DbKind::Mysql, Action::OptimizeTable, &a("")).is_err());
        assert!(action_sql(&DbKind::Pg, Action::OptimizeTable, &a("t")).is_err());
    }

    #[test]
    fn pg_client_filter_by_version() {
        assert!(PG_OVERVIEW.contains("{CLIENT}") && PG_SESSIONS.contains("{CLIENT}"));
        assert!(pg_client_filter(160002).contains("backend_type"));
        assert!(!pg_client_filter(90624).contains("backend_type"));
    }

    #[test]
    fn pg_slow_sql_follows_version_and_schema() {
        let old = pg_slow_sql("public", 120005);
        assert!(old.contains("total_time") && !old.contains("total_exec_time"));
        let new = pg_slow_sql("ext", 160002);
        assert!(new.contains("total_exec_time") && new.contains("\"ext\".pg_stat_statements"));
    }

    #[test]
    fn mysql_overview_from_status() {
        let status: HashMap<String, String> = [
            ("Uptime", "90061"),
            ("Threads_connected", "5"),
            ("Threads_running", "2"),
            ("Innodb_buffer_pool_read_requests", "1000"),
            ("Innodb_buffer_pool_reads", "10"),
            ("Questions", "180122"),
            ("Slow_queries", "3"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let r = my_overview_row(&status, "8.0.36", "151", Some("2048".into()));
        let get = |k: &str| r.rows[0][r.columns.iter().position(|c| c == k).unwrap()].clone();
        assert_eq!(get("connections").as_deref(), Some("5 / 151"));
        assert_eq!(get("buffer_hit_pct").as_deref(), Some("99.00"));
        assert_eq!(get("qps").as_deref(), Some("2.0"));
        assert_eq!(get("uptime").as_deref(), Some("1d 1h"));
        assert_eq!(get("aborted_connects"), None);
    }

    #[test]
    fn human_secs_units() {
        assert_eq!(human_secs(5), "5s");
        assert_eq!(human_secs(125), "2m 5s");
        assert_eq!(human_secs(3725), "1h 2m");
    }

    #[test]
    fn mysql_permission_errors_become_unavailable() {
        let e = AppError::new(ErrorKind::Query, "x").with_detail("Access denied; you need the PROCESS privilege");
        assert_eq!(my_unavailable(&e).unwrap().unavailable.unwrap().reason, "privilege");
        let e = AppError::new(ErrorKind::Query, "x").with_detail("Table 'sys.innodb_lock_waits' doesn't exist");
        assert_eq!(my_unavailable(&e).unwrap().unavailable.unwrap().reason, "my_sys_missing");
        assert!(my_unavailable(&AppError::new(ErrorKind::Query, "syntax error")).is_none());
    }
}
