use crate::commands::AppState;
use crate::core::connection::Connection;
use crate::core::credentials;
use crate::core::repository::ConnectionRepo;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::rds::client::{KeyDetail, KeyValue};
use crate::ssh::Target;
use rust_i18n::t;

fn build_url(conn: &Connection, target: &Target, password: &str) -> String {
    crate::rds::client::build_url(
        &target.host,
        target.port,
        &conn.user,
        password,
        &conn.database,
        conn.ssl_mode,
    )
}

pub async fn redis_connect(state: &AppState, id: String) -> AppResult<()> {
    let conns = ConnectionRepo::new(&state.config_dir).load()?;
    let conn = conns
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| AppError::new(ErrorKind::NotFound, t!("core.err.connection_not_found")))?;
    let password = credentials::load_password(&state.backend, &conn)?;
    let (target, tunnel) = crate::ssh::resolve(&conn, &state.ssh_context(&conn)?).await?;
    let url = build_url(&conn, &target, &password);
    state
        .redis_pool
        .connect_target(&id, &target, &url, conn.ssl_mode)
        .await?;
    state.tunnels.set(&id, tunnel).await;
    Ok(())
}

pub async fn redis_scan(state: &AppState, id: String, pattern: String) -> AppResult<Vec<String>> {
    let p = if pattern.trim().is_empty() {
        "*".to_string()
    } else {
        pattern
    };
    state.redis_pool.scan(&id, &p).await
}

pub async fn redis_get_key(state: &AppState, id: String, key: String) -> AppResult<KeyValue> {
    state.redis_pool.get_value(&id, &key).await
}

pub async fn redis_key_detail(state: &AppState, id: String, key: String) -> AppResult<KeyDetail> {
    state.redis_pool.key_detail(&id, &key).await
}

pub async fn redis_exec(state: &AppState, id: String, command: String) -> AppResult<String> {
    state.redis_pool.exec(&id, &command).await
}
