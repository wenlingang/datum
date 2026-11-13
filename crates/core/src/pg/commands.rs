use crate::commands::AppState;
use crate::core::connection::Connection;
use crate::core::credentials;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::pg::browse::{self, TableDetail};
use crate::pg::client::QueryResult;
use crate::pg::edit::{self, CellEdit, RowDelete};
use crate::ssh::Target;
use rust_i18n::t;

/// 用显式 setter 构造连接 Config,避免手拼连接串在空密码/特殊字符时丢失 dbname。
fn build_config(conn: &Connection, target: &Target, password: &str) -> tokio_postgres::Config {
    let mut cfg = tokio_postgres::Config::new();
    crate::pg::client::apply_target(&mut cfg, target);
    cfg.user(&conn.user)
        .password(password)
        .dbname(&conn.database);
    crate::pg::client::apply_ssl_mode(&mut cfg, conn.ssl_mode);
    cfg
}

/// 用连接 id 找到配置 + 密码,建立 PG 连接。
pub async fn pg_connect(state: &AppState, id: String) -> AppResult<()> {
    let conns = crate::core::repository::ConnectionRepo::new(&state.config_dir).load()?;
    let conn = conns
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| AppError::new(ErrorKind::NotFound, t!("core.err.connection_not_found")))?;
    let password = credentials::load_password(&state.backend, &conn)?;
    let (target, tunnel) = crate::ssh::resolve(&conn, &state.ssh_context(&conn)?).await?;
    let config = build_config(&conn, &target, &password);
    state.pg_pool.connect_target(&id, &config, conn.ssl_mode, &target).await?;
    state.tunnels.set(&id, tunnel).await;
    Ok(())
}

pub async fn pg_list_objects(state: &AppState, id: String) -> AppResult<Vec<String>> {
    browse::list_tables(&state.pg_pool, &id).await
}

pub async fn pg_list_databases(state: &AppState, id: String) -> AppResult<Vec<String>> {
    browse::list_databases(&state.pg_pool, &id).await
}

pub async fn pg_query(
    state: &AppState,
    id: String,
    sql: String,
    max_rows: Option<u32>,
) -> AppResult<QueryResult> {
    state.pg_pool.query_capped(&id, &sql, max_rows).await
}

pub async fn pg_disconnect(state: &AppState, id: String) -> AppResult<()> {
    state.pg_pool.disconnect(&id).await;
    Ok(())
}

pub async fn pg_import_sql(
    state: &AppState,
    id: String,
    statements: Vec<String>,
) -> AppResult<usize> {
    state.pg_pool.import_sql(&id, &statements).await
}

pub async fn pg_table_detail(
    state: &AppState,
    id: String,
    table: String,
) -> AppResult<TableDetail> {
    browse::table_detail(&state.pg_pool, &id, &table).await
}

pub async fn pg_commit_edits(
    state: &AppState,
    id: String,
    edits: Vec<CellEdit>,
    deletes: Vec<RowDelete>,
) -> AppResult<u64> {
    edit::commit_edits(&state.pg_pool, &id, &edits, &deletes).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connection::{DbKind, Env};

    fn conn() -> Connection {
        Connection {
            id: "c1".into(),
            name: "n".into(),
            kind: DbKind::Pg,
            env: Env::Local,
            host: "127.0.0.1".into(),
            port: 5432,
            user: "postgres".into(),
            database: "my-app-dev".into(),
            plaintext_password: None,
            file_path: None,
            ssl_mode: Default::default(),
            uri: None,
            auth_source: None,
            ssh: None,
        }
    }

    #[test]
    fn build_config_keeps_dbname_with_empty_password() {
        // 回归:空密码不能让 dbname 丢失
        let c = conn();
        let cfg = build_config(&c, &Target::direct(&c.host, c.port), "");
        assert_eq!(cfg.get_dbname(), Some("my-app-dev"));
        assert_eq!(cfg.get_user(), Some("postgres"));
        assert_eq!(cfg.get_password(), Some(&b""[..]));
        assert!(cfg.get_hostaddrs().is_empty());
    }

    #[test]
    fn tunneled_config_dials_local_port_but_keeps_host_for_tls() {
        let target = Target::forwarded(50123, "db.internal");
        let cfg = build_config(&conn(), &target, "pw");
        assert_eq!(cfg.get_hosts(), &[tokio_postgres::config::Host::Tcp("db.internal".into())]);
        assert_eq!(cfg.get_hostaddrs(), &["127.0.0.1".parse::<std::net::IpAddr>().unwrap()]);
        assert_eq!(cfg.get_ports(), &[50123]);
    }
}
