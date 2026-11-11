//! 工作区外壳用到的原生能力:系统保存对话框写文件、导出格式渲染(CSV / Markdown / INSERT)。
//! 对应 GPUI 版 workspace/ops.rs 的 `save_text` 与 `ExportFormat::render`。

use crate::runtime::from;
use datum_core::core::connection::DbKind;
use datum_core::grid::{Cell, file_timestamp, to_csv, to_markdown};
use datum_core::sql::Dialect;
use datum_core::sql::build::build_insert_row;
use nsh_core::JsonValue;
use nsh_core::file_dialog::SaveDialog;
use nsh_core::prelude::*;

/// 工作区外壳的原生能力。
#[module]
pub struct Shell;

#[module]
impl Shell {
    /// 弹出系统保存对话框(默认在下载目录,预填 `file_name`),把 `text` 写进用户选的路径。
    /// 返回写入的路径;用户取消时为 null。写入失败时报错(文案为系统错误)。
    pub async fn save_text(file_name: String, text: String) -> Result<Option<String>, String> {
        let mut dialog = SaveDialog::new().file_name(file_name);
        if let Some(dir) = dirs::download_dir() {
            dialog = dialog.directory(dir);
        }
        let Some(path) = pick_save_path(dialog).await? else {
            return Ok(None);
        };
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        Ok(Some(path.to_string_lossy().into_owned()))
    }

    /// 把结果行渲染成导出文本。`format`:`"csv"` / `"markdown"` / `"sql"`(每行一条 INSERT,
    /// 方言随 `kind`,非关系库按 PostgreSQL)。`rows` 为二维数组,NULL 为 null。
    pub fn render_export(
        format: String,
        table: String,
        columns: Vec<String>,
        rows: JsonValue,
        kind: String,
    ) -> Result<String, String> {
        let rows: Vec<Vec<Cell>> = from(rows, "rows")?;
        let refs: Vec<&Vec<Cell>> = rows.iter().collect();
        match format.as_str() {
            "csv" => Ok(to_csv(&columns, &refs)),
            "markdown" => Ok(to_markdown(&columns, &refs)),
            "sql" => {
                let k: Option<DbKind> = from(JsonValue::String(kind), "kind").ok();
                let dialect = k.as_ref().and_then(Dialect::from_kind).unwrap_or(Dialect::Pg);
                Ok(refs
                    .iter()
                    .map(|r| build_insert_row(&table, &columns, r, dialect))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            other => Err(format!("unknown export format: {other}")),
        }
    }

    /// 文件名友好的本地时间戳 `YYYYMMDD_HHmmss`。
    pub fn file_timestamp() -> String {
        file_timestamp()
    }
}
