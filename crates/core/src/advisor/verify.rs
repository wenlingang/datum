//! 候选索引的验证。不在库里留下任何对象:
//! - PG + hypopg:辅助会话上开只读事务,建假想索引(只存在于本会话内存)→ EXPLAIN → `hypopg_reset()`;
//!   结束时回滚事务并再清一次(假想索引不随事务回滚)。
//! - SQLite:连接可写、没有进行中的事务、表不大时,`BEGIN; CREATE INDEX …; EXPLAIN QUERY PLAN …; ROLLBACK`。
//! - MySQL / MariaDB:只取基线计划。
//!
//! 每条语句都限时(PG 的 statement_timeout,SQLite 的进度回调)。

use super::{Candidate, Combo, Mode, Options, Verdict, Verify, VERIFY_TIMEOUT_MS};
use crate::core::connection::DbKind;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::explain::{self, Plan, PlanNode};
use crate::pg::client::{run_capped, QueryResult};
use crate::service;
use crate::sql::Dialect;
use crate::AppState;
use rust_i18n::t;
use std::time::{Duration, Instant};

/// 成本至少降这么多才算有收益。
const MIN_GAIN: f64 = 0.05;

pub(super) struct Outcome {
    pub mode: Mode,
    pub note: String,
    pub install_sql: Option<String>,
    /// 验证时顺带拿到的基线计划(PG hypopg / SQLite)
    pub baseline: Option<Plan>,
    /// 与候选一一对应
    pub verifies: Vec<Option<Verify>>,
    pub combo: Option<Combo>,
}

impl Outcome {
    fn unverified(n: usize, note: String) -> Self {
        Outcome { mode: Mode::Unverified, note, install_sql: None, baseline: None, verifies: vec![None; n], combo: None }
    }
}

fn plan_of(tree: Option<PlanNode>, raw: QueryResult) -> Plan {
    Plan { tree, raw, analyzed: false, total_ms: None }
}

/// 现在的执行计划:只读会话(PG / MySQL 走辅助会话)、限时,纯 EXPLAIN 不执行语句。
pub(super) async fn baseline(state: &AppState, kind: &DbKind, id: &str, sql: &str) -> AppResult<Plan> {
    let run = |q: String| async move { service::query_meta_read_only_timeout(state, kind, id, &q, 10_000, VERIFY_TIMEOUT_MS).await };
    match kind {
        DbKind::Pg => {
            let raw = run(format!("EXPLAIN (FORMAT JSON) {sql}")).await?;
            let text = raw.rows.first().and_then(|r| r.first().cloned().flatten()).unwrap_or_default();
            Ok(plan_of(explain::parse_pg_json(&text).map(|(t, _)| t), raw))
        }
        DbKind::Mysql => match run(format!("EXPLAIN FORMAT=TREE {sql}")).await {
            Ok(raw) => {
                let text: String = raw.rows.iter().filter_map(|r| r.first().cloned().flatten()).collect::<Vec<_>>().join("\n");
                Ok(plan_of(explain::parse_mysql_tree(&text), raw))
            }
            // MariaDB 没有 FORMAT=TREE:退回表格形态
            Err(_) => Ok(plan_of(None, run(format!("EXPLAIN {sql}")).await?)),
        },
        DbKind::Sqlite => {
            let raw = service::query_read_only_timeout(state, kind, id, &format!("EXPLAIN QUERY PLAN {sql}"), 10_000, VERIFY_TIMEOUT_MS).await?;
            Ok(plan_of(explain::parse_sqlite_rows(&raw.rows), raw))
        }
        DbKind::Redis => Err(AppError::new(ErrorKind::Query, t!("core.advisor.err.unsupported_db"))),
    }
}

pub(super) async fn run(state: &AppState, kind: &DbKind, id: &str, sql: &str, cands: &[Candidate], opts: &Options) -> Outcome {
    let n = cands.len();
    match kind {
        DbKind::Pg => pg(state, id, sql, cands).await,
        DbKind::Sqlite => sqlite(state, id, sql, cands, opts.sqlite_max_rows).await,
        _ => Outcome::unverified(n, t!("core.advisor.note.mysql").to_string()),
    }
}

// ------------------------------------------------------------ 比较

fn walk<'a>(n: &'a PlanNode, out: &mut Vec<&'a PlanNode>) {
    out.push(n);
    for c in &n.children {
        walk(c, out);
    }
}

fn count(tree: Option<&PlanNode>, f: impl Fn(&PlanNode) -> bool) -> usize {
    let Some(t) = tree else { return 0 };
    let mut all = vec![];
    walk(t, &mut all);
    all.into_iter().filter(|n| f(n)).count()
}

fn is_sort(n: &PlanNode) -> bool {
    n.label.ends_with("Sort") || n.label.starts_with("Sort") || n.detail.contains("TEMP B-TREE")
}

/// 文本里是否出现了这个索引名(前后不是标识符字符)。
fn mentions(text: &str, name: &str) -> bool {
    let mut from = 0;
    while let Some(p) = text[from..].find(name) {
        let s = from + p;
        let e = s + name.len();
        let ok_l = s == 0 || !text[..s].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_');
        let ok_r = !text[e..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_');
        if ok_l && ok_r {
            return true;
        }
        from = e;
    }
    false
}

fn uses(tree: Option<&PlanNode>, name: &str) -> bool {
    count(tree, |n| mentions(&n.detail, name)) > 0
}

/// 把计划里的临时索引名(hypopg 的 `<12345>btree_t_a`、SQLite 的 `datum_advisor_0`)换成建议的索引名。
fn rename(n: &mut PlanNode, from: &str, to: &str) {
    if n.detail.contains(from) {
        n.detail = n.detail.replace(from, to);
    }
    for c in &mut n.children {
        rename(c, from, to);
    }
}

/// 对比基线与加了索引后的计划。`temp_names` 是计划里的临时索引名。
fn compare(before: Option<&Plan>, mut after: Plan, temp_names: &[(String, String)]) -> (Verify, Vec<String>) {
    let bt = before.and_then(|p| p.tree.as_ref());
    let used: Vec<String> = temp_names.iter().filter(|(tmp, _)| uses(after.tree.as_ref(), tmp)).map(|(_, real)| real.clone()).collect();
    if let Some(tree) = after.tree.as_mut() {
        for (tmp, real) in temp_names {
            rename(tree, tmp, real);
        }
    }
    let at = after.tree.as_ref();
    let cost_before = bt.and_then(|t| t.cost);
    let cost_after = at.and_then(|t| t.cost);
    let improvement_pct = match (cost_before, cost_after) {
        (Some(b), Some(a)) if b > 0.0 => Some((b - a) / b * 100.0),
        _ => None,
    };
    let (fb, fa) = (count(bt, |n| n.full_scan), count(at, |n| n.full_scan));
    let (sb, sa) = (count(bt, is_sort), count(at, is_sort));
    let uses_index = !used.is_empty();
    let verdict = if !uses_index {
        Verdict::NotUsed
    } else {
        match improvement_pct {
            Some(p) if p >= MIN_GAIN * 100.0 => Verdict::Helps,
            Some(_) => Verdict::NoGain,
            // 没有成本数字(SQLite):少了全表扫描或临时排序就算有收益
            None if fa < fb || sa < sb => Verdict::Helps,
            None => Verdict::NoGain,
        }
    };
    let v = Verify {
        verdict,
        cost_before,
        cost_after,
        improvement_pct,
        uses_index,
        full_scans_before: fb,
        full_scans_after: fa,
        sorts_before: sb,
        sorts_after: sa,
        plan: Some(after),
        message: None,
    };
    (v, used)
}

fn failed(message: String, verdict: Verdict) -> Verify {
    Verify {
        verdict,
        cost_before: None,
        cost_after: None,
        improvement_pct: None,
        uses_index: false,
        full_scans_before: 0,
        full_scans_after: 0,
        sorts_before: 0,
        sorts_after: 0,
        plan: None,
        message: Some(message),
    }
}

/// 每个候选单独验证后,把有收益的(两个以上)一起加上再看一次。
fn combo_members(verifies: &[Option<Verify>]) -> Vec<usize> {
    let helps: Vec<usize> = verifies.iter().enumerate().filter(|(_, v)| v.as_ref().is_some_and(|v| v.verdict == Verdict::Helps)).map(|(i, _)| i).collect();
    if helps.len() >= 2 { helps } else { vec![] }
}

// ------------------------------------------------------------ PostgreSQL

const HYPOPG_STATE: &str = "SELECT (SELECT extversion FROM pg_extension WHERE extname = 'hypopg'), \
     EXISTS (SELECT 1 FROM pg_available_extensions WHERE name = 'hypopg')";

/// 一组同时加上的假想索引的结果:(假想索引名, 计划)。
type GroupResult = Result<(Vec<String>, Plan), String>;

async fn pg(state: &AppState, id: &str, sql: &str, cands: &[Candidate]) -> Outcome {
    let n = cands.len();
    let kind = DbKind::Pg;
    let probe = service::query_meta_read_only_timeout(state, &kind, id, HYPOPG_STATE, 1, VERIFY_TIMEOUT_MS).await;
    let (installed, available) = match &probe {
        Ok(r) => {
            let row = r.rows.first();
            (row.and_then(|r| r.first().cloned().flatten()).is_some(), row.and_then(|r| r.get(1).cloned().flatten()).as_deref() == Some("t"))
        }
        Err(_) => (false, false),
    };
    if !installed {
        let mut o = Outcome::unverified(n, t!(if available { "core.advisor.note.hypopg_not_installed" } else { "core.advisor.note.hypopg_unavailable" }).to_string());
        if available {
            o.install_sql = Some("CREATE EXTENSION hypopg;".into());
        }
        return o;
    }
    let note = t!("core.advisor.note.hypopg").to_string();
    if cands.is_empty() {
        return Outcome { mode: Mode::Hypopg, note, install_sql: None, baseline: None, verifies: vec![], combo: None };
    }
    let groups: Vec<Vec<String>> = cands.iter().map(|c| vec![c.ddl.clone()]).collect();
    let (base, results) = match hypo(state, id, sql, groups).await {
        Ok(x) => x,
        Err(e) => {
            let mut o = Outcome::unverified(n, t!("core.advisor.note.failed", error = e.display()).to_string());
            o.mode = Mode::Unverified;
            return o;
        }
    };
    let verifies: Vec<Option<Verify>> = results
        .into_iter()
        .zip(cands)
        .map(|(r, c)| {
            Some(match r {
                Ok((names, plan)) => {
                    let temp: Vec<(String, String)> = names.into_iter().map(|t| (t, c.name.clone())).collect();
                    compare(Some(&base), plan, &temp).0
                }
                Err(e) => failed(e, Verdict::Error),
            })
        })
        .collect();
    let members = combo_members(&verifies);
    let combo = if members.is_empty() {
        None
    } else {
        let ddls: Vec<String> = members.iter().map(|&i| cands[i].ddl.clone()).collect();
        match hypo(state, id, sql, vec![ddls]).await {
            Ok((_, mut rs)) => match rs.pop() {
                Some(Ok((names, plan))) => {
                    let temp: Vec<(String, String)> = names.into_iter().zip(members.iter().map(|&i| cands[i].name.clone())).collect();
                    let (verify, used) = compare(Some(&base), plan, &temp);
                    Some(Combo { names: members.iter().map(|&i| cands[i].name.clone()).collect(), used, verify })
                }
                _ => None,
            },
            Err(_) => None,
        }
    };
    Outcome { mode: Mode::Hypopg, note, install_sql: None, baseline: Some(base), verifies, combo }
}

/// 在辅助会话的只读事务里依次试每组假想索引。返回基线计划与每组的结果。
async fn hypo(state: &AppState, id: &str, sql: &str, groups: Vec<Vec<String>>) -> AppResult<(Plan, Vec<GroupResult>)> {
    let sql = sql.to_string();
    state.pg_pool.with_meta(id, move |c| Box::pin(hypo_session(c, sql, groups))).await?
}

async fn hypo_session(c: &tokio_postgres::Client, sql: String, groups: Vec<Vec<String>>) -> AppResult<(Plan, Vec<GroupResult>)> {
    run_capped(c, "BEGIN READ ONLY", None).await?;
    let r = hypo_steps(c, &sql, &groups).await;
    // 假想索引存在会话内存里,不随事务回滚:回滚后再清一次,确保什么都不留下
    let _ = c.batch_execute("ROLLBACK").await;
    let _ = run_capped(c, "SELECT hypopg_reset()", None).await;
    r
}

async fn explain_json(c: &tokio_postgres::Client, sql: &str) -> AppResult<Plan> {
    let raw = run_capped(c, &format!("EXPLAIN (FORMAT JSON) {sql}"), Some(10_000)).await?;
    let text = raw.rows.first().and_then(|r| r.first().cloned().flatten()).unwrap_or_default();
    Ok(plan_of(explain::parse_pg_json(&text).map(|(t, _)| t), raw))
}

async fn hypo_steps(c: &tokio_postgres::Client, sql: &str, groups: &[Vec<String>]) -> AppResult<(Plan, Vec<GroupResult>)> {
    run_capped(c, &format!("SET LOCAL statement_timeout = {VERIFY_TIMEOUT_MS}"), None).await?;
    run_capped(c, "SELECT hypopg_reset()", None).await?;
    let base = explain_json(c, sql).await?;
    let mut out = vec![];
    for g in groups {
        run_capped(c, "SAVEPOINT datum_hypo", None).await?;
        let r: AppResult<(Vec<String>, Plan)> = async {
            let mut names = vec![];
            for ddl in g {
                let lit = crate::sql::build::literal(ddl, Dialect::Pg);
                let r = run_capped(c, &format!("SELECT indexname FROM hypopg_create_index({lit})"), Some(1)).await?;
                if let Some(name) = r.rows.first().and_then(|r| r.first().cloned().flatten()) {
                    names.push(name);
                }
            }
            Ok((names, explain_json(c, sql).await?))
        }
        .await;
        match &r {
            Ok(_) => run_capped(c, "RELEASE SAVEPOINT datum_hypo", None).await?,
            Err(_) => run_capped(c, "ROLLBACK TO SAVEPOINT datum_hypo", None).await?,
        };
        run_capped(c, "SELECT hypopg_reset()", None).await?;
        out.push(r.map_err(|e| e.display()));
    }
    Ok((base, out))
}

// ------------------------------------------------------------ SQLite

/// SQLite 不验证的原因。
enum SqliteSkip {
    ReadOnly,
    InTransaction,
}

async fn sqlite(state: &AppState, id: &str, sql: &str, cands: &[Candidate], max_rows: u64) -> Outcome {
    let n = cands.len();
    if cands.is_empty() {
        return Outcome { mode: Mode::SqliteTxn, note: t!("core.advisor.note.sqlite", rows = max_rows).to_string(), install_sql: None, baseline: None, verifies: vec![], combo: None };
    }
    let (id2, sql2) = (id.to_string(), sql.to_string());
    let items: Vec<(String, String)> = cands.iter().map(|c| (c.table_sql.clone(), c.col_list(Dialect::Sqlite))).collect();
    let r = state
        .sqlite_pool
        .blocking(move |p| p.with_conn_mut(&id2, |conn| Ok(sqlite_session(conn, &sql2, &items, max_rows))))
        .await;
    let r = match r {
        Ok(r) => r,
        Err(e) => return Outcome::unverified(n, t!("core.advisor.note.failed", error = e.display()).to_string()),
    };
    match r {
        Err(Ok(SqliteSkip::ReadOnly)) => Outcome::unverified(n, t!("core.advisor.note.sqlite_read_only").to_string()),
        Err(Ok(SqliteSkip::InTransaction)) => Outcome::unverified(n, t!("core.advisor.note.sqlite_in_txn").to_string()),
        Err(Err(e)) => Outcome::unverified(n, t!("core.advisor.note.failed", error = e).to_string()),
        Ok((base, singles)) => {
            let verifies: Vec<Option<Verify>> = singles
                .into_iter()
                .zip(cands)
                .map(|(r, c)| {
                    Some(match r {
                        SqliteTry::Done(tmp, plan) => compare(Some(&base), *plan, &[(tmp, c.name.clone())]).0,
                        SqliteTry::TooLarge(rows) => failed(t!("core.advisor.too_large", table = c.table, rows = rows, limit = max_rows).to_string(), Verdict::Skipped),
                        SqliteTry::Failed(e) => failed(e, Verdict::Error),
                    })
                })
                .collect();
            // 组合:再开一次连接锁(单个候选的结果出来后才知道谁有收益)
            let members = combo_members(&verifies);
            let combo = if members.is_empty() {
                None
            } else {
                let items: Vec<(String, String)> = members.iter().map(|&i| (cands[i].table_sql.clone(), cands[i].col_list(Dialect::Sqlite))).collect();
                let (id3, sql3) = (id.to_string(), sql.to_string());
                let names: Vec<String> = members.iter().map(|&i| cands[i].name.clone()).collect();
                let r = state
                    .sqlite_pool
                    .blocking(move |p| p.with_conn_mut(&id3, |conn| Ok(sqlite_group(conn, &sql3, &items))))
                    .await;
                match r {
                    Ok(Ok((temps, plan))) => {
                        let temp: Vec<(String, String)> = temps.into_iter().zip(names.iter().cloned()).collect();
                        let (verify, used) = compare(Some(&base), plan, &temp);
                        Some(Combo { names, used, verify })
                    }
                    _ => None,
                }
            };
            Outcome { mode: Mode::SqliteTxn, note: t!("core.advisor.note.sqlite", rows = max_rows).to_string(), install_sql: None, baseline: Some(base), verifies, combo }
        }
    }
}

enum SqliteTry {
    Done(String, Box<Plan>),
    TooLarge(u64),
    Failed(String),
}

type SqliteResult = Result<(Plan, Vec<SqliteTry>), Result<SqliteSkip, String>>;

fn eqp(conn: &rusqlite::Connection, sql: &str) -> Result<Plan, String> {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).map_err(|e| e.to_string())?;
    let cols: Vec<String> = stmt.column_names().map(|s| s.to_string()).collect();
    let n = cols.len();
    let rows: Vec<Vec<Option<String>>> = stmt
        .query_map([], |r| {
            Ok((0..n)
                .map(|i| {
                    r.get_ref(i).ok().and_then(|v| match v {
                        rusqlite::types::ValueRef::Null => None,
                        rusqlite::types::ValueRef::Integer(x) => Some(x.to_string()),
                        rusqlite::types::ValueRef::Real(x) => Some(x.to_string()),
                        rusqlite::types::ValueRef::Text(t) | rusqlite::types::ValueRef::Blob(t) => Some(String::from_utf8_lossy(t).into_owned()),
                    })
                })
                .collect())
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let tree = explain::parse_sqlite_rows(&rows);
    Ok(plan_of(tree, QueryResult { columns: cols, rows, affected: None }))
}

/// 进度回调限时:超过期限中止当前语句。
fn with_deadline<R>(conn: &mut rusqlite::Connection, f: impl FnOnce(&rusqlite::Connection) -> R) -> R {
    let deadline = Instant::now() + Duration::from_millis(VERIFY_TIMEOUT_MS as u64);
    let _ = conn.progress_handler(10_000, Some(move || Instant::now() >= deadline));
    let r = f(conn);
    let _ = conn.progress_handler(0, None::<fn() -> bool>);
    r
}

/// 在一个事务里建这些索引(临时名 `datum_advisor_N`)、看计划,然后回滚。
fn sqlite_group(conn: &mut rusqlite::Connection, sql: &str, items: &[(String, String)]) -> Result<(Vec<String>, Plan), String> {
    with_deadline(conn, |conn| {
        conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
        let r = (|| {
            let mut names = vec![];
            for (k, (table, cols)) in items.iter().enumerate() {
                let name = format!("datum_advisor_{k}");
                conn.execute_batch(&format!("CREATE INDEX \"{name}\" ON {table} ({cols})")).map_err(|e| e.to_string())?;
                names.push(name);
            }
            Ok((names, eqp(conn, sql)?))
        })();
        // 无论成败都回滚:索引一并撤销
        let rb = conn.execute_batch("ROLLBACK");
        match (r, rb) {
            (Ok(x), Ok(())) => Ok(x),
            (Err(e), _) => Err(e),
            (_, Err(e)) => Err(e.to_string()),
        }
    })
}

fn sqlite_session(conn: &mut rusqlite::Connection, sql: &str, items: &[(String, String)], max_rows: u64) -> SqliteResult {
    if conn.is_readonly(rusqlite::MAIN_DB).unwrap_or(true) || pragma_on(conn, "query_only") {
        return Err(Ok(SqliteSkip::ReadOnly));
    }
    if !conn.is_autocommit() {
        return Err(Ok(SqliteSkip::InTransaction));
    }
    let base = with_deadline(conn, |c| eqp(c, sql)).map_err(Err)?;
    let mut sizes: std::collections::HashMap<String, Result<u64, String>> = std::collections::HashMap::new();
    let mut out = vec![];
    for (table, cols) in items {
        if !sizes.contains_key(table) {
            let n = with_deadline(conn, |c| {
                c.query_row(&format!("SELECT COUNT(*) FROM (SELECT 1 FROM {table} LIMIT {})", max_rows + 1), [], |r| r.get::<_, i64>(0))
                    .map(|n| n as u64)
                    .map_err(|e| e.to_string())
            });
            sizes.insert(table.clone(), n);
        }
        let size = sizes[table].clone();
        out.push(match size {
            Err(e) => SqliteTry::Failed(e),
            Ok(rows) if rows > max_rows => SqliteTry::TooLarge(rows),
            Ok(_) => match sqlite_group(conn, sql, &[(table.clone(), cols.clone())]) {
                Ok((mut names, plan)) => SqliteTry::Done(names.pop().unwrap_or_default(), Box::new(plan)),
                Err(e) => SqliteTry::Failed(e),
            },
        });
    }
    Ok((base, out))
}

fn pragma_on(conn: &rusqlite::Connection, name: &str) -> bool {
    conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get::<_, i64>(0)).map(|v| v != 0).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(label: &str, detail: &str, cost: Option<f64>, full: bool, kids: Vec<PlanNode>) -> PlanNode {
        PlanNode { label: label.into(), detail: detail.into(), cost, full_scan: full, children: kids, ..Default::default() }
    }

    fn plan(t: PlanNode) -> Plan {
        plan_of(Some(t), QueryResult { columns: vec![], rows: vec![], affected: None })
    }

    #[test]
    fn compare_cost_and_index_use() {
        let before = plan(node("Sort", "", Some(200.0), false, vec![node("Seq Scan", "orders", Some(180.0), true, vec![])]));
        let after = plan(node("Index Scan", "orders · index <13001>btree_orders_status", Some(20.0), false, vec![]));
        let (v, used) = compare(Some(&before), after, &[("<13001>btree_orders_status".into(), "idx_orders_status".into())]);
        assert_eq!(v.verdict, Verdict::Helps);
        assert_eq!(used, vec!["idx_orders_status"]);
        assert_eq!(v.improvement_pct.map(|p| p.round()), Some(90.0));
        assert_eq!((v.full_scans_before, v.full_scans_after, v.sorts_before, v.sorts_after), (1, 0, 1, 0));
        assert_eq!(v.plan.unwrap().tree.unwrap().detail, "orders · index idx_orders_status", "临时名换成建议名");
        let unused = plan(node("Seq Scan", "orders", Some(180.0), true, vec![]));
        assert_eq!(compare(Some(&before), unused, &[("<1>x".into(), "y".into())]).0.verdict, Verdict::NotUsed);
        let flat = plan(node("Index Scan", "index <1>x", Some(199.0), false, vec![]));
        assert_eq!(compare(Some(&before), flat, &[("<1>x".into(), "y".into())]).0.verdict, Verdict::NoGain);
    }

    #[test]
    fn sqlite_plans_without_cost() {
        let before = plan(node("QUERY PLAN", "", None, false, vec![node("SCAN", "orders", None, true, vec![]), node("USE", "TEMP B-TREE FOR ORDER BY", None, false, vec![])]));
        let after = plan(node("SEARCH", "orders USING INDEX datum_advisor_0 (status=?)", None, false, vec![]));
        let (v, _) = compare(Some(&before), after, &[("datum_advisor_0".into(), "idx_orders_status".into())]);
        assert_eq!(v.verdict, Verdict::Helps);
        assert!(v.improvement_pct.is_none());
        // datum_advisor_1 不能误认成 datum_advisor_10
        assert!(!mentions("USING INDEX datum_advisor_10", "datum_advisor_1"));
        assert!(mentions("USING COVERING INDEX datum_advisor_1 (a=?)", "datum_advisor_1"));
    }
}
