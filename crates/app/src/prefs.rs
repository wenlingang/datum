//! 界面偏好(主题、字体、行数上限等),落盘到配置目录的 ui_prefs.json。
//! 替代 Tauri 版存在 localStorage 里的那些设置;读失败一律回退默认值。

use gpui_kit::{App, Global};
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ThemePreference {
    Light,
    #[default]
    Dark,
    System,
}

impl ThemePreference {
    pub const ALL: [ThemePreference; 3] = [Self::Light, Self::Dark, Self::System];

    pub fn label(self) -> String {
        match self {
            Self::Light => t!("prefs.theme_light"),
            Self::Dark => t!("prefs.theme_dark"),
            Self::System => t!("prefs.theme_system"),
        }
        .to_string()
    }
}

/// 界面语言偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum LanguagePreference {
    #[default]
    System,
    ZhCn,
    ZhTw,
    En,
}

impl LanguagePreference {
    pub const ALL: [LanguagePreference; 4] = [Self::System, Self::ZhCn, Self::ZhTw, Self::En];

    /// 语言名用各自语言书写(不随界面语言翻译),「跟随系统」随界面语言。
    pub fn label(self) -> String {
        match self {
            Self::System => rust_i18n::t!("prefs.language_system").to_string(),
            Self::ZhCn => "简体中文".into(),
            Self::ZhTw => "繁體中文".into(),
            Self::En => "English".into(),
        }
    }
}

/// 单次查询/取数返回的最大行数预设。
pub const MAX_ROWS_OPTIONS: [u32; 7] = [1, 100, 500, 1000, 5000, 10_000, 100_000];
pub const DEFAULT_MAX_ROWS: u32 = 1000;
/// 上限的上限:防误输超大值把界面拖死。
pub const MAX_ROWS_CEILING: u32 = 1_000_000;

/// 字体候选(字体族)。`.SystemUIFont` 是系统界面字体。展示名见 [`font_family_label`]。
pub const FONT_FAMILIES: [&str; 7] = [
    ".SystemUIFont",
    "PingFang SC",
    "Helvetica Neue",
    "Microsoft YaHei",
    "SF Mono",
    "Menlo",
    "JetBrains Mono",
];

/// 字体族的展示名(随界面语言);不在候选里的返回 None。
pub fn font_family_label(family: &str) -> Option<String> {
    let label = match family {
        ".SystemUIFont" => t!("prefs.font_system"),
        "PingFang SC" => t!("prefs.font_pingfang"),
        "Helvetica Neue" => "Helvetica Neue".into(),
        "Microsoft YaHei" => t!("prefs.font_yahei"),
        "SF Mono" | "Menlo" | "JetBrains Mono" => t!("prefs.font_mono", name = family),
        _ => return None,
    };
    Some(label.to_string())
}

/// 正文字号候选(px)。展示名见 [`font_size_label`]。
pub const FONT_SIZES: [u32; 5] = [12, 13, 14, 15, 16];

/// 字号的展示名(随界面语言);不在候选里的返回 None。
pub fn font_size_label(size: u32) -> Option<String> {
    let label = match size {
        12 => t!("prefs.size_small"),
        13 => t!("prefs.size_standard"),
        14 => t!("prefs.size_medium"),
        15 => t!("prefs.size_large"),
        16 => t!("prefs.size_xlarge"),
        _ => return None,
    };
    Some(label.to_string())
}

/// 字重候选。展示名见 [`font_weight_label`]。
pub const FONT_WEIGHTS: [u16; 4] = [400, 500, 600, 700];

/// 字重的展示名(随界面语言);不在候选里的返回 None。
pub fn font_weight_label(weight: u16) -> Option<String> {
    let label = match weight {
        400 => t!("prefs.weight_regular"),
        500 => t!("prefs.weight_medium"),
        600 => t!("prefs.weight_semibold"),
        700 => t!("prefs.weight_bold"),
        _ => return None,
    };
    Some(label.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    pub theme: ThemePreference,
    pub language: LanguagePreference,
    pub font_family: String,
    pub font_size: u32,
    pub font_weight: u16,
    pub max_rows: u32,
    /// AI 输入框的基准高度(px)
    pub ai_input_height: f32,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: ThemePreference::Dark,
            language: LanguagePreference::System,
            font_family: FONT_FAMILIES[0].to_string(),
            font_size: 13,
            font_weight: 400,
            max_rows: DEFAULT_MAX_ROWS,
            ai_input_height: 44.,
        }
    }
}

impl Global for Prefs {}

fn path() -> PathBuf {
    crate::runtime::state().config_dir.join("ui_prefs.json")
}

impl Prefs {
    pub fn load() -> Self {
        std::fs::read(path())
            .ok()
            .and_then(|b| serde_json::from_slice::<Prefs>(&b).ok())
            .map(|mut p| {
                if !(1..=MAX_ROWS_CEILING).contains(&p.max_rows) {
                    p.max_rows = DEFAULT_MAX_ROWS;
                }
                p
            })
            .unwrap_or_default()
    }

    pub fn global(cx: &App) -> &Prefs {
        cx.global::<Prefs>()
    }

    /// 修改偏好并立即落盘(失败静默:偏好丢了不值得打扰用户)。
    pub fn update(cx: &mut App, f: impl FnOnce(&mut Prefs)) {
        let prefs = cx.global_mut::<Prefs>();
        f(prefs);
        let snapshot = prefs.clone();
        if let Ok(json) = serde_json::to_vec_pretty(&snapshot) {
            let p = path();
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, json);
        }
    }
}

/// 规范化自定义行数上限:正整数且不超过天花板,否则 None。
pub fn normalize_max_rows(v: &str) -> Option<u32> {
    v.trim()
        .parse::<u32>()
        .ok()
        .filter(|n| (1..=MAX_ROWS_CEILING).contains(n))
}
