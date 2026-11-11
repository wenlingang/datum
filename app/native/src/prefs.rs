//! 界面偏好(主题、语言、字体、行数上限等),落盘到配置目录的 ui_prefs.json。
//! 形状与 GPUI 版一致,升级后沿用原文件;读失败一律回退默认值。

use crate::runtime::state;
use nsh_core::JsonValue;
use nsh_core::prelude::*;
use serde::{Deserialize, Serialize};

/// 单次查询/取数返回的最大行数的默认值与上限。
const DEFAULT_MAX_ROWS: u32 = 1000;
const MAX_ROWS_CEILING: u32 = 1_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PrefsData {
    /// "light" | "dark" | "system"
    theme: String,
    /// "system" | "zhCn" | "zhTw" | "en"
    language: String,
    font_family: String,
    font_size: u32,
    font_weight: u16,
    max_rows: u32,
    ai_input_height: f32,
}

impl Default for PrefsData {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            language: "system".into(),
            font_family: ".SystemUIFont".into(),
            font_size: 13,
            font_weight: 400,
            max_rows: DEFAULT_MAX_ROWS,
            ai_input_height: 44.,
        }
    }
}

fn path() -> std::path::PathBuf {
    state().config_dir.join("ui_prefs.json")
}

/// 界面偏好的读写。
#[module]
pub struct Prefs;

#[module]
impl Prefs {
    /// 读取偏好(缺省项用默认值)。
    pub fn load() -> JsonValue {
        let mut p = std::fs::read(path())
            .ok()
            .and_then(|b| serde_json::from_slice::<PrefsData>(&b).ok())
            .unwrap_or_default();
        if !(1..=MAX_ROWS_CEILING).contains(&p.max_rows) {
            p.max_rows = DEFAULT_MAX_ROWS;
        }
        serde_json::to_value(p).unwrap_or(JsonValue::Null)
    }

    /// 保存偏好(失败静默:偏好丢了不值得打扰用户)。
    pub fn save(prefs: JsonValue) {
        let Ok(p) = serde_json::from_value::<PrefsData>(prefs) else {
            return;
        };
        if let Ok(json) = serde_json::to_vec_pretty(&p) {
            let file = path();
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(file, json);
        }
    }

    /// 设置 core 报错文案的语言(`en` / `zh-CN` / `zh-TW`)。
    pub fn set_core_locale(locale: String) {
        rust_i18n::set_locale(&locale);
    }

    /// 应用版本号。
    pub fn app_version() -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }
}
