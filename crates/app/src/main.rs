//! Datum —— 简约现代的桌面数据库管理工具(GPUI Kit 版)。

// 文案在 locales/*.yml(_version: 2,每个键下并列 en / zh-CN / zh-TW)。
rust_i18n::i18n!("locales", fallback = "en");

mod actions;
mod assets;
mod chat;
mod dialogs;
mod erd;
mod i18n;
mod prefs;
mod runtime;
#[cfg(feature = "snapshot")]
mod snapshot;
mod store;
mod tab;
mod theme;
mod ui;
mod workspace;

use gpui_kit::component::TitleBar;
use gpui_kit::{
    App, AppContext as _, Bounds, Menu, MenuItem, TitlebarOptions, WindowBounds, WindowOptions,
    point, px, size,
};
use rust_i18n::t;

/// 应用菜单。切换语言后需重新调用以更新菜单文字。
pub fn set_menus(cx: &mut App) {
    cx.set_menus([
        Menu {
            name: "Datum".into(),
            items: vec![
                MenuItem::action(t!("menu.settings"), actions::OpenSettings),
                MenuItem::separator(),
                MenuItem::action(t!("menu.hide"), actions::HideWindow),
                MenuItem::action(t!("menu.quit"), actions::Quit),
            ],
            disabled: false,
        },
        Menu {
            name: t!("menu.file").into(),
            items: vec![
                MenuItem::action(t!("menu.new_query"), actions::NewQuery),
                MenuItem::action(t!("menu.close_tab"), actions::CloseTab),
            ],
            disabled: false,
        },
        Menu {
            name: t!("menu.view").into(),
            items: vec![
                MenuItem::action(t!("menu.toggle_sidebar"), actions::ToggleSidebar),
                MenuItem::action(t!("menu.toggle_detail"), actions::ToggleDetail),
                MenuItem::action(t!("menu.toggle_ai"), actions::ToggleAi),
                MenuItem::action(t!("menu.erd"), actions::OpenErd),
                MenuItem::separator(),
                MenuItem::action(t!("menu.refresh"), actions::Refresh),
            ],
            disabled: false,
        },
    ]);
}

fn main() {
    runtime::init();
    #[cfg(feature = "snapshot")]
    if let Ok(script) = std::env::var("DATUM_SNAPSHOT") {
        snapshot::run(script);
        return;
    }
    let app = gpui_kit::application().with_assets(assets::AppAssets);
    // macOS 惯例:关闭窗口后应用留在 Dock,点 Dock 图标再唤回
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            open_main_window(cx);
        }
    });
    app.run(|cx: &mut App| {
        gpui_kit::init(cx);
        cx.set_global(prefs::Prefs::load());
        i18n::apply(cx);
        theme::init(cx);
        store::Stores::init(cx);

        actions::bind_keys(cx);
        cx.on_action(|_: &actions::Quit, cx| cx.quit());
        cx.on_action(|_: &actions::HideWindow, cx| cx.hide());
        set_menus(cx);

        open_main_window(cx);
        cx.activate(true);
    });
}

fn open_main_window(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(900.), px(600.))),
        titlebar: Some(TitlebarOptions {
            title: Some("Datum".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(14.), px(15.))),
        }),
        ..TitleBar::window_options()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| workspace::Workspace::new(window, cx))
    })
    .expect("无法打开主窗口");
}
