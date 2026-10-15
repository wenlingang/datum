//! 读取已保存的连接与密码后转发给 [`MongoPool`](super::client::MongoPool)。

use super::uri;
use crate::commands::AppState;
use crate::core::connection::Connection;
use crate::core::credentials;
use crate::core::repository::ConnectionRepo;
use crate::error::{AppError, AppResult, ErrorKind};
use rust_i18n::t;

fn load(state: &AppState, id: &str) -> AppResult<Connection> {
    ConnectionRepo::new(&state.config_dir)
        .load()?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| AppError::new(ErrorKind::NotFound, t!("core.err.connection_not_found")))
}

pub async fn mongo_connect(state: &AppState, id: &str) -> AppResult<()> {
    let conn = load(state, id)?;
    let password = credentials::load_password(&state.backend, &conn)?;
    state
        .mongo_pool
        .connect(
            id,
            &uri::connect_uris(&conn, &password),
            conn.database.clone(),
        )
        .await
}
