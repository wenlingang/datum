//! Datum 设计系统:三色分工 —— 靛(primary)= 交互、紫洋红(ai)= AI 专属、青(data)= 数据/焦点。
//!
//! 组件通用的语义色在 `assets/themes/datum.json` 里定义,注册为 gpui-kit 主题;
//! gpui-kit 没有对应角色的应用级颜色(环境色、单元格灰阶、AI 色等)放在 [`Palette`] 里。
//! 原始色值只出现在本文件与主题 JSON 中。

use crate::prefs::{Prefs, ThemePreference};
use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{App, Global, Hsla, Window, WindowAppearance, px, rgb, rgba};

/// gpui-kit 主题没有覆盖的应用级颜色角色。
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// 数据/焦点色(青):激活表、成功提示、选中指示
    pub data: Hsla,
    /// AI 专属色(紫洋红)
    pub ai: Hsla,
    pub ai_tint: Hsla,
    /// AI 色块上的文字/图标
    pub on_ai: Hsla,
    /// 次一级正文(介于 foreground 与 muted 之间)
    pub fg_soft: Hsla,
    /// 最弱的文本(占位、计数)
    pub fg_faint: Hsla,
    /// 较强的分隔线
    pub hairline: Hsla,
    /// 抬起一档的表面(胶囊、输入框底)
    pub raised: Hsla,
    /// 连接图标竖轨
    pub rail: Hsla,
    /// 编辑器/画布底色
    pub editor: Hsla,
    /// 面板底色(工具条、状态条)
    pub panel: Hsla,
    pub env_local: Hsla,
    pub env_staging: Hsla,
    pub env_prod: Hsla,
    /// 结果单元格灰阶:主值 → 软值 → 次级 → 哈希块 → 凹陷
    pub cell_strong: Hsla,
    pub cell: Hsla,
    pub cell_dim: Hsla,
    pub cell_hash: Hsla,
    pub cell_null: Hsla,
    /// 选中行/数据高亮底
    pub selection: Hsla,
    /// 暂存编辑/暂存删除的单元格底
    pub dirty_bg: Hsla,
    pub delete_bg: Hsla,
    pub negative: Hsla,
}

impl Global for Palette {}

impl Palette {
    fn dark() -> Self {
        Self {
            data: rgb(0x22d3ee).into(),
            ai: rgb(0xc084fc).into(),
            ai_tint: rgba(0xc084fc12).into(),
            on_ai: rgb(0x160a26).into(),
            fg_soft: rgb(0xb7bccb).into(),
            fg_faint: rgb(0x5a6173).into(),
            hairline: rgba(0x94a3cc2b).into(),
            raised: rgb(0x171a22).into(),
            rail: rgb(0x0d0f15).into(),
            editor: rgb(0x0a0b0f).into(),
            panel: rgb(0x101218).into(),
            env_local: rgb(0x34d399).into(),
            env_staging: rgb(0xfbbf24).into(),
            env_prod: rgb(0xf87171).into(),
            cell_strong: rgb(0xdde0ea).into(),
            cell: rgb(0xc3c8d6).into(),
            cell_dim: rgb(0x9098ac).into(),
            cell_hash: rgb(0x6b7288).into(),
            cell_null: rgb(0x545b6e).into(),
            selection: rgba(0x22d3ee21).into(),
            dirty_bg: rgba(0xf59e0b24).into(),
            delete_bg: rgba(0xf871711f).into(),
            negative: rgb(0xf87171).into(),
        }
    }

    fn light() -> Self {
        Self {
            data: rgb(0x0891b2).into(),
            ai: rgb(0x9333ea).into(),
            ai_tint: rgba(0x9333ea0d).into(),
            on_ai: rgb(0xffffff).into(),
            fg_soft: rgb(0x3a4154).into(),
            fg_faint: rgb(0x97a0b3).into(),
            hairline: rgba(0x0f172a26).into(),
            raised: rgb(0xeceef4).into(),
            rail: rgb(0xeceef4).into(),
            editor: rgb(0xfdfdfe).into(),
            panel: rgb(0xf7f8fb).into(),
            env_local: rgb(0x059669).into(),
            env_staging: rgb(0xb45309).into(),
            env_prod: rgb(0xdc2626).into(),
            cell_strong: rgb(0x1e2330).into(),
            cell: rgb(0x3a4154).into(),
            cell_dim: rgb(0x6a7288).into(),
            cell_hash: rgb(0x97a0b3).into(),
            cell_null: rgb(0xa9b1c2).into(),
            selection: rgba(0x06b6d424).into(),
            dirty_bg: rgba(0xb453091a).into(),
            delete_bg: rgba(0xd23f4b1a).into(),
            negative: rgb(0xdc2626).into(),
        }
    }
}

pub trait ActivePalette {
    fn palette(&self) -> &Palette;
}

impl ActivePalette for App {
    fn palette(&self) -> &Palette {
        self.global::<Palette>()
    }
}

/// 注册 Datum 主题并按偏好应用一次。必须在 `gpui_kit::init` 之后调用。
pub fn init(cx: &mut App) {
    if let Some(json) = crate::assets::text("themes/datum.json") {
        if let Err(e) = ThemeRegistry::global_mut(cx).load_themes_from_str(&json) {
            eprintln!("加载 Datum 主题失败: {e}");
        }
    }
    let registry = ThemeRegistry::global(cx).themes().clone();
    Theme::update(cx, |theme| {
        if let Some(dark) = registry.get("Datum Dark") {
            theme.dark_theme = dark.clone();
        }
        if let Some(light) = registry.get("Datum Light") {
            theme.light_theme = light.clone();
        }
    });
    cx.set_global(Palette::dark());
    apply(None, cx);
}

fn resolve_mode(
    pref: ThemePreference,
    appearance: Option<WindowAppearance>,
    cx: &App,
) -> ThemeMode {
    match pref {
        ThemePreference::Light => ThemeMode::Light,
        ThemePreference::Dark => ThemeMode::Dark,
        ThemePreference::System => {
            let appearance = appearance.unwrap_or_else(|| cx.window_appearance());
            match appearance {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
                _ => ThemeMode::Light,
            }
        }
    }
}

/// 按当前偏好应用主题模式与字体。`appearance` 为窗口外观变化时的新值。
pub fn apply(appearance: Option<WindowAppearance>, cx: &mut App) {
    let prefs = Prefs::global(cx).clone();
    let mode = resolve_mode(prefs.theme, appearance, cx);
    Theme::change(mode, None, cx);
    cx.set_global(if mode.is_dark() {
        Palette::dark()
    } else {
        Palette::light()
    });
    Theme::update(cx, |theme| {
        theme.font_family = prefs.font_family.clone().into();
        // 主题的 font_size 是整套 rem 尺度的基准;正文用 text_sm(0.875rem),
        // 让"正文字号"落在用户选择的像素值上。
        theme.font_size = px(prefs.font_size as f32 / 0.875);
        theme.mono_font_size = px(prefs.font_size as f32);
    });
}

/// 切换主题偏好(顶栏菜单)。
pub fn set_preference(pref: ThemePreference, window: &mut Window, cx: &mut App) {
    Prefs::update(cx, |p| p.theme = pref);
    apply(Some(window.appearance()), cx);
}
