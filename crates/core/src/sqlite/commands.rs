use crate::commands::AppState;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::sqlite::browse::{self, TableDetail};
use crate::sqlite::client::QueryResult;
use crate::sqlite::edit::{self, CellEdit, RowDelete};
use rust_i18n::t;

/// 连接配置里的 SQLite 文件路径。
pub fn sqlite_file_path(state: &AppState, id: &str) -> AppResult<String> {
    let conns = crate::core::repository::ConnectionRepo::new(&state.config_dir).load()?;
    let conn = conns
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| AppError::new(ErrorKind::NotFound, t!("core.err.connection_not_found")))?;
    conn.file_path
        .filter(|p| !p.is_empty())
        .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.sqlite_path_missing")))
}

/// 用连接 id 找到配置中的文件路径,打开 SQLite 数据库。
pub async fn sqlite_connect(state: &AppState, id: String) -> AppResult<()> {
    let path = sqlite_file_path(state, &id)?;
    state.sqlite_pool.connect(&id, &path)
}

pub async fn sqlite_list_objects(state: &AppState, id: String) -> AppResult<Vec<String>> {
    state.sqlite_pool.blocking(move |p| browse::list_tables(p, &id)).await
}

pub async fn sqlite_query(
    state: &AppState,
    id: String,
    sql: String,
    max_rows: Option<u32>,
) -> AppResult<QueryResult> {
    state.sqlite_pool.blocking(move |p| p.query_capped(&id, &sql, max_rows)).await
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
    state.sqlite_pool.blocking(move |p| p.import_sql(&id, &statements)).await
}

pub async fn sqlite_table_detail(
    state: &AppState,
    id: String,
    table: String,
) -> AppResult<TableDetail> {
    state.sqlite_pool.blocking(move |p| browse::table_detail(p, &id, &table)).await
}

pub async fn sqlite_commit_edits(
    state: &AppState,
    id: String,
    edits: Vec<CellEdit>,
    deletes: Vec<RowDelete>,
) -> AppResult<u64> {
    state.sqlite_pool.blocking(move |p| edit::commit_edits(p, &id, &edits, &deletes)).await
}
