use crate::commands::AppState;
use crate::core::credentials;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::my::browse;
use crate::pg::browse::TableDetail;
use crate::pg::client::QueryResult;
use crate::pg::edit::{CellEdit, RowDelete};
use rust_i18n::t;

pub async fn my_connect(state: &AppState, id: String) -> AppResult<()> {
    let conns = crate::core::repository::ConnectionRepo::new(&state.config_dir).load()?;
    let conn = conns
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| AppError::new(ErrorKind::NotFound, t!("core.err.connection_not_found")))?;
    let password = credentials::load_password(&state.backend, &conn)?;
    state
        .my_pool
        .connect(
            &id,
            &conn.host,
            conn.port,
            &conn.user,
            &password,
            &conn.database,
        )
        .await
}

pub async fn my_list_objects(state: &AppState, id: String) -> AppResult<Vec<String>> {
    browse::list_tables(&state.my_pool, &id).await
}

pub async fn my_list_databases(state: &AppState, id: String) -> AppResult<Vec<String>> {
    browse::list_databases(&state.my_pool, &id).await
}

pub async fn my_query(
    state: &AppState,
    id: String,
    sql: String,
    max_rows: Option<u32>,
) -> AppResult<QueryResult> {
    state.my_pool.query_capped(&id, &sql, max_rows).await
}

pub async fn my_disconnect(state: &AppState, id: String) -> AppResult<()> {
    state.my_pool.disconnect(&id).await;
    Ok(())
}

pub async fn my_import_sql(
    state: &AppState,
    id: String,
    statements: Vec<String>,
) -> AppResult<usize> {
    state.my_pool.import_sql(&id, &statements).await
}

pub async fn my_table_detail(
    state: &AppState,
    id: String,
    table: String,
) -> AppResult<TableDetail> {
    browse::table_detail(&state.my_pool, &id, &table).await
}

pub async fn my_commit_edits(
    state: &AppState,
    id: String,
    edits: Vec<CellEdit>,
    deletes: Vec<RowDelete>,
) -> AppResult<u64> {
    state.my_pool.commit_edits(&id, &edits, &deletes).await
}
