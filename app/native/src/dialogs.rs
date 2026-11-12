//! 对话框用到的原生能力:系统文件选择框(要的是路径,nsh 的 `Picker` 只给字节)、
//! AI 协议展示名。

use crate::runtime::json;
use datum_core::ai::settings::Protocol;
use nsh_core::JsonValue;
use nsh_core::file_dialog::{FileFilter, OpenDialog};
use nsh_core::prelude::*;

/// 对话框的原生辅助:文件选择、连接串处理、AI 协议名。
#[module]
pub struct Dialogs;

#[module]
impl Dialogs {
    /// 弹出系统的打开文件框,返回选中文件的绝对路径;取消返回 null。
    /// `extensions` 非空时只列这些扩展名(`filter_name` 为其展示名)。
    pub async fn pick_file(
        title: String,
        filter_name: Option<String>,
        extensions: Vec<String>,
    ) -> Result<Option<String>, String> {
        let mut dialog = OpenDialog::files().title(title.clone()).button_label(title);
        if !extensions.is_empty() {
            dialog = dialog.filter(FileFilter::new(filter_name.unwrap_or_default(), extensions));
        }
        let picked = pick_paths(dialog).await?;
        Ok(picked
            .and_then(|paths| paths.into_iter().next())
            .map(|p| p.to_string_lossy().into_owned()))
    }

    /// 用表单当前参数(不保存)列出实例下的库,新建/编辑连接时下拉选库。
    pub async fn probe_databases(conn: JsonValue, password: String) -> Result<JsonValue, String> {
        let conn: datum_core::core::connection::Connection = crate::runtime::from(conn, "connection")?;
        let ssh = crate::conn::ssh_context(&conn);
        crate::runtime::spawn(async move { datum_core::service::probe_databases(&conn, &password, &ssh).await })
            .await?
            .map_err(crate::runtime::err)
            .and_then(json)
    }

    /// 协议的展示名(按当前 core 语言):`[{ id, label }]`,顺序即菜单顺序。
    pub fn ai_protocols() -> Result<JsonValue, String> {
        let list: Vec<JsonValue> = Protocol::ALL
            .iter()
            .map(|p| serde_json::json!({ "id": p, "label": p.label() }))
            .collect();
        json(list)
    }
}
