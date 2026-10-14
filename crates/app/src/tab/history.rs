//! 查询历史浮层:按数据库隔离,只展示当前 tab 所在连接的条目。
//! 单击展开完整 SQL,双击再次执行(同连接)或填入编辑器(跨连接)。

use super::WorkTab;
use crate::store::{Stores, Toast};
use crate::theme::ActivePalette as _;
use crate::ui;
use datum_core::core::history::HistoryEntry;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Subscription,
    WeakEntity, Window, div, px,
};
use rust_i18n::t;

pub struct HistoryPanel {
    tab: WeakEntity<WorkTab>,
    expanded: Option<(String, Entity<EditorState>)>,
    _sub: Subscription,
}

/// 多行 SQL 压缩为单行用于列表展示。
fn compress(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 相对时间:1 分钟内"刚刚",1 小时内"x 分钟前",当天 HH:mm,更早带日期。
fn fmt_time(ts: i64) -> String {
    let now = chrono::Local::now();
    let diff = now.timestamp_millis() - ts;
    if diff < 60_000 {
        return t!("history.just_now").to_string();
    }
    if diff < 3_600_000 {
        return t!("history.minutes_ago", n = diff / 60_000).to_string();
    }
    let Some(t) =
        chrono::DateTime::from_timestamp_millis(ts).map(|t| t.with_timezone(&chrono::Local))
    else {
        return String::new();
    };
    if t.date_naive() == now.date_naive() {
        t.format("%H:%M").to_string()
    } else {
        t.format("%-m/%-d %H:%M").to_string()
    }
}

impl HistoryPanel {
    pub fn new(tab: WeakEntity<WorkTab>, cx: &mut Context<Self>) -> Self {
        let history = Stores::global(cx).history.clone();
        let sub = cx.observe(&history, |_, _, cx| cx.notify());
        Self {
            tab,
            expanded: None,
            _sub: sub,
        }
    }

    fn toggle_expand(&mut self, entry: &HistoryEntry, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .expanded
            .as_ref()
            .is_some_and(|(id, _)| *id == entry.id)
        {
            self.expanded = None;
        } else {
            let sql = entry.sql.clone();
            let lang = if entry.kind == "redis" { "text" } else { "sql" };
            let editor = cx.new(|cx| {
                EditorState::new(window, cx)
                    .language(lang)
                    .line_number(false)
                    .soft_wrap(true)
                    .default_value(sql)
            });
            self.expanded = Some((entry.id.clone(), editor));
        }
        cx.notify();
    }

    /// 双击:同连接立即执行;跨连接只填入不执行 —— 历史里一条来自 staging 的 UPDATE
    /// 被双击到 prod 连接的 tab 上静默执行是不可接受的。
    fn pick(&mut self, entry: &HistoryEntry, window: &mut Window, cx: &mut Context<Self>) {
        let entry = entry.clone();
        let _ = self.tab.update(cx, |tab, cx| {
            tab.toolbar.history_open = false;
            tab.set_sql(&entry.sql, window, cx);
            if entry.conn_id == tab.conn_id {
                tab.run(Some(entry.sql.clone()), true, false, window, cx);
            } else {
                cx.emit(super::TabEvent::Toast(Toast::Info(
                    t!("history.from_other_conn", name = entry.conn_name)
                        .to_string()
                        .into(),
                )));
            }
            cx.notify();
        });
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(tab) = self.tab.upgrade() else {
            return div();
        };
        let conn_id = tab.read(cx).conn_id.clone();
        let conn_label = Stores::global(cx)
            .conns
            .read(cx)
            .get(&conn_id)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let entries: Vec<HistoryEntry> = Stores::global(cx)
            .history
            .read(cx)
            .entries
            .iter()
            .filter(|e| e.conn_id == conn_id)
            .cloned()
            .collect();
        let palette = *cx.palette();
        let mono = cx.theme().mono_font_family.clone();

        let header = h_flex()
            .justify_between()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_1p5()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(ui::icon("rotate-ccw-clock").xsmall())
                    .child(t!("history.title"))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .font_family(mono.clone())
                            .text_color(palette.fg_faint)
                            .child(format!("· {conn_label}")),
                    ),
            )
            .when(!entries.is_empty(), |this| {
                let conn_id = conn_id.clone();
                this.child(
                    Button::new("history-clear")
                        .ghost()
                        .xsmall()
                        .label(t!("history.clear"))
                        .tooltip(t!("history.clear_tooltip"))
                        .on_click(cx.listener(move |_, _, window, cx| {
                            let history = Stores::global(cx).history.clone();
                            history.update(cx, |h, cx| h.clear(Some(conn_id.clone()), cx));
                            window.push_notification(
                                gpui_kit::component::notification::Notification::success(
                                    t!("history.cleared").to_string(),
                                ),
                                cx,
                            );
                        })),
                )
            });

        let list = if entries.is_empty() {
            div()
                .px_4()
                .py_6()
                .text_xs()
                .text_center()
                .text_color(palette.fg_faint)
                .child(t!("history.empty"))
                .into_any_element()
        } else {
            v_flex()
                .id("history-list")
                .max_h(px(420.))
                .overflow_y_scroll()
                .children(entries.into_iter().map(|e| {
                    let expanded = self
                        .expanded
                        .as_ref()
                        .filter(|(id, _)| *id == e.id)
                        .map(|(_, ed)| ed.clone());
                    let meta = format!(
                        "· {} · {}ms{}",
                        fmt_time(e.executed_at),
                        e.elapsed_ms,
                        match (e.row_count, e.affected) {
                            (Some(n), _) => format!(" · {}", t!("history.n_rows", n = n)),
                            (None, Some(n)) => format!(" · {}", t!("history.affected", n = n)),
                            _ => String::new(),
                        }
                    );
                    let entry = e.clone();
                    v_flex()
                        .id(gpui_kit::SharedString::from(format!("hist-{}", e.id)))
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .cursor_pointer()
                        .hover(|s| s.bg(cx.theme().list_hover))
                        .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                            if ev.click_count() >= 2 {
                                this.pick(&entry, window, cx);
                            } else {
                                this.toggle_expand(&entry, window, cx);
                            }
                        }))
                        .child(
                            div()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(cx.theme().foreground)
                                .child(compress(&e.sql)),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .mt_0p5()
                                .text_xs()
                                .text_color(palette.fg_faint)
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(e.conn_name.clone()),
                                )
                                .child(meta),
                        )
                        .when_some(expanded, |this, editor| {
                            this.child(
                                div()
                                    .mt_1p5()
                                    .h(px(140.))
                                    .rounded(cx.theme().radius)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .overflow_hidden()
                                    .child(
                                        Editor::new(&editor)
                                            .readonly(true)
                                            .bordered(false)
                                            .h(gpui_kit::relative(1.)),
                                    ),
                            )
                        })
                }))
                .into_any_element()
        };

        v_flex().w(px(480.)).child(header).child(list)
    }
}
