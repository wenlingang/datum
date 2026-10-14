use crate::commands::AppState;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::sqlite::browse::{self, TableDetail};
use crate::sqlite::client::QueryResult;
use crate::sqlite::edit::{self, CellEdit, RowDelete};
use rust_i18n::t;

/// 用连接 id 找到配置中的文件路径,打开 SQLite 数据库。
pub async fn sqlite_connect(state: &AppState, id: String) -> AppResult<()> {
    let conns = crate::core::repository::ConnectionRepo::new(&state.config_dir).load()?;
    let conn = conns
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| AppError::new(ErrorKind::NotFound, t!("core.err.connection_not_found")))?;
    let path = conn
        .file_path
        .filter(|p| !p.is_empty())
        .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.sqlite_path_missing")))?;
    state.sqlite_pool.connect(&id, &path)
}

pub async fn sqlite_list_objects(state: &AppState, id: String) -> AppResult<Vec<String>> {
    browse::list_tables(&state.sqlite_pool, &id)
}

pub async fn sqlite_query(
    state: &AppState,
    id: String,
    sql: String,
    max_rows: Option<u32>,
) -> AppResult<QueryResult> {
    state.sqlite_pool.query_capped(&id, &sql, max_rows)
}

pub async fn sqlite_disconnect(state: &AppState, id: String) -> AppResult<()> {
    state.sqlite_pool.disconnect(&id);
    Ok(())
}

pub async fn sqlite_import_sql(
    state: &AppState,
    id: String,
    statements: Vec<String>,
) -> AppResult<usize> {
    state.sqlite_pool.import_sql(&id, &statements)
}

pub async fn sqlite_table_detail(
    state: &AppState,
    id: String,
    table: String,
) -> AppResult<TableDetail> {
    browse::table_detail(&state.sqlite_pool, &id, &table)
}

pub async fn sqlite_commit_edits(
    state: &AppState,
    id: String,
    edits: Vec<CellEdit>,
    deletes: Vec<RowDelete>,
) -> AppResult<u64> {
    edit::commit_edits(&state.sqlite_pool, &id, &edits, &deletes)
}
