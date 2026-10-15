//! 查询工作区的工具条(编辑器下缘):运行(含行数上限)/ 格式化 / Explain,右侧查询历史。

use super::WorkTab;
use super::history::HistoryPanel;
use crate::actions::{ExplainQuery, FormatSql, RunQuery, ToggleHistory};
use crate::prefs::{MAX_ROWS_OPTIONS, Prefs, normalize_max_rows};
use crate::theme::ActivePalette as _;
use crate::ui;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
    px,
};
use rust_i18n::t;

/// 工具条里两个浮层的状态。
pub struct ToolbarState {
    pub limit_open: bool,
    pub history_open: bool,
    pub limit_input: Entity<InputState>,
    pub history: Entity<HistoryPanel>,
    _sub: Subscription,
}

impl ToolbarState {
    pub fn new(window: &mut Window, cx: &mut Context<WorkTab>) -> Self {
        let limit_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("toolbar.limit_custom")));
        let sub = cx.subscribe(&limit_input, |tab, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let value = input.read(cx).value().to_string();
                if let Some(n) = normalize_max_rows(&value) {
                    Prefs::update(cx, |p| p.max_rows = n);
                    tab.toolbar.limit_open = false;
                    cx.notify();
                }
            }
        });
        let tab = cx.entity().downgrade();
        let history = cx.new(|cx| HistoryPanel::new(tab, cx));
        Self {
            limit_open: false,
            history_open: false,
            limit_input,
            history,
            _sub: sub,
        }
    }
}

fn shortcut(text: &'static str, cx: &gpui_kit::App) -> impl IntoElement {
    div()
        .text_xs()
        .font_family(cx.theme().mono_font_family.clone())
        .opacity(0.6)
        .child(text)
}

pub fn render(tab: &WorkTab, cx: &mut Context<WorkTab>) -> impl IntoElement {
    let relational = tab.db_kind.supports_sql();
    let max_rows = Prefs::global(cx).max_rows;
    let palette = *cx.palette();

    let run = Button::new("run")
        .primary()
        .xsmall()
        .icon(ui::icon("play"))
        .loading(tab.running)
        .label(if relational {
            t!("toolbar.run_with_limit", n = max_rows).to_string()
        } else {
            t!("toolbar.run").to_string()
        })
        .tooltip(t!("toolbar.run_tooltip"))
        .on_click(|_, window, cx| window.dispatch_action(Box::new(RunQuery), cx));

    let limit = Popover::new("run-limit")
        .anchor(Anchor::TopLeft)
        .open(tab.toolbar.limit_open)
        .on_open_change(cx.listener(|tab, open: &bool, _, cx| {
            tab.toolbar.limit_open = *open;
            cx.notify();
        }))
        .trigger(
            Button::new("run-limit-trigger")
                .primary()
                .xsmall()
                .icon(ui::icon("chevron-down"))
                .tooltip(t!("toolbar.limit_tooltip")),
        )
        .child(
            v_flex()
                .w(px(160.))
                .gap_0p5()
                .child(
                    div()
                        .px_2()
                        .pb_1()
                        .text_xs()
                        .font_weight(FontWeight::BOLD)
                        .text_color(palette.fg_faint)
                        .child(t!("toolbar.limit_title")),
                )
                .children(MAX_ROWS_OPTIONS.iter().map(|n| {
                    let n = *n;
                    ui::nav_row(
                        ("limit", n as usize),
                        t!("toolbar.n_rows", n = n).to_string(),
                        None,
                        n == max_rows,
                        cx,
                    )
                    .h(px(26.))
                    .text_xs()
                    .on_click(cx.listener(move |tab, _, _, cx| {
                        Prefs::update(cx, |p| p.max_rows = n);
                        tab.toolbar.limit_open = false;
                        cx.notify();
                    }))
                }))
                .child(
                    h_flex()
                        .mt_1()
                        .pt_1p5()
                        .gap_1p5()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .flex_1()
                                .child(Input::new(&tab.toolbar.limit_input).xsmall()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("toolbar.rows_unit")),
                        ),
                ),
        );

    let history = Popover::new("history")
        .anchor(Anchor::TopRight)
        .open(tab.toolbar.history_open)
        .on_open_change(cx.listener(|tab, open: &bool, _, cx| {
            tab.toolbar.history_open = *open;
            cx.notify();
        }))
        .trigger(
            Button::new("history-trigger")
                .ghost()
                .xsmall()
                .icon(ui::icon("rotate-ccw-clock"))
                .label(t!("toolbar.history"))
                .tooltip(t!("toolbar.history_tooltip")),
        )
        .p_0()
        .child(tab.toolbar.history.clone());

    h_flex()
        .h(px(38.))
        .flex_shrink_0()
        .px_2p5()
        .gap_1p5()
        .bg(palette.panel)
        .border_t_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .gap_px()
                .child(run)
                .when(relational, |this| this.child(limit)),
        )
        .when(relational, |this| {
            this.child(
                Button::new("format")
                    .ghost()
                    .xsmall()
                    .icon(ui::icon("wand-sparkles"))
                    .label(t!("toolbar.format"))
                    .child(shortcut("⌘⇧F", cx))
                    .tooltip(t!("toolbar.format_tooltip"))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(FormatSql), cx)),
            )
            .child(
                Button::new("explain")
                    .ghost()
                    .xsmall()
                    .icon(ui::icon("list-tree"))
                    .label("Explain")
                    .child(shortcut("⌘E", cx))
                    .tooltip(t!("toolbar.explain_tooltip"))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(ExplainQuery), cx)),
            )
        })
        .child(div().flex_1())
        .child(history)
        .on_action(cx.listener(|tab, _: &ToggleHistory, _, cx| {
            tab.toolbar.history_open = !tab.toolbar.history_open;
            cx.notify();
        }))
}
