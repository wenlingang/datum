//! 界面调用的入口:分析表结构给出自动方案、预览、生成 INSERT 文本、后台插入、AI 建议。
//! 读库一律走只读的元数据查询;写入另开连接(见 [`super::job`])。

use super::ai::{self, AiSuggestion};
use super::engine::{insert_sql, Context, Engine};
use super::gen::{date_to_days, Locale};
use super::infer::{self, ColumnHints};
use super::job::{MyWriter, PgWriter, Progress, SqliteWriter, Writer};
use super::meta::{self, BaseType, TableMeta};
use super::plan::{Plan, PREVIEW_ROWS, SQL_MAX_ROWS};
use super::rng::random_seed;
use super::Generator;
use crate::core::connection::DbKind;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::service;
use crate::sql::build::ident;
use crate::sql::Dialect;
use crate::AppState;
use chrono::Datelike;
use rust_i18n::t;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// 父表键最多取这么多个来抽样。
const FK_SAMPLE: u32 = 10_000;
/// 唯一列现有值最多读这么多个(更大的表靠生成值本身的随机性避开冲突,撞了由数据库报错)。
const EXISTING_MAX: u32 = 200_000;

fn unsupported() -> AppError {
    AppError::new(ErrorKind::Connection, t!("core.err.redis_unsupported"))
}

fn dialect(kind: &DbKind) -> AppResult<Dialect> {
    Dialect::from_kind(kind).ok_or_else(unsupported)
}

/// 今天(天数,1970-01-01 为 0)。
pub fn today() -> i64 {
    let d = chrono::Local::now().date_naive();
    date_to_days(d.year() as i64, d.month(), d.day())
}

fn first_cells(r: crate::pg::client::QueryResult) -> Vec<String> {
    r.rows.into_iter().filter_map(|row| row.into_iter().next().flatten()).collect()
}

/// 读出表的造数视图(列、约束、外键)。
pub async fn load_meta(state: &AppState, kind: &DbKind, id: &str, table: &str) -> AppResult<TableMeta> {
    let d = dialect(kind)?;
    let cols = service::query_meta(state, kind, id, &meta::columns_sql(d, table), None).await?;
    if cols.rows.is_empty() {
        return Err(AppError::new(ErrorKind::NotFound, t!("core.err.import_table_missing", table = table)));
    }
    // CHECK 读不出来(老版本 MySQL 没有 CHECK_CONSTRAINTS)不影响其余
    let checks = service::query_meta(state, kind, id, &meta::checks_sql(d, table), None).await.map(first_cells).unwrap_or_default();
    let detail = service::table_detail(state, kind, id, table).await?;
    let fks = service::fetch_erd_schema(state, kind, id).await.map(|e| e.fks).unwrap_or_default();
    Ok(meta::build_meta(d, table, &cols.rows, &checks, &detail.indexes, &fks))
}

/// 需要用户留意的情况。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ParentEmpty {
    pub columns: Vec<String>,
    pub parent: String,
    /// 外键列都可空(会写 NULL)
    pub nullable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub meta: TableMeta,
    pub plan: Plan,
    /// 父表还没有数据的外键
    pub parent_empty: Vec<ParentEmpty>,
    /// 生成器类型(下拉顺序)
    pub kinds: Vec<&'static str>,
}

/// 分析一张表,给出自动方案(行数 100、随机种子)。
pub async fn analyze(state: &AppState, kind: &DbKind, id: &str, table: &str, locale: Locale) -> AppResult<Analysis> {
    let d = dialect(kind)?;
    let meta = load_meta(state, kind, id, table).await?;
    let mut hints: HashMap<String, ColumnHints> = HashMap::new();
    for c in &meta.columns {
        if c.auto.is_some() || c.fk.is_some() {
            continue;
        }
        let mut h = ColumnHints::default();
        if c.values.is_empty() && c.base.is_textual() && !c.unique && infer::semantic_is_enumish(&c.name) {
            h.distinct = service::distinct_values(state, kind, id, table, &c.name, &c.data_type).await.ok().flatten();
        }
        if c.unique && matches!(c.base, BaseType::Int { .. }) {
            let sql = format!("SELECT MAX({}) FROM {}", ident(&c.name, d), ident(table, d));
            h.max_int = service::query_meta(state, kind, id, &sql, Some(1))
                .await
                .ok()
                .and_then(|r| first_cells(r).into_iter().next())
                .and_then(|v| v.trim().parse().ok());
        }
        hints.insert(c.name.clone(), h);
    }
    let columns = infer::infer_table(&meta, &hints, today());
    let mut parent_empty = vec![];
    for g in &meta.fks {
        let sql = format!("SELECT 1 FROM {} LIMIT 1", ident(&g.ref_table, d));
        let empty = service::query_meta(state, kind, id, &sql, Some(1)).await.map(|r| r.rows.is_empty()).unwrap_or(false);
        if empty {
            parent_empty.push(ParentEmpty {
                columns: g.columns.clone(),
                parent: g.ref_table.clone(),
                nullable: g.columns.iter().all(|c| meta.column(c).is_some_and(|m| m.nullable)),
            });
        }
    }
    let plan = Plan { table: table.to_string(), rows: 100, seed: random_seed(), locale, columns };
    Ok(Analysis { meta, plan, parent_empty, kinds: infer::KINDS.to_vec() })
}

/// 去重保序。
fn dedupe(rows: Vec<Vec<Option<String>>>) -> Vec<Vec<Option<String>>> {
    let mut seen = HashSet::new();
    rows.into_iter().filter(|r| seen.insert(r.clone())).collect()
}

/// 读生成要用的库里现状:父表的键、唯一列已有的值。
pub async fn load_context(state: &AppState, kind: &DbKind, id: &str, meta: &TableMeta, plan: &Plan) -> AppResult<Context> {
    let d = dialect(kind)?;
    let gen_of = |name: &str| plan.columns.iter().find(|c| c.name == name).map(|c| &c.generator);
    let mut ctx = Context::default();
    for g in &meta.fks {
        if !g.columns.iter().any(|c| gen_of(c) == Some(&Generator::ForeignKey)) {
            continue;
        }
        // SQLite 引用隐式主键时被引列为空:取父表主键补齐
        let mut refs = g.ref_columns.clone();
        if refs.iter().any(String::is_empty) {
            let pd = service::table_detail(state, kind, id, &g.ref_table).await?;
            let pk: Vec<String> = pd.columns.iter().filter(|c| c.is_pk).map(|c| c.name.clone()).collect();
            for (i, r) in refs.iter_mut().enumerate() {
                if r.is_empty() {
                    *r = pk.get(i).cloned().unwrap_or_else(|| "rowid".into());
                }
            }
        }
        let cols: Vec<String> = refs.iter().map(|c| ident(c, d)).collect();
        let sql = format!(
            "SELECT {} FROM {} WHERE {} LIMIT {FK_SAMPLE}",
            cols.join(", "),
            ident(&g.ref_table, d),
            cols.iter().map(|c| format!("{c} IS NOT NULL")).collect::<Vec<_>>().join(" AND "),
        );
        let r = service::query_meta(state, kind, id, &sql, Some(FK_SAMPLE)).await?;
        ctx.fk_keys.insert(g.constraint.clone(), dedupe(r.rows));
    }
    for c in meta.columns.iter().filter(|c| c.unique && c.auto.is_none()) {
        match gen_of(&c.name) {
            None | Some(Generator::Skip | Generator::Null | Generator::Sequence { .. } | Generator::ForeignKey) => continue,
            _ => {}
        }
        let sql = format!(
            "SELECT {col} FROM {t} WHERE {col} IS NOT NULL LIMIT {EXISTING_MAX}",
            col = ident(&c.name, d),
            t = ident(&meta.table, d)
        );
        let values = service::query_meta(state, kind, id, &sql, Some(EXISTING_MAX)).await.map(first_cells).unwrap_or_default();
        ctx.existing.insert(c.name.clone(), values.into_iter().collect());
    }
    Ok(ctx)
}

async fn engine(state: &AppState, kind: &DbKind, id: &str, plan: &Plan) -> AppResult<Engine> {
    let d = dialect(kind)?;
    let meta = load_meta(state, kind, id, &plan.table).await?;
    let ctx = load_context(state, kind, id, &meta, plan).await?;
    Engine::new(plan, &meta, &ctx, d)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// 预览:实际插入时的前 [`PREVIEW_ROWS`] 行(同一种子,结果一致)。
pub async fn preview(state: &AppState, kind: &DbKind, id: &str, plan: &Plan) -> AppResult<Preview> {
    let mut e = engine(state, kind, id, plan).await?;
    let mut rows = vec![];
    for _ in 0..PREVIEW_ROWS.min(plan.rows) {
        rows.push(e.next_row()?.iter().map(|v| v.display()).collect());
    }
    Ok(Preview { columns: e.columns().to_vec(), rows })
}

/// 整份 INSERT 文本(不超过 [`SQL_MAX_ROWS`] 行;每 100 行一条语句)。
pub async fn insert_script(state: &AppState, kind: &DbKind, id: &str, plan: &Plan) -> AppResult<String> {
    if plan.rows > SQL_MAX_ROWS {
        return Err(AppError::new(ErrorKind::Query, t!("testdata.err.sql_too_many", max = SQL_MAX_ROWS)));
    }
    let d = dialect(kind)?;
    let mut e = engine(state, kind, id, plan).await?;
    let columns = e.columns().to_vec();
    let mut parts = vec![];
    while e.remaining() > 0 {
        let n = e.remaining().min(100);
        let mut rows = vec![];
        for _ in 0..n {
            rows.push(e.next_row()?);
        }
        parts.push(insert_sql(&plan.table, &columns, &rows, d));
    }
    Ok(parts.join("\n\n"))
}

/// 后台插入(用 [`progress`] 轮询)。方案校验与读现状在这里同步完成,有错直接返回。
pub async fn start(state: &AppState, kind: &DbKind, id: &str, plan: &Plan) -> AppResult<()> {
    if state.testdata_jobs.running(id) {
        return Err(AppError::new(ErrorKind::Query, t!("testdata.err.running")));
    }
    let e = engine(state, kind, id, plan).await?;
    let rt = tokio::runtime::Handle::current();
    let writer: Box<dyn Writer> = match kind {
        DbKind::Pg => Box::new(PgWriter::new(state.pg_pool.dial_dedicated(id).await?, rt)),
        DbKind::Mysql => Box::new(MyWriter::new(state.my_pool.dedicated_conn(id).await?, rt)),
        DbKind::Sqlite => Box::new(SqliteWriter::open(&crate::sqlite::commands::sqlite_file_path(state, id)?)?),
        DbKind::Redis => return Err(unsupported()),
    };
    state.testdata_jobs.start(id, e, plan.table.clone(), writer)
}

pub fn progress(state: &AppState, id: &str) -> Option<Progress> {
    state.testdata_jobs.progress(id)
}

pub fn cancel(state: &AppState, id: &str) {
    state.testdata_jobs.cancel(id)
}

/// 让 AI 按建表语句建议各列的生成器(只发结构)。
pub async fn ai_suggest(state: &AppState, kind: &DbKind, id: &str, table: &str) -> Result<AiSuggestion, String> {
    let meta = load_meta(state, kind, id, table).await.map_err(|e| e.display())?;
    let ddl = service::create_table_ddl(state, kind, id, table).await.map_err(|e| e.display())?;
    ai::suggest(state, &meta, &ddl, today()).await
}
