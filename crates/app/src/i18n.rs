//! 界面语言:偏好(跟随系统 / 简体中文 / 繁體中文 / English)→ rust-i18n 的 locale。
//!
//! rust-i18n 的当前 locale 是进程全局的,app、datum-core 与 gpui-component 共用,
//! 所以组件库自带的文案(确定 / 取消等)也随之切换。

use crate::prefs::{LanguagePreference, Prefs};
use gpui_kit::App;

pub const ZH_CN: &str = "zh-CN";
pub const ZH_TW: &str = "zh-TW";
pub const EN: &str = "en";

/// 系统首选语言映射到支持的 locale:繁体(zh-Hant / zh-TW / zh-HK / zh-MO)→ zh-TW,
/// 其他中文 → zh-CN,其余一律英文。
pub fn system_locale() -> &'static str {
    sys_locale::get_locales()
        .next()
        .map_or(EN, |l| map_locale(&l))
}

fn map_locale(tag: &str) -> &'static str {
    let t = tag.to_ascii_lowercase().replace('_', "-");
    if !t.starts_with("zh") {
        return EN;
    }
    if t.contains("hant") || t.contains("-tw") || t.contains("-hk") || t.contains("-mo") {
        ZH_TW
    } else {
        ZH_CN
    }
}

pub fn resolve(pref: LanguagePreference) -> &'static str {
    match pref {
        LanguagePreference::System => system_locale(),
        LanguagePreference::ZhCn => ZH_CN,
        LanguagePreference::ZhTw => ZH_TW,
        LanguagePreference::En => EN,
    }
}

/// 按偏好设置当前语言。启动时调用一次;切换语言后再调用并重绘。
pub fn apply(cx: &App) {
    rust_i18n::set_locale(resolve(Prefs::global(cx).language));
}

/// 切换语言:写偏好 → 设 locale → 重设菜单 → 重绘所有窗口。
pub fn set_preference(pref: LanguagePreference, cx: &mut App) {
    Prefs::update(cx, |p| p.language = pref);
    apply(cx);
    crate::set_menus(cx);
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::map_locale;

    #[test]
    fn maps_system_locales() {
        assert_eq!(map_locale("zh-Hans-CN"), "zh-CN");
        assert_eq!(map_locale("zh-CN"), "zh-CN");
        assert_eq!(map_locale("zh-Hant-TW"), "zh-TW");
        assert_eq!(map_locale("zh_HK"), "zh-TW");
        assert_eq!(map_locale("en-US"), "en");
        assert_eq!(map_locale("ja-JP"), "en");
    }
}
