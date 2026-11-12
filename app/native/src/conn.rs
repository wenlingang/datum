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
    pub fn saved_password(id: String) -> Result<String, String> {
        core_cmd::saved_password(&state(), &id).map_err(err)
    }

    /// 切库:同实例已有该库的连接则复用,否则克隆一个。返回目标连接。
    pub fn switch_database(id: String, database: String) -> Result<JsonValue, String> {
        core_cmd::switch_database(&state(), id, database)
            .map_err(err)
            .and_then(json)
    }

    /// 用表单当前参数(而非已保存配置)测一次连通性。
    /// `conn.ssh.secret` 为 null 时(编辑时没改 SSH 密钥)用已保存的。
    pub async fn test_connection(conn: JsonValue, password: String) -> Result<(), String> {
        let conn: Connection = from(conn, "connection")?;
        let ssh = ssh_context(&conn);
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

    /// 把文本写到文件(导出 CSV / SQL / Markdown)。
    pub fn write_text_file(path: String, contents: String) -> Result<(), String> {
        std::fs::write(&path, contents).map_err(|e| e.to_string())
    }

    /// 配置目录(诊断用)。
    pub fn config_dir() -> String {
        state().config_dir.display().to_string()
    }

    /// 上次退出时的会话(打开的标签与内容);没有或读不了为 null。
    pub fn load_session() -> JsonValue {
        std::fs::read(state().config_dir.join(SESSION_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(JsonValue::Null)
    }

    /// 保存会话:先写临时文件再改名,写到一半崩溃也不会留下坏文件。
    pub fn save_session(session: JsonValue) -> Result<(), String> {
        let dir = state().config_dir.clone();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let tmp = dir.join(format!("{SESSION_FILE}.tmp"));
        let bytes = serde_json::to_vec(&session).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, dir.join(SESSION_FILE)).map_err(|e| e.to_string())
    }
}

/// 会话文件名(在配置目录下)。
const SESSION_FILE: &str = "session.json";

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
