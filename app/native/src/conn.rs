//! 连接配置与查询历史(本地文件,同步读写,文件很小)。

use crate::runtime::{err, from, json, spawn, state};
use datum_core::commands as core_cmd;
use datum_core::core::connection::Connection;
use datum_core::core::history::HistoryEntry;
use datum_core::service;
use nsh_core::JsonValue;
use nsh_core::prelude::*;

/// 连接配置、密码与查询历史。
#[module]
pub struct Store;

#[module]
impl Store {
    /// 全部已保存的连接(不含密码)。
    pub fn connections() -> Result<JsonValue, String> {
        core_cmd::list_connections(&state()).map_err(err).and_then(json)
    }

    /// 保存连接(新建或覆盖同 id);`password` 为 null 时保留原密码。返回保存后的连接。
    pub fn save_connection(conn: JsonValue, password: Option<String>) -> Result<JsonValue, String> {
        let conn: Connection = from(conn, "connection")?;
        core_cmd::save_connection(&state(), conn, password)
            .map_err(err)
            .and_then(json)
    }

    /// 删除连接(连同钥匙串里的密码)。
    pub fn delete_connection(id: String) -> Result<(), String> {
        core_cmd::delete_connection(&state(), id).map_err(err)
    }

    /// 已保存连接的密码(编辑表单「测试连接」用)。
    /// 等用户确认的 SSH 主机密钥:`[{ host, port, keyType, fingerprint }]`(首次连接的跳板机)。
    pub fn ssh_pending_host_keys() -> Result<JsonValue, String> {
        json(datum_core::ssh::pending_host_keys())
    }

    /// 用户确认后信任这把主机密钥(写进 Datum 的 known_hosts);指纹须与待确认的一致。
    pub fn ssh_trust_host_key(host: String, port: u32, fingerprint: String) -> Result<(), String> {
        let port = u16::try_from(port).map_err(|e| e.to_string())?;
        datum_core::ssh::trust_host_key(&host, port, &fingerprint, &state().known_hosts_path()).map_err(err)
    }

    /// 用户不信任:清掉待确认的主机密钥。
    pub fn ssh_forget_pending_host_keys() {
        datum_core::ssh::forget_pending_host_keys()
    }

    /// 切库:同实例已有该库的连接则复用,否则克隆一个。返回目标连接。
    pub fn switch_database(id: String, database: String) -> Result<JsonValue, String> {
        core_cmd::switch_database(&state(), id, database)
            .map_err(err)
            .and_then(json)
    }

    /// 用表单当前参数(而非已保存配置)测一次连通性。
    /// `conn.ssh.secret` 为 null 时(编辑时没改 SSH 密钥)用已保存的;
    /// `password` 为 null 时(编辑时没改密码)用已保存的 —— 已保存的密码不交给界面。
    pub async fn test_connection(conn: JsonValue, password: Option<String>) -> Result<(), String> {
        let conn: Connection = from(conn, "connection")?;
        let ssh = ssh_context(&conn);
        let password = password_or_saved(&conn, password);
        spawn(async move { service::test_connection(&conn, &password, &ssh).await })
            .await?
            .map_err(err)
    }

    /// 查询历史(最新在前)。
    pub fn history() -> Result<JsonValue, String> {
        json(core_cmd::list_history(&state()).unwrap_or_default())
    }

    /// 记录一条历史(id 由这里生成);返回落盘后的完整列表。失败返回 null。
    pub fn record_history(entry: JsonValue) -> Result<JsonValue, String> {
        let mut entry: HistoryEntry = from(entry, "history entry")?;
        entry.id = uuid::Uuid::new_v4().to_string();
        match core_cmd::append_history(&state(), entry) {
            Ok(list) => json(list),
            Err(_) => Ok(JsonValue::Null),
        }
    }

    /// 清空历史;`conn_id` 给定时只清该连接的。返回剩余列表。
    pub fn clear_history(conn_id: Option<String>) -> Result<JsonValue, String> {
        core_cmd::clear_history(&state(), conn_id)
            .map_err(err)
            .and_then(json)
    }

    /// 读取用户选中的 SQL 文件(UTF-8,有大小上限)。
    pub fn read_sql_file(path: String) -> Result<String, String> {
        core_cmd::read_sql_file(path)
    }

    /// 把文本写到配置目录下的文件(测试准备导入用的数据文件)。只允许写配置目录,
    /// 不给界面一个能写任意路径的入口;真正的导出走保存对话框。
    pub fn write_text_file(path: String, contents: String) -> Result<(), String> {
        let path = std::path::PathBuf::from(path);
        let dir = state().config_dir.canonicalize().map_err(|e| e.to_string())?;
        let parent = path.parent().ok_or("invalid path")?.canonicalize().map_err(|e| e.to_string())?;
        if !parent.starts_with(&dir) || path.file_name().is_none() {
            return Err("only files in the config directory can be written".into());
        }
        std::fs::write(parent.join(path.file_name().unwrap()), contents).map_err(|e| e.to_string())
    }

    /// 配置目录(诊断用)。
    pub fn config_dir() -> String {
        state().config_dir.display().to_string()
    }

    /// 上次退出时的会话(打开的标签与内容);没有或读不了为 null。
    pub fn load_session() -> JsonValue {
        load_json(SESSION_FILE)
    }

    /// 保存会话:先写临时文件再改名,写到一半崩溃也不会留下坏文件。
    pub fn save_session(session: JsonValue) -> Result<(), String> {
        save_json(SESSION_FILE, &session)
    }

    /// 保存过的 AI 对话;没有或读不了为 null。
    pub fn load_ai_conversations() -> JsonValue {
        load_json(AI_CONVOS_FILE)
    }

    /// 保存 AI 对话(原子写)。
    pub fn save_ai_conversations(data: JsonValue) -> Result<(), String> {
        save_json(AI_CONVOS_FILE, &data)
    }

    /// 收藏的 SQL 片段;没有或读不了为 null。
    pub fn load_snippets() -> JsonValue {
        load_json(SNIPPETS_FILE)
    }

    /// 保存 SQL 片段(原子写)。
    pub fn save_snippets(data: JsonValue) -> Result<(), String> {
        save_json(SNIPPETS_FILE, &data)
    }

    /// Redis 值的查看格式偏好(按连接 + key 前缀记住);没有或读不了为 null。
    pub fn load_redis_formats() -> JsonValue {
        load_json(REDIS_FORMATS_FILE)
    }

    /// 保存 Redis 值的查看格式偏好(原子写)。
    pub fn save_redis_formats(data: JsonValue) -> Result<(), String> {
        save_json(REDIS_FORMATS_FILE, &data)
    }
}

/// 会话文件名(在配置目录下)。
const SESSION_FILE: &str = "session.json";
/// AI 对话文件名(在配置目录下)。
const AI_CONVOS_FILE: &str = "ai_conversations.json";
/// SQL 片段文件名(在配置目录下)。
const SNIPPETS_FILE: &str = "snippets.json";
/// Redis 查看格式偏好文件名(在配置目录下)。
const REDIS_FORMATS_FILE: &str = "redis_formats.json";

fn load_json(name: &str) -> JsonValue {
    std::fs::read(state().config_dir.join(name))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(JsonValue::Null)
}

/// 里面有 SQL 与对话内容:仅本用户可读写;先写临时文件再改名,写到一半崩溃也不会留下坏文件。
fn save_json(name: &str, value: &JsonValue) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    datum_core::core::fsutil::write_private(&state().config_dir.join(name), &bytes).map_err(|e| e.to_string())
}

/// 表单里没填密码(null)时用已保存的;读不到按空密码。
pub fn password_or_saved(conn: &Connection, password: Option<String>) -> String {
    password.unwrap_or_else(|| core_cmd::saved_password(&state(), &conn.id).unwrap_or_default())
}

/// 表单参数建隧道的上下文:SSH 密钥取表单里填的,没填(null)则用已保存的。
pub fn ssh_context(conn: &Connection) -> service::SshContext {
    let st = state();
    let secret = match conn.ssh.as_ref().and_then(|s| s.secret.clone()) {
        Some(s) => s,
        None => core_cmd::saved_ssh_secret(&st, &conn.id).unwrap_or_default(),
    };
    service::SshContext {
        secret,
        known_hosts: st.known_hosts_path(),
    }
}
