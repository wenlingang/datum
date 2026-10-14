//! 应用命令。键位、菜单与按钮派发同一个 Action,行为只实现一次。

use gpui_kit::{App, KeyBinding, actions};

actions!(
    datum,
    [
        Quit,
        OpenSettings,
        NewQuery,
        CloseTab,
        ToggleSidebar,
        ToggleDetail,
        ToggleAi,
        OpenErd,
        Refresh,
        RunQuery,
        FormatSql,
        ExplainQuery,
        ToggleHistory,
        SendSelectionToAi,
        ToggleFilter,
        CommitEdits,
        HideWindow,
    ]
);

/// 绑定键位。必须在 `set_menus` 之前调用,菜单才会显示快捷键。
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-t", NewQuery, Some("Workspace")),
        KeyBinding::new("cmd-w", CloseTab, Some("Workspace")),
        KeyBinding::new("cmd-b", ToggleSidebar, Some("Workspace")),
        KeyBinding::new("cmd-r", Refresh, Some("Workspace")),
        KeyBinding::new("cmd-y", ToggleHistory, Some("Workspace")),
        KeyBinding::new("cmd-e", ExplainQuery, Some("Workspace")),
        KeyBinding::new("cmd-f", ToggleFilter, Some("Workspace")),
        KeyBinding::new("cmd-s", CommitEdits, Some("Workspace")),
        KeyBinding::new("cmd-shift-f", FormatSql, Some("Workspace")),
        // 编辑器内:需要压过 Input 自带的 secondary-enter / cmd-shift-f / ctrl-e 等绑定,
        // 同一节点上后注册的绑定优先,故在编辑器上下文里再绑一次。
        KeyBinding::new("cmd-enter", RunQuery, Some("SqlEditor > Input")),
        KeyBinding::new("cmd-shift-f", FormatSql, Some("SqlEditor > Input")),
        KeyBinding::new("cmd-e", ExplainQuery, Some("SqlEditor > Input")),
        KeyBinding::new("cmd-shift-a", SendSelectionToAi, Some("SqlEditor > Input")),
    ]);
}
