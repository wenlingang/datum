//! 对话框用到的原生能力:系统文件选择框(要的是路径,nsh 的 `Picker` 只给字节)、
//! Mongo 连接串的拆分 / 预览(core 的 `mongo::uri`)、AI 协议展示名。

use crate::runtime::{from, json};
use datum_core::ai::settings::Protocol;
use datum_core::core::connection::SslMode;
use datum_core::mongo::uri;
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

    /// 拆分 Mongo 连接串:`{ sanitized, user, password, hosts, database }`。
    /// `sanitized` 不含密码与库名;`password` 为 null 表示串里没写密码。不合法时抛出原因。
    pub fn split_mongo_uri(raw: String) -> Result<JsonValue, String> {
        let p = uri::split_uri(&raw)?;
        Ok(serde_json::json!({
            "sanitized": p.sanitized,
            "user": p.user,
            "password": p.password,
            "hosts": p.hosts,
            "database": p.database,
        }))
    }

    /// 把库名放回已去库名的连接串(编辑表单回填用)。
    pub fn mongo_with_database(sanitized: String, database: String) -> String {
        uri::with_database(&sanitized, &database)
    }

    /// 用 `••••` 遮住连接串里的密码(表单预览)。
    pub fn redact_mongo_uri(raw: String) -> String {
        uri::redact(raw.trim())
    }

    /// 表单模式下的 Mongo 连接串预览(密码已遮住)。`ssl_mode` 同 Connection.sslMode。
    pub fn mongo_form_preview(
        host: String,
        port: u32,
        user: String,
        has_password: bool,
        auth_source: String,
        ssl_mode: JsonValue,
    ) -> Result<String, String> {
        let ssl: SslMode = from(ssl_mode, "sslMode")?;
        let port = u16::try_from(port).unwrap_or(27017);
        let uris = uri::form_uris(
            &host,
            port,
            &user,
            if has_password { "PW" } else { "" },
            Some(auth_source.as_str()),
            ssl,
        );
        Ok(uris.first().map(|u| uri::redact(u)).unwrap_or_default())
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
