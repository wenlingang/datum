use crate::commands::AppState;
use crate::core::connection::Connection;
use crate::core::credentials;
use crate::core::repository::ConnectionRepo;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::rds::client::{KeyDetail, KeyValue};
use rust_i18n::t;

/// 构造 redis://[user][:password]@host:port/db 连接串。
fn build_url(conn: &Connection, password: &str) -> String {
    let db = conn.database.trim().parse::<u8>().unwrap_or(0);
    let auth = if !conn.user.is_empty() {
        format!("{}:{}@", conn.user, password)
    } else if !password.is_empty() {
        format!(":{}@", password)
    } else {
        String::new()
    };
    format!("redis://{auth}{}:{}/{}", conn.host, conn.port, db)
}

pub async fn redis_connect(state: &AppState, id: String) -> AppResult<()> {
    let conns = ConnectionRepo::new(&state.config_dir).load()?;
    let conn = conns
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| AppError::new(ErrorKind::NotFound, t!("core.err.connection_not_found")))?;
    let password = credentials::load_password(&state.backend, &conn)?;
    state
        .redis_pool
        .connect(&id, &build_url(&conn, &password))
        .await
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
