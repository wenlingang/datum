//! 按连接类型分发的统一入口:UI 只和这里打交道,不关心具体驱动。
//!
//! 所有异步函数都需要在 tokio 运行时里执行。

use crate::core::connection::DbKind;
use crate::error::{AppError, AppResult, ErrorKind};
pub use crate::mongo::client::FindPage;
use crate::mongo::convert::MongoResult;
use crate::mongo::parse::MongoOp;
use crate::pg::browse::TableDetail;
use crate::pg::client::QueryResult;
use crate::pg::edit::{CellEdit, RowDelete};
use crate::rds::client::{KeyDetail, KeyValue};
use crate::schema::{self, ColumnsByTable, ErdSchema};
use crate::sql::build::{build_insert_row, ident};
use crate::sql::Dialect;
pub use crate::ssh::SshContext;
use crate::{mongo, my, pg, rds, sqlite, AppState};
use mongodb::bson::{Bson, Document};
use rust_i18n::t;

fn unsupported() -> AppError {
    AppError::new(ErrorKind::Connection, t!("core.err.redis_unsupported"))
}

fn mongo_unsupported() -> AppError {
    AppError::new(ErrorKind::Mongo, t!("core.err.mongo_unsupported"))
}

/// 建立连接(读取已保存配置与密码)。
pub async fn connect(state: &AppState, kind: &DbKind, id: &str) -> AppResult<()> {
    let id = id.to_string();
    match kind {
        DbKind::Pg => pg::commands::pg_connect(state, id).await,
        DbKind::Mysql => my::commands::my_connect(state, id).await,
        DbKind::Sqlite => sqlite::commands::sqlite_connect(state, id).await,
        DbKind::Redis => rds::commands::redis_connect(state, id).await,
        DbKind::Mongo => mongo::commands::mongo_connect(state, &id).await,
    }
}

/// 断开并移除连接(服务器侧会话随之终止)。
pub async fn disconnect(state: &AppState, kind: &DbKind, id: &str) {
    let tunnel_id = id;
    let id = id.to_string();
    let _ = match kind {
        DbKind::Pg => pg::commands::pg_disconnect(state, id).await,
        DbKind::Mysql => my::commands::my_disconnect(state, id).await,
        DbKind::Sqlite => sqlite::commands::sqlite_disconnect(state, id).await,
        DbKind::Redis => {
            // 实时会话的专用连接可能经同一条隧道:先停掉它们(退订、关连接、结束后台任务)
            state.redis_pool.stop_lives(&id).await;
            Ok(())
        }
        DbKind::Mongo => {
            state.mongo_pool.disconnect(&id).await;
            Ok(())
        }
    };
    // 驱动断开后再关隧道
    state.tunnels.close(tunnel_id).await;
}

/// 列出对象:关系库为表名,Redis 为全部 key。
pub async fn list_objects(state: &AppState, kind: &DbKind, id: &str) -> AppResult<Vec<String>> {
    let id = id.to_string();
    match kind {
        DbKind::Pg => pg::commands::pg_list_objects(state, id).await,
        DbKind::Mysql => my::commands::my_list_objects(state, id).await,
        DbKind::Sqlite => sqlite::commands::sqlite_list_objects(state, id).await,
        DbKind::Redis => rds::commands::redis_scan(state, id, "*".into()).await,
        DbKind::Mongo => state.mongo_pool.list_collections(&id).await,
    }
}

/// 实例下的库列表(Redis 固定 0–15,SQLite 无库概念)。
pub async fn list_databases(state: &AppState, kind: &DbKind, id: &str) -> AppResult<Vec<String>> {
    let id = id.to_string();
    match kind {
        DbKind::Pg => pg::commands::pg_list_databases(state, id).await,
        DbKind::Mysql => my::commands::my_list_databases(state, id).await,
        DbKind::Redis => Ok((0..16).map(|i| i.to_string()).collect()),
        DbKind::Mongo => state.mongo_pool.list_databases(&id).await,
        DbKind::Sqlite => Ok(vec![]),
    }
}

/// 执行任意 SQL;`max_rows` 为 None 时不限行数(导出等全量场景)。
pub async fn query(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    sql: &str,
    max_rows: Option<u32>,
) -> AppResult<QueryResult> {
    let (id, sql) = (id.to_string(), sql.to_string());
    match kind {
        DbKind::Pg => pg::commands::pg_query(state, id, sql, max_rows).await,
        DbKind::Mysql => my::commands::my_query(state, id, sql, max_rows).await,
        DbKind::Sqlite => sqlite::commands::sqlite_query(state, id, sql, max_rows).await,
        DbKind::Redis => Err(unsupported()),
        DbKind::Mongo => Err(mongo_unsupported()),
    }
}

/// 在只读模式下执行 SQL:免确认执行(AI 生成、静态检查判定为只读的语句)的最后一道保险。
/// 静态检查无法看穿视图里的写函数、重载运算符等语义副作用,这里让数据库强制拒绝写入:
/// 先把会话切到只读,执行后无论成败都恢复。用会话级设置而不是包一个事务,
/// 这样语句里即使出现 COMMIT,后续语句开的新事务仍然是只读的。
pub async fn query_read_only(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    sql: &str,
    max_rows: Option<u32>,
) -> AppResult<QueryResult> {
    // 每个驱动都在持有同一连接的前提下完成「切只读 → 执行 → 恢复」,
    // 否则并发的另一次执行可能在中间把连接恢复成读写(PG/SQLite 锁住连接,MySQL 独占池中一条连接)
    match kind {
        DbKind::Pg => state.pg_pool.query_read_only(id, sql, max_rows).await,
        DbKind::Mysql => state.my_pool.query_read_only(id, sql, max_rows).await,
        DbKind::Sqlite => state.sqlite_pool.query_read_only(id, sql, max_rows),
        DbKind::Redis => Err(unsupported()),
        DbKind::Mongo => Err(mongo_unsupported()),
    }
}

/// 写操作影响范围预估的结果:条件匹配的行数与前几行。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteImpact {
    pub count: u64,
    /// 整表操作(TRUNCATE / DROP TABLE):数的是表里现有的行
    pub whole_table: bool,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// 预估的统计查询最多跑多久(服务端限时,PG / MySQL)。
pub const IMPACT_TIMEOUT_MS: u32 = 5000;

async fn read_only_bounded(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    sql: &str,
    max_rows: u32,
) -> AppResult<QueryResult> {
    query_read_only_timeout(state, kind, id, sql, max_rows, IMPACT_TIMEOUT_MS).await
}

/// 只读会话 + 服务端限时(PG / MySQL;SQLite 本地执行不限时),超时报 `impact_timeout` 文案。
pub async fn query_read_only_timeout(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    sql: &str,
    max_rows: u32,
    timeout_ms: u32,
) -> AppResult<QueryResult> {
    let r = match kind {
        DbKind::Pg => state.pg_pool.query_read_only_bounded(id, sql, Some(max_rows), Some(timeout_ms)).await,
        DbKind::Mysql => state.my_pool.query_read_only_bounded(id, sql, Some(max_rows), Some(timeout_ms)).await,
        DbKind::Sqlite => state.sqlite_pool.query_read_only(id, sql, Some(max_rows)),
        DbKind::Redis | DbKind::Mongo => return Err(unsupported()),
    };
    r.map_err(|e| {
        let text = e.display().to_ascii_lowercase();
        if text.contains("statement timeout") || text.contains("maximum statement execution time") {
            AppError::new(ErrorKind::Query, t!("core.err.impact_timeout", secs = timeout_ms / 1000))
        } else {
            e
        }
    })
}

/// 预估一条写语句的影响范围:改写成只读的 COUNT 与预览查询,在数据库强制只读的会话里执行。
/// 改写不了的语句(一次改多张表、数据修改 CTE、INSERT 等)返回 None。
pub async fn write_impact(state: &AppState, kind: &DbKind, id: &str, stmt: &str) -> AppResult<Option<WriteImpact>> {
    let Some(dialect) = Dialect::from_kind(kind) else {
        return Ok(None);
    };
    let Some(q) = crate::sql::impact::impact_query(stmt, dialect) else {
        return Ok(None);
    };
    let counted = read_only_bounded(state, kind, id, &q.count_sql, 1).await?;
    let count = counted
        .rows
        .first()
        .and_then(|r| r.first().cloned().flatten())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .ok_or_else(|| AppError::new(ErrorKind::Query, t!("core.err.query_failed")))?;
    let preview = if count == 0 {
        QueryResult { columns: vec![], rows: vec![], affected: None }
    } else {
        read_only_bounded(state, kind, id, &q.rows_sql, crate::sql::impact::PREVIEW_ROWS).await?
    };
    Ok(Some(WriteImpact {
        count,
        whole_table: q.whole_table,
        columns: preview.columns,
        rows: preview.rows,
    }))
}

/// MySQL / MariaDB 改表结构语句的风险评估:只读取版本与 information_schema 里的表信息,
/// 按 Online DDL 规则表判定(见 `sql::alter`)。**不向服务器发送 ALTER**:MySQL 没有 dry-run,
/// 带 `ALGORITHM=INSTANT` 试探一旦支持就会真执行。非 MySQL 或不是改表结构的语句返回 None。
pub async fn alter_assess(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    stmt: &str,
) -> AppResult<Option<crate::sql::alter::AlterAssessment>> {
    use crate::sql::alter::{self, ColumnMeta, Server, TableMeta};
    if *kind != DbKind::Mysql {
        return Ok(None);
    }
    let Some(parsed) = alter::parse(stmt) else {
        return Ok(None);
    };
    let ro = |sql: String, max: u32| async move { read_only_bounded(state, kind, id, &sql, max).await };
    let cell = |r: &QueryResult, row: usize, col: usize| r.rows.get(row).and_then(|x| x.get(col).cloned().flatten());
    let base = ro("SELECT VERSION(), @@foreign_key_checks, DATABASE()".into(), 1).await?;
    let version = cell(&base, 0, 0).unwrap_or_default();
    let server = Server::parse(&version)
        .ok_or_else(|| AppError::new(ErrorKind::Query, t!("core.err.query_failed")).with_detail(version.clone()))?;
    let fk_checks = cell(&base, 0, 1).map(|v| v.trim() != "0");
    let Some(schema) = parsed.schema.clone().or_else(|| cell(&base, 0, 2)) else {
        return Ok(Some(alter::assess(&parsed, &server, None)));
    };
    let lit = |v: &str| crate::sql::build::literal(v, Dialect::Mysql);
    let filter = format!("TABLE_SCHEMA = {} AND TABLE_NAME = {}", lit(&schema), lit(&parsed.table));
    let table = ro(
        format!(
            "SELECT ENGINE, ROW_FORMAT, TABLE_ROWS, DATA_LENGTH + INDEX_LENGTH, TABLE_COLLATION, CREATE_OPTIONS \
             FROM information_schema.TABLES WHERE {filter}"
        ),
        1,
    )
    .await?;
    if table.rows.is_empty() {
        return Ok(Some(alter::assess(&parsed, &server, None)));
    }
    let num = |v: Option<String>| v.and_then(|x| x.trim().parse::<u64>().ok());
    let columns = ro(
        format!(
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, CHARACTER_SET_NAME, COLLATION_NAME, \
             CHARACTER_MAXIMUM_LENGTH, CHARACTER_OCTET_LENGTH, EXTRA, COLUMN_KEY \
             FROM information_schema.COLUMNS WHERE {filter} ORDER BY ORDINAL_POSITION"
        ),
        5000,
    )
    .await?;
    let indexes = ro(
        format!("SELECT COLUMN_NAME, INDEX_TYPE FROM information_schema.STATISTICS WHERE {filter}"),
        5000,
    )
    .await?;
    let charsets = ro("SELECT CHARACTER_SET_NAME, DEFAULT_COLLATE_NAME FROM information_schema.CHARACTER_SETS".into(), 500).await?;
    // INSTANT 加删列的行版本上限(64)只在 8.0.29+ 有;查不到不影响其余判定
    let row_versions = if server.flavor == alter::Flavor::Mysql && server.version >= (8, 0, 29) {
        let name = format!("{schema}/{}", parsed.table);
        ro(format!("SELECT TOTAL_ROW_VERSIONS FROM information_schema.INNODB_TABLES WHERE NAME = {}", lit(&name)), 1)
            .await
            .ok()
            .and_then(|r| num(cell(&r, 0, 0)))
            .map(|v| v as u32)
    } else {
        None
    };
    let lower = |v: Option<String>| v.map(|x| x.to_ascii_lowercase());
    let meta = TableMeta {
        engine: cell(&table, 0, 0),
        row_format: cell(&table, 0, 1),
        rows: num(cell(&table, 0, 2)),
        bytes: num(cell(&table, 0, 3)),
        collation: lower(cell(&table, 0, 4)),
        create_options: cell(&table, 0, 5).unwrap_or_default(),
        columns: (0..columns.rows.len())
            .map(|i| ColumnMeta {
                name: cell(&columns, i, 0).unwrap_or_default(),
                column_type: cell(&columns, i, 1).unwrap_or_default().to_ascii_lowercase(),
                nullable: cell(&columns, i, 2).is_some_and(|v| v.eq_ignore_ascii_case("YES")),
                charset: lower(cell(&columns, i, 3)),
                collation: lower(cell(&columns, i, 4)),
                max_chars: num(cell(&columns, i, 5)),
                octets: num(cell(&columns, i, 6)),
                extra: cell(&columns, i, 7).unwrap_or_default().to_ascii_lowercase(),
                primary: cell(&columns, i, 8).is_some_and(|v| v.eq_ignore_ascii_case("PRI")),
            })
            .collect(),
        indexed_columns: (0..indexes.rows.len()).filter_map(|i| cell(&indexes, i, 0)).collect(),
        has_fulltext: (0..indexes.rows.len()).any(|i| cell(&indexes, i, 1).is_some_and(|t| t.eq_ignore_ascii_case("FULLTEXT"))),
        foreign_key_checks: fk_checks,
        row_versions,
        default_collations: (0..charsets.rows.len())
            .filter_map(|i| Some((lower(cell(&charsets, i, 0))?, lower(cell(&charsets, i, 1))?)))
            .collect(),
    };
    Ok(Some(alter::assess(&parsed, &server, Some(&meta))))
}

/// 一段 SQL 的撤销准备:执行前抓快照、生成反向语句。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoPrep {
    /// 反向语句(按执行顺序;多条写语句时已倒序排好),空表示不能撤销
    pub statements: Vec<String>,
    /// 快照里的行数(会被恢复的行)
    pub rows: usize,
    /// 不能撤销的原因(给界面显示);能撤销为 None
    pub reason: Option<String>,
}

fn unsupported_reason(u: crate::sql::undo::Unsupported) -> String {
    use crate::sql::undo::Unsupported::*;
    match u {
        Statement => t!("core.undo.unsupported_statement"),
        WholeTable => t!("core.undo.whole_table"),
        TooManyRows => t!("core.undo.too_many_rows", max = crate::sql::undo::MAX_ROWS),
        NoPrimaryKey => t!("core.undo.no_primary_key"),
        ChangesPrimaryKey => t!("core.undo.changes_primary_key"),
        UnknownColumns => t!("core.undo.unknown_columns"),
    }
    .to_string()
}

/// 撤销查快照的服务端限时。
const UNDO_TIMEOUT_MS: u32 = 30_000;

/// 执行写语句之前调用:逐条抓取会被改动的原始行,生成反向语句。
/// 只读语句跳过;有一条写语句不能撤销,整段就不提供撤销(给出原因)。
pub async fn undo_prepare(state: &AppState, kind: &DbKind, id: &str, sql: &str) -> AppResult<UndoPrep> {
    use crate::sql::undo::{self, Snapshot, Unsupported};
    let no = |reason: String| UndoPrep { statements: vec![], rows: 0, reason: Some(reason) };
    let Some(dialect) = Dialect::from_kind(kind) else {
        return Ok(no(unsupported_reason(Unsupported::Statement)));
    };
    let mut per_statement: Vec<Vec<String>> = Vec::new();
    let mut rows = 0usize;
    for stmt in crate::sql::stmt::statements(sql) {
        if crate::sql::risk::is_read_only(&stmt) {
            continue;
        }
        let Some(q) = crate::sql::impact::impact_query(&stmt, dialect) else {
            return Ok(no(unsupported_reason(Unsupported::Statement)));
        };
        if q.whole_table {
            return Ok(no(unsupported_reason(Unsupported::WholeTable)));
        }
        let max = undo::MAX_ROWS as u32 + 1;
        let name = crate::sql::impact::unquote(q.table.rsplit('.').next().unwrap_or(&q.table));
        let detail = table_detail(state, kind, id, &name).await.ok();
        // 二进制值让数据库写好字面量再取回(见 sql::undo 模块说明)
        let (sql, literal) = match &detail {
            Some(det) => undo::binary_safe_snapshot(&q.snapshot_sql, &det.columns, dialect),
            None => (q.snapshot_sql.clone(), vec![]),
        };
        let r = query_read_only_timeout(state, kind, id, &sql, max, UNDO_TIMEOUT_MS).await?;
        if r.rows.len() > undo::MAX_ROWS {
            return Ok(no(unsupported_reason(Unsupported::TooManyRows)));
        }
        let primary_key = if matches!(q.op, crate::sql::impact::WriteOp::Update { .. }) {
            detail.map(|d| d.columns.into_iter().filter(|c| c.is_pk).map(|c| c.name).collect()).unwrap_or_default()
        } else {
            vec![]
        };
        let snap = Snapshot { table: q.table, op: q.op, columns: r.columns, rows: r.rows, primary_key, literal };
        rows += snap.rows.len();
        match undo::statements(&snap, dialect) {
            Ok(s) => per_statement.push(s),
            Err(u) => return Ok(no(unsupported_reason(u))),
        }
    }
    if per_statement.is_empty() {
        return Ok(no(unsupported_reason(Unsupported::Statement)));
    }
    // 后执行的先撤销
    let statements = per_statement.into_iter().rev().flatten().collect();
    Ok(UndoPrep { statements, rows, reason: None })
}

pub async fn table_detail(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    table: &str,
) -> AppResult<TableDetail> {
    let (id, table) = (id.to_string(), table.to_string());
    match kind {
        DbKind::Pg => pg::commands::pg_table_detail(state, id, table).await,
        DbKind::Mysql => my::commands::my_table_detail(state, id, table).await,
        DbKind::Sqlite => sqlite::commands::sqlite_table_detail(state, id, table).await,
        DbKind::Redis => Err(unsupported()),
        DbKind::Mongo => Err(mongo_unsupported()),
    }
}

/// 单事务提交暂存的编辑与删除,返回受影响行数。
pub async fn commit_edits(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    edits: Vec<CellEdit>,
    deletes: Vec<RowDelete>,
) -> AppResult<u64> {
    let id = id.to_string();
    match kind {
        DbKind::Pg => pg::commands::pg_commit_edits(state, id, edits, deletes).await,
        DbKind::Mysql => my::commands::my_commit_edits(state, id, edits, deletes).await,
        DbKind::Sqlite => sqlite::commands::sqlite_commit_edits(state, id, edits, deletes).await,
        DbKind::Redis => Err(unsupported()),
        DbKind::Mongo => Err(mongo_unsupported()),
    }
}

/// 单事务导入:全成功提交,任一失败回滚;返回执行的语句数。
pub async fn import_sql(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    statements: Vec<String>,
) -> AppResult<usize> {
    let id = id.to_string();
    match kind {
        DbKind::Pg => pg::commands::pg_import_sql(state, id, statements).await,
        DbKind::Mysql => my::commands::my_import_sql(state, id, statements).await,
        DbKind::Sqlite => sqlite::commands::sqlite_import_sql(state, id, statements).await,
        DbKind::Redis => Err(unsupported()),
        DbKind::Mongo => Err(mongo_unsupported()),
    }
}

/// 建表语句:MySQL 用 SHOW CREATE TABLE,其余由表详情合成。
pub async fn create_table_ddl(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    table: &str,
) -> AppResult<String> {
    if *kind == DbKind::Mysql {
        let r = query(
            state,
            kind,
            id,
            &format!("SHOW CREATE TABLE {}", ident(table, Dialect::Mysql)),
            None,
        )
        .await?;
        return Ok(r
            .rows
            .first()
            .and_then(|row| row.get(1).cloned().flatten())
            .unwrap_or_default());
    }
    let detail = table_detail(state, kind, id, table).await?;
    Ok(crate::sql::build::build_create_table(table, &detail))
}

/// 整库列速览(补全与 AI 上下文用)。
pub async fn fetch_columns(state: &AppState, kind: &DbKind, id: &str) -> AppResult<ColumnsByTable> {
    let dialect = Dialect::from_kind(kind).ok_or_else(unsupported)?;
    let r = query(state, kind, id, schema::columns_sql(dialect), None).await?;
    Ok(schema::rows_to_column_map(&r.rows))
}

/// 整库外键 + 主键(ER 图用)。
pub async fn fetch_erd_schema(state: &AppState, kind: &DbKind, id: &str) -> AppResult<ErdSchema> {
    let dialect = Dialect::from_kind(kind).ok_or_else(unsupported)?;
    let fks = query(state, kind, id, schema::foreign_keys_sql(dialect), None).await?;
    let pks = query(state, kind, id, schema::primary_keys_sql(dialect), None).await?;
    Ok(ErdSchema {
        fks: schema::rows_to_fk_list(&fks.rows),
        pk_by_table: schema::rows_to_pk_map(&pks.rows),
    })
}

/// 补全用的外键:物理外键 + 按命名约定推断的逻辑外键(物理外键已覆盖的列不再推断)。
pub async fn foreign_keys(state: &AppState, kind: &DbKind, id: &str) -> AppResult<Vec<schema::ForeignKey>> {
    let erd = fetch_erd_schema(state, kind, id).await?;
    let columns = fetch_columns(state, kind, id).await?;
    let tables: Vec<String> = columns.keys().cloned().collect();
    let mut fks = erd.fks;
    let inferred = crate::erd::infer_foreign_keys(&tables, &columns, &fks);
    fks.extend(inferred);
    Ok(fks)
}

/// 补全取值:最多给出这么多个不同的值,超过就不算「枚举类」列。
pub const DISTINCT_MAX: usize = 30;
/// 取值抽样的行数与单值长度上限(太长的值不像枚举)。
const DISTINCT_SAMPLE_ROWS: u32 = 5000;
const DISTINCT_MAX_CHARS: usize = 64;
const DISTINCT_TIMEOUT_MS: u32 = 3000;

/// 看起来不是枚举的列类型:时间、浮点、大文本 / 二进制 / JSON。
fn not_enum_like(data_type: &str) -> bool {
    let t = data_type.to_ascii_lowercase();
    ["time", "date", "float", "double", "real", "numeric", "decimal", "json", "blob", "bytea", "binary", "uuid", "text[]", "xml"]
        .iter()
        .any(|k| t.contains(k))
}

/// 低基数列的实际取值(补全用):在前几千行里取不同的值,不超过 [`DISTINCT_MAX`] 个才返回,
/// 否则(或类型不像枚举)返回 None。只读会话 + 限时,不会拖慢编辑器。
pub async fn distinct_values(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    table: &str,
    column: &str,
    data_type: &str,
) -> AppResult<Option<Vec<String>>> {
    let dialect = Dialect::from_kind(kind).ok_or_else(unsupported)?;
    if not_enum_like(data_type) {
        return Ok(None);
    }
    let sql = format!(
        "SELECT DISTINCT v FROM (SELECT {col} AS v FROM {table} LIMIT {DISTINCT_SAMPLE_ROWS}) s WHERE v IS NOT NULL LIMIT {}",
        DISTINCT_MAX + 1,
        col = ident(column, dialect),
        table = ident(table, dialect),
    );
    let r = query_read_only_timeout(state, kind, id, &sql, DISTINCT_MAX as u32 + 1, DISTINCT_TIMEOUT_MS).await?;
    let mut values: Vec<String> = r.rows.into_iter().filter_map(|row| row.into_iter().next().flatten()).collect();
    if values.len() > DISTINCT_MAX || values.iter().any(|v| v.chars().count() > DISTINCT_MAX_CHARS || v.contains('\n')) {
        return Ok(None);
    }
    values.sort_by(|a, b| crate::grid::compare_cells(a, b));
    Ok(Some(values))
}

/// JSON 键名推断:取前这么多行(非 NULL)做样本。
pub const JSON_SAMPLE_ROWS: u32 = 200;
/// 单个值超过这个大小(PG 存储尺寸,字节)不取样:几个大文档就能拖慢补全,而键名多半在小文档里也看得到。
const JSON_SAMPLE_MAX_BYTES: u32 = 256 * 1024;
const JSON_TIMEOUT_MS: u32 = 3000;

/// 补全 JSON 键名用:从前若干行取样,推断各层出现过的键(只读会话 + 限时)。
/// 只做 PG;其他库返回 None。界面按 连接 / 表 / 列 缓存整棵键树,逐级补全时在本地沿路径查。
pub async fn json_key_tree(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    table: &str,
    column: &str,
) -> AppResult<Option<crate::json::KeyTree>> {
    if *kind != DbKind::Pg {
        return Ok(None);
    }
    let col = ident(column, Dialect::Pg);
    // ::text 让 json / jsonb / 存 JSON 的文本列都以文本取回;pg_column_size 只看存储头,不解压大值
    let sql = format!(
        "SELECT {col}::text FROM {table} WHERE {col} IS NOT NULL AND pg_column_size({col}) <= {JSON_SAMPLE_MAX_BYTES} LIMIT {JSON_SAMPLE_ROWS}",
        table = ident(table, Dialect::Pg),
    );
    let r = query_read_only_timeout(state, kind, id, &sql, JSON_SAMPLE_ROWS, JSON_TIMEOUT_MS).await?;
    let samples: Vec<String> = r.rows.into_iter().filter_map(|row| row.into_iter().next().flatten()).collect();
    Ok(Some(crate::json::infer_keys(samples.iter().map(String::as_str))))
}

/// 导出整个数据库为 INSERT 语句:逐表查询全部行,表间用注释分隔;单表失败不影响其他表。
pub async fn export_database(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    tables: &[String],
) -> String {
    let Some(dialect) = Dialect::from_kind(kind) else {
        return String::new();
    };
    let mut parts = Vec::new();
    for t in tables {
        match query(
            state,
            kind,
            id,
            &format!("SELECT * FROM {}", ident(t, dialect)),
            None,
        )
        .await
        {
            Ok(r) => {
                parts.push(
                    t!("core.export.table_rows", table = t, count = r.rows.len()).to_string(),
                );
                if !r.rows.is_empty() {
                    parts.push(
                        r.rows
                            .iter()
                            .map(|row| build_insert_row(t, &r.columns, row, dialect))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                }
            }
            Err(e) => parts
                .push(t!("core.export.table_failed", table = t, error = e.display()).to_string()),
        }
    }
    parts.join("\n\n")
}

pub async fn redis_get_key(
    state: &AppState,
    id: &str,
    key: &str,
) -> AppResult<(KeyValue, KeyDetail)> {
    let value = rds::commands::redis_get_key(state, id.to_string(), key.to_string()).await?;
    let detail = rds::commands::redis_key_detail(state, id.to_string(), key.to_string()).await?;
    Ok((value, detail))
}

/// 连接配置里的 Redis 库号(0–15,解析不了按 0)。
fn redis_db(state: &AppState, id: &str) -> u8 {
    crate::core::repository::ConnectionRepo::new(&state.config_dir)
        .load()
        .ok()
        .and_then(|all| all.into_iter().find(|c| c.id == id))
        .and_then(|c| c.database.trim().parse().ok())
        .unwrap_or(0)
}

pub async fn redis_overview(state: &AppState, id: &str) -> AppResult<rds::analysis::Overview> {
    state.redis_pool.overview(id, redis_db(state, id)).await
}

pub async fn redis_slowlog(state: &AppState, id: &str) -> AppResult<Vec<rds::analysis::SlowEntry>> {
    state.redis_pool.slowlog(id, 50).await
}

pub async fn redis_analysis_start(state: &AppState, id: &str) -> AppResult<()> {
    state.redis_pool.start_analysis(id).await
}

pub async fn redis_analysis_report(state: &AppState, id: &str) -> Option<rds::analysis::Report> {
    state.redis_pool.analysis_report(id).await
}

pub async fn redis_analysis_cancel(state: &AppState, id: &str) {
    state.redis_pool.cancel_analysis(id).await
}

pub async fn redis_pubsub_start(state: &AppState, id: &str, subs: Vec<rds::live::Sub>) -> AppResult<String> {
    state.redis_pool.start_pubsub(id, subs, Default::default()).await
}

pub async fn redis_monitor_start(state: &AppState, id: &str, secs: u64, filter: rds::live::MonitorFilter) -> AppResult<String> {
    state.redis_pool.start_monitor(id, secs, filter, Default::default()).await
}

pub async fn redis_live_poll(state: &AppState, sid: &str, since: u64, limit: usize) -> AppResult<rds::live::LivePoll> {
    Ok(state.redis_pool.live(sid).await?.poll(since, limit))
}

/// 一条消息保存下来的完整内容;已被丢掉 / 清空时为 None。
pub async fn redis_live_detail(state: &AppState, sid: &str, seq: u64) -> AppResult<Option<rds::live::LiveDetail>> {
    Ok(state.redis_pool.live(sid).await?.detail(seq))
}

pub async fn redis_live_subscribe(state: &AppState, sid: &str, subs: &[rds::live::Sub]) -> AppResult<()> {
    state.redis_pool.live(sid).await?.subscribe(subs).await
}

pub async fn redis_live_unsubscribe(state: &AppState, sid: &str, subs: &[rds::live::Sub]) -> AppResult<()> {
    state.redis_pool.live(sid).await?.unsubscribe(subs).await
}

pub async fn redis_live_filter(state: &AppState, sid: &str, filter: rds::live::MonitorFilter) -> AppResult<()> {
    state.redis_pool.live(sid).await?.set_filter(filter);
    Ok(())
}

pub async fn redis_live_clear(state: &AppState, sid: &str) -> AppResult<()> {
    state.redis_pool.live(sid).await?.clear();
    Ok(())
}

/// 停止接收(缓冲留着,还能看详情)。
pub async fn redis_live_stop(state: &AppState, sid: &str) {
    state.redis_pool.stop_live(sid).await
}

/// 停止并移除会话,释放缓冲。
pub async fn redis_live_close(state: &AppState, sid: &str) {
    state.redis_pool.close_live(sid).await
}

pub async fn redis_publish(state: &AppState, id: &str, channel: &str, message: &str) -> AppResult<i64> {
    state.redis_pool.publish(id, channel, message).await
}

/// SQLite:记下连接上 legacy_alter_table / foreign_keys 的当前值(编辑器多语句执行前)。
pub fn sqlite_session_flags(state: &AppState, id: &str) -> AppResult<sqlite::client::SessionFlags> {
    state.sqlite_pool.session_flags(id)
}

/// SQLite:多语句执行出错停下后,回滚未结束的事务并恢复执行前的设置;返回是否回滚了事务。
pub fn sqlite_recover(state: &AppState, id: &str, before: &sqlite::client::SessionFlags) -> AppResult<bool> {
    state.sqlite_pool.recover(id, before)
}

/// SQLite 改表结构:读出表的现状(列、外键、相关对象),界面据此填表单。
pub fn sqlite_alter_load(state: &AppState, id: &str, table: &str) -> AppResult<sqlite::alter::TableSnapshot> {
    state.sqlite_pool.with_conn(id, |c| sqlite::alter::snapshot(c, table))
}

/// SQLite 改表结构:按当前结构生成脚本(预览)。
pub fn sqlite_alter_preview(state: &AppState, id: &str, plan: &sqlite::alter::AlterPlan) -> AppResult<sqlite::alter::AlterScript> {
    state.sqlite_pool.with_conn(id, |c| sqlite::alter::generate(&sqlite::alter::snapshot(c, &plan.table)?, plan))
}

/// SQLite 改表结构:重新生成并核对与预览一致后,在一个事务里执行。
pub fn sqlite_alter_apply(
    state: &AppState,
    id: &str,
    plan: &sqlite::alter::AlterPlan,
    expected_sql: &str,
) -> AppResult<sqlite::alter::ApplyOutcome> {
    state.sqlite_pool.with_conn(id, |c| sqlite::alter::preview_and_apply(c, plan, expected_sql))
}

/// SQLite 导入 CSV / JSON:预览文件开头(列、推断类型、样例行)。
pub fn sqlite_import_preview(src: &sqlite::import_data::SourceOptions) -> AppResult<sqlite::import_data::Preview> {
    sqlite::import_data::preview(src)
}

/// SQLite 导入 CSV / JSON:后台开始导入(用 `sqlite_import_progress` 轮询)。
pub fn sqlite_import_start(state: &AppState, id: &str, plan: sqlite::import_data::ImportPlan) -> AppResult<()> {
    let path = sqlite::commands::sqlite_file_path(state, id)?;
    state.sqlite_imports.start(id, &path, plan)
}

/// SQLite 导入的当前进度;没开始过为 None。
pub fn sqlite_import_progress(state: &AppState, id: &str) -> Option<sqlite::import_data::Progress> {
    state.sqlite_imports.progress(id)
}

/// 取消进行中的导入(事务回滚,表也不会留下)。
pub fn sqlite_import_cancel(state: &AppState, id: &str) {
    state.sqlite_imports.cancel(id)
}

/// 查看分析排行榜里的 key(集群模式下 `node` 是它所在的主节点)。
pub async fn redis_key_on(state: &AppState, id: &str, node: Option<&str>, key: &str) -> AppResult<rds::client::KeyValue> {
    state.redis_pool.get_value_on(id, node, key).await
}

/// 删除分析排行榜里的 key(UNLINK,老版本 DEL)。返回删掉的个数(已不存在为 0)。
pub async fn redis_delete_key(state: &AppState, id: &str, node: Option<&str>, key: &str) -> AppResult<i64> {
    state.redis_pool.delete_key(id, node, key).await
}

/// key 详情(TTL、大小);集群模式下在 `node` 上读。
pub async fn redis_key_detail_on(state: &AppState, id: &str, node: Option<&str>, key: &str) -> AppResult<rds::client::KeyDetail> {
    state.redis_pool.key_detail_on(id, node, key).await
}

/// 元素级编辑(增删改字段 / 成员 / 下标,Stream 追加条目)。
pub async fn redis_edit(
    state: &AppState,
    id: &str,
    node: Option<&str>,
    key: &str,
    edit: &rds::edit::KeyEdit,
) -> AppResult<rds::edit::EditResult> {
    state.redis_pool.edit_key(id, node, key, edit).await
}

/// 设置(`Some(秒)`)/ 移除(`None`)TTL。
pub async fn redis_set_ttl(state: &AppState, id: &str, node: Option<&str>, key: &str, seconds: Option<i64>) -> AppResult<()> {
    state.redis_pool.set_ttl(id, node, key, seconds).await
}

/// 重命名 key(RENAMENX,目标已存在时报错)。
pub async fn redis_rename(state: &AppState, id: &str, node: Option<&str>, key: &str, new_key: &str) -> AppResult<()> {
    state.redis_pool.rename_key(id, node, key, new_key).await
}

/// 新建 key(已存在时报错)。
pub async fn redis_create_key(state: &AppState, id: &str, node: Option<&str>, spec: &rds::edit::NewKey) -> AppResult<()> {
    state.redis_pool.create_key(id, node, spec).await
}

/// Stream 按 ID 范围取一页(`rev` 为倒序,最新的在前)。
pub async fn redis_stream_range(
    state: &AppState,
    id: &str,
    node: Option<&str>,
    key: &str,
    q: &rds::edit::StreamRange,
) -> AppResult<rds::edit::StreamPage> {
    state.redis_pool.stream_range(id, node, key, q).await
}

/// Stream 的消费组、消费者与待确认摘要。
pub async fn redis_stream_groups(state: &AppState, id: &str, node: Option<&str>, key: &str) -> AppResult<Vec<rds::edit::StreamGroup>> {
    state.redis_pool.stream_groups(id, node, key).await
}

pub async fn redis_exec(state: &AppState, id: &str, command: &str) -> AppResult<String> {
    rds::commands::redis_exec(state, id.to_string(), command.to_string()).await
}

/// 测试连通性(用表单当前参数,不保存)。配了 SSH 时先建一条临时隧道,测完即关。
pub async fn test_connection(
    conn: &crate::core::connection::Connection,
    password: &str,
    ssh: &SshContext,
) -> AppResult<()> {
    if conn.kind == DbKind::Mongo {
        return mongo::client::test_connect(&mongo::uri::connect_uris(conn, password)).await;
    }
    let (target, _tunnel) = crate::ssh::resolve(conn, ssh).await?;
    let kind = match conn.kind {
        DbKind::Pg => "pg",
        DbKind::Mysql => "mysql",
        DbKind::Redis => "redis",
        DbKind::Sqlite => "sqlite",
        DbKind::Mongo => "mongo",
    };
    crate::commands::test_connection(
        kind.into(),
        target,
        conn.user.clone(),
        password.into(),
        conn.database.clone(),
        conn.file_path.clone(),
        conn.ssl_mode,
    )
    .await
}

/// 用表单当前参数(不保存)列出实例下的库:新建/编辑连接时下拉选库。
/// Redis 固定 0–15;SQLite 没有库的概念,返回空。
pub async fn probe_databases(
    conn: &crate::core::connection::Connection,
    password: &str,
    ssh: &SshContext,
) -> AppResult<Vec<String>> {
    match conn.kind {
        DbKind::Pg => {
            let (target, _tunnel) = crate::ssh::resolve(conn, ssh).await?;
            pg::client::probe_databases(&target, &conn.user, password, &conn.database, conn.ssl_mode).await
        }
        DbKind::Mysql => {
            let (target, _tunnel) = crate::ssh::resolve(conn, ssh).await?;
            my::client::probe_databases(&target, &conn.user, password, conn.ssl_mode).await
        }
        DbKind::Redis => Ok((0..16).map(|i| i.to_string()).collect()),
        DbKind::Sqlite | DbKind::Mongo => Ok(vec![]),
    }
}

pub async fn mongo_find_page(
    state: &AppState,
    id: &str,
    coll: &str,
    page: FindPage,
) -> AppResult<MongoResult> {
    state.mongo_pool.find_page(id, coll, page).await
}

/// (数量, 是否精确):无过滤条件时为估算值。
pub async fn mongo_count(
    state: &AppState,
    id: &str,
    coll: &str,
    filter: Option<Document>,
) -> AppResult<(u64, bool)> {
    state.mongo_pool.count(id, coll, filter).await
}

pub async fn mongo_execute(
    state: &AppState,
    id: &str,
    ops: Vec<MongoOp>,
    max_rows: u32,
    read_only: bool,
) -> AppResult<MongoResult> {
    state.mongo_pool.execute(id, ops, max_rows, read_only).await
}

/// 按 `_id` 取完整文档(文档编辑器用,忽略当前页的 Projection)。
pub async fn mongo_find_by_id(
    state: &AppState,
    id: &str,
    coll: &str,
    doc_id: Bson,
) -> AppResult<Option<String>> {
    state.mongo_pool.find_by_id(id, coll, doc_id).await
}

pub async fn mongo_replace_one(
    state: &AppState,
    id: &str,
    coll: &str,
    doc_id: Bson,
    doc: Document,
) -> AppResult<()> {
    state.mongo_pool.replace_one(id, coll, doc_id, doc).await
}

pub async fn mongo_insert_one(
    state: &AppState,
    id: &str,
    coll: &str,
    doc: Document,
) -> AppResult<Bson> {
    state.mongo_pool.insert_one(id, coll, doc).await
}

pub async fn mongo_delete_one(
    state: &AppState,
    id: &str,
    coll: &str,
    doc_id: Bson,
) -> AppResult<()> {
    state.mongo_pool.delete_one(id, coll, doc_id).await
}

/// 诊断:某个库支持的分区。
pub fn diag_sections(kind: &DbKind) -> Vec<crate::diag::SectionInfo> {
    crate::diag::sections(kind)
}

/// 诊断:查询一个分区。
pub async fn diag_run(state: &AppState, kind: &DbKind, id: &str, section: &str) -> AppResult<crate::diag::DiagResult> {
    crate::diag::run(state, kind, id, section).await
}

/// 诊断:执行取消 / 终止会话、VACUUM 等操作。
pub async fn diag_act(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    action: crate::diag::Action,
    args: &[String],
) -> AppResult<()> {
    crate::diag::act(state, kind, id, action, args).await
}
