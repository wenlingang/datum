//! 写操作记录(写日志):追加、列表、详情、撤销前核对、导出、清空与设置。
//! 读写文件放到线程池上做(追加要 fsync,整理时会重写整份文件),不占界面线程。

use crate::runtime::{err, from, json, spawn, state};
use datum_core::commands as core_cmd;
use datum_core::core::connection::DbKind;
use datum_core::core::journal::{JournalEntry, JournalFilter, JournalSettings};
use datum_core::service;
use nsh_core::JsonValue;
use nsh_core::prelude::*;

/// 写操作记录。
#[module]
pub struct Journal;

#[module]
impl Journal {
    /// 新记录的 id(界面先拿到 id,查询历史与撤销都能指向这条记录)。
    pub fn new_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// 追加一条记录;返回记录 id,写日志关掉时为 null。
    pub async fn record(entry: JsonValue) -> Result<Option<String>, String> {
        let entry: JournalEntry = from(entry, "journal entry")?;
        spawn(async move { core_cmd::journal_append(&state(), entry) }).await?.map_err(err)
    }

    /// 列表(最新在前):`filter` 为 `{ source, status, text, from, to, limit }`,都可省。
    pub async fn list(conn_id: String, filter: JsonValue) -> Result<JsonValue, String> {
        let filter: JournalFilter = if filter.is_null() { JournalFilter::default() } else { from(filter, "filter")? };
        spawn(async move { core_cmd::journal_list(&state(), &conn_id, &filter) }).await.and_then(json)
    }

    /// 单条完整记录 `{ entry, undoneBy, previousSession, laterWrites }`;不存在为 null。
    pub async fn get(conn_id: String, id: String) -> Result<JsonValue, String> {
        spawn(async move { core_cmd::journal_get(&state(), &conn_id, &id) }).await.and_then(json)
    }

    /// 还能撤销的记录:`[[id, rows]]`。
    pub async fn undoable(conn_id: String) -> Result<JsonValue, String> {
        spawn(async move { core_cmd::journal_undoable(&state(), &conn_id) }).await.and_then(json)
    }

    /// 撤销前核对:`{ previousSession, laterWrites, undoneBy, checked, missing, existing, error }`。
    pub async fn undo_check(kind: String, conn_id: String, id: String) -> Result<JsonValue, String> {
        let k: DbKind = from(JsonValue::String(kind), "kind")?;
        spawn(async move { service::journal_undo_check(&state(), &k, &conn_id, &id).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 按筛选导出:`format` 为 `"md"`(变更记录)或 `"csv"`。
    pub async fn export(conn_id: String, filter: JsonValue, format: String) -> Result<String, String> {
        let filter: JournalFilter = if filter.is_null() { JournalFilter::default() } else { from(filter, "filter")? };
        spawn(async move { core_cmd::journal_export(&state(), &conn_id, &filter, &format) }).await
    }

    /// 清空:`conn_id` 给定时只清该连接的,否则全部。
    pub async fn clear(conn_id: Option<String>) -> Result<(), String> {
        spawn(async move { core_cmd::journal_clear(&state(), conn_id.as_deref()) }).await?.map_err(err)
    }

    /// 设置 `{ enabled, storeRows, retentionDays }`。
    pub fn settings() -> Result<JsonValue, String> {
        json(core_cmd::journal_settings(&state()))
    }

    /// 保存设置,返回规整后的设置。
    pub fn save_settings(settings: JsonValue) -> Result<JsonValue, String> {
        let s: JournalSettings = from(settings, "journal settings")?;
        core_cmd::journal_save_settings(&state(), &s).map_err(err).and_then(json)
    }
}
