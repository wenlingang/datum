use crate::core::connection::Connection;
use crate::core::credentials::{self, KeyringBackend};
use crate::core::history::{HistoryEntry, HistoryRepo};
use crate::core::repository::ConnectionRepo;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::my::client::MyPool;
use crate::pg::client::PgPool;
use crate::rds::client::RedisPool;
use crate::sqlite::client::SqlitePool;
use rust_i18n::t;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct AppState {
    pub config_dir: PathBuf,
    pub backend: KeyringBackend,
    pub lock: Mutex<()>,         // 串行化配置文件写入
    pub history_lock: Mutex<()>, // 历史写盘独立锁:每次执行 SQL 都会写,不与连接保存竞争
    pub pg_pool: PgPool,
    pub redis_pool: RedisPool,
    pub my_pool: MyPool,
    pub sqlite_pool: SqlitePool,
}

impl AppState {
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            config_dir,
            backend: KeyringBackend,
            lock: Mutex::new(()),
            history_lock: Mutex::new(()),
            pg_pool: PgPool::default(),
            redis_pool: RedisPool::default(),
            my_pool: MyPool::default(),
            sqlite_pool: SqlitePool::default(),
        }
    }

    /// 配置目录沿用 Tauri 版的 app_config_dir(标识符 com.dbstudio.app),
    /// 升级后原有的连接、历史与 AI 配置无需迁移。
    pub fn default_config_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| std::env::temp_dir())
            .join("com.dbstudio.app")
    }

    fn repo(&self) -> ConnectionRepo {
        ConnectionRepo::new(&self.config_dir)
    }
    fn history_repo(&self) -> HistoryRepo {
        HistoryRepo::new(&self.config_dir)
    }
}

/// 返回给前端的连接(永远不含密码)。
fn sanitize(mut c: Connection) -> Connection {
    c.plaintext_password = None;
    c
}

/// 编辑时未提供新密码(password=None):沿用已存连接的明文密码。
/// 前端传回的连接已被 sanitize 抹掉密码,直接保存会丢失 local 的明文密码,
/// 故从已存连接里把明文密码带回来(staging/prod 密码在钥匙串,不受影响)。
fn carry_over_password(mut conn: Connection, existing: Option<&Connection>) -> Connection {
    if let Some(e) = existing {
        conn.plaintext_password = e.plaintext_password.clone();
    }
    conn
}

pub fn list_connections(state: &AppState) -> AppResult<Vec<Connection>> {
    let conns = state.repo().load()?;
    Ok(conns.into_iter().map(sanitize).collect())
}

pub fn save_connection(
    state: &AppState,
    conn: Connection,
    password: Option<String>,
) -> AppResult<Connection> {
    let _g = state.lock.lock().unwrap();
    let mut all = state.repo().load()?;
    let stored = match password {
        Some(pw) => credentials::save_password(&state.backend, conn, &pw)?,
        None => {
            let existing = all.iter().find(|c| c.id == conn.id);
            carry_over_password(conn, existing)
        }
    };
    match all.iter().position(|c| c.id == stored.id) {
        Some(i) => all[i] = stored.clone(),
        None => all.push(stored.clone()),
    }
    state.repo().save(&all)?;
    Ok(sanitize(stored))
}

pub fn delete_connection(state: &AppState, id: String) -> AppResult<()> {
    let _g = state.lock.lock().unwrap();
    let mut all = state.repo().load()?;
    let target =
        all.iter().find(|c| c.id == id).cloned().ok_or_else(|| {
            AppError::new(ErrorKind::NotFound, t!("core.err.connection_not_found"))
        })?;
    credentials::delete_password(&state.backend, &target)?;
    all.retain(|c| c.id != id);
    state.repo().save(&all)
}

/// 克隆连接的命名/ID:去掉旧「/库名」「@库名」后缀再拼新库名,连环切库不叠加后缀。
fn switched_name(name: &str, old_db: &str, new_db: &str) -> String {
    let base = name.strip_suffix(&format!("/{old_db}")).unwrap_or(name);
    format!("{base}/{new_db}")
}
fn switched_id(id: &str, database: &str) -> String {
    let base = id.split('@').next().unwrap_or(id);
    format!("{base}@{database}")
}

/// 侧栏切库:同实例(kind/host/port/user 相同)下目标库已有连接则直接复用,
/// 否则克隆当前连接(密码一并复制到钥匙串/明文字段)并持久化。返回目标连接(不含密码)。
pub fn switch_database(state: &AppState, id: String, database: String) -> AppResult<Connection> {
    let _g = state.lock.lock().unwrap();
    let mut all = state.repo().load()?;
    let src =
        all.iter().find(|c| c.id == id).cloned().ok_or_else(|| {
            AppError::new(ErrorKind::NotFound, t!("core.err.connection_not_found"))
        })?;
    if let Some(existing) = all.iter().find(|c| {
        c.kind == src.kind
            && c.host == src.host
            && c.port == src.port
            && c.user == src.user
            && c.database == database
    }) {
        return Ok(sanitize(existing.clone()));
    }
    let password = credentials::load_password(&state.backend, &src)?;
    let mut clone = src.clone();
    clone.id = switched_id(&src.id, &database);
    // 派生 id 撞上既有连接(如克隆后被改过 host)时退回随机后缀,保证 id 唯一
    if all.iter().any(|c| c.id == clone.id) {
        clone.id = format!(
            "{}-{}",
            clone.id,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        );
    }
    clone.name = switched_name(&src.name, &src.database, &database);
    clone.database = database;
    let stored = credentials::save_password(&state.backend, clone, &password)?;
    all.push(stored.clone());
    state.repo().save(&all)?;
    Ok(sanitize(stored))
}

/// 用表单当前填写的参数(而非已保存配置)做一次真实连通性测试,不缓存、不落盘。
pub async fn test_connection(
    kind: String,
    host: String,
    port: u16,
    user: String,
    password: String,
    database: String,
    file_path: Option<String>,
) -> AppResult<()> {
    match kind.as_str() {
        "pg" => crate::pg::client::test_connect(&host, port, &user, &password, &database).await,
        "mysql" => crate::my::client::test_connect(&host, port, &user, &password, &database).await,
        "redis" => crate::rds::client::test_connect(&host, port, &user, &password, &database).await,
        "sqlite" => crate::sqlite::client::test_connect(file_path.as_deref().unwrap_or("")),
        _ => Err(AppError::new(
            ErrorKind::Connection,
            t!("core.err.unsupported_kind"),
        )),
    }
}

pub fn list_history(state: &AppState) -> AppResult<Vec<HistoryEntry>> {
    Ok(state.history_repo().load())
}

/// 去重/截断在仓库层做,返回落盘后的完整列表,前端直接覆盖内存态。
pub fn append_history(state: &AppState, entry: HistoryEntry) -> AppResult<Vec<HistoryEntry>> {
    let _g = state.history_lock.lock().unwrap();
    state.history_repo().append(entry)
}

/// conn_id 为 Some 时只清该连接的历史(历史按数据库隔离),None 清全部。
/// 返回剩余的完整列表,前端直接覆盖内存态。
pub fn clear_history(state: &AppState, conn_id: Option<String>) -> AppResult<Vec<HistoryEntry>> {
    let _g = state.history_lock.lock().unwrap();
    let repo = state.history_repo();
    match conn_id {
        Some(id) => repo.clear_conn(&id),
        None => {
            repo.clear()?;
            Ok(Vec::new())
        }
    }
}

/// 导入 SQL 文件的大小上限(20 MB):挡住误选超大文件把整段文本塞进前端。
const IMPORT_SQL_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// 读取用户通过系统对话框选中的 SQL 文件(UTF-8 文本)。
pub fn read_sql_file(path: String) -> Result<String, String> {
    let meta = std::fs::metadata(&path)
        .map_err(|e| t!("core.err.read_file_failed", error = e).to_string())?;
    if meta.len() > IMPORT_SQL_MAX_BYTES {
        return Err(t!(
            "core.err.file_too_large",
            size = format!("{:.1}", meta.len() as f64 / 1_048_576.0),
            limit = IMPORT_SQL_MAX_BYTES / 1_048_576
        )
        .to_string());
    }
    std::fs::read_to_string(&path)
        .map_err(|e| t!("core.err.read_file_utf8_failed", error = e).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connection::{DbKind, Env};

    fn conn(id: &str, env: Env, pw: Option<&str>) -> Connection {
        Connection {
            id: id.into(),
            name: "n".into(),
            kind: DbKind::Pg,
            env,
            host: "h".into(),
            port: 5432,
            user: "u".into(),
            database: "d".into(),
            plaintext_password: pw.map(|s| s.into()),
            file_path: None,
        }
    }

    #[test]
    fn carry_over_keeps_local_plaintext_from_existing() {
        // 前端传回的(已 sanitize)连接没有密码
        let incoming = conn("c1", Env::Local, None);
        // 已存连接里有明文密码
        let existing = conn("c1", Env::Local, Some("secret"));
        let merged = carry_over_password(incoming, Some(&existing));
        assert_eq!(merged.plaintext_password.as_deref(), Some("secret"));
    }

    #[test]
    fn carry_over_noop_when_no_existing() {
        let incoming = conn("c1", Env::Local, None);
        let merged = carry_over_password(incoming, None);
        assert_eq!(merged.plaintext_password, None);
    }

    #[test]
    fn switched_name_replaces_previous_db_suffix() {
        assert_eq!(
            switched_name("analytics-prod", "analytics", "reporting"),
            "analytics-prod/reporting"
        );
        // 克隆的克隆不叠加后缀
        assert_eq!(
            switched_name("analytics-prod/reporting", "reporting", "analytics_shadow"),
            "analytics-prod/analytics_shadow"
        );
    }

    #[test]
    fn switched_id_derives_from_base_id() {
        assert_eq!(switched_id("c1", "reporting"), "c1@reporting");
        assert_eq!(switched_id("c1@reporting", "analytics"), "c1@analytics");
    }
}
