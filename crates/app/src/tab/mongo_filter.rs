//! Mongo 集合 tab 的顶部过滤栏(Filter / Sort / Projection)与底部分页条。

use super::WorkTab;
use super::mongo::PAGE_SIZES;
use crate::theme::ActivePalette as _;
use crate::ui;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Styled as _,
    Subscription, Window, div, px,
};
use rust_i18n::t;

pub struct FilterBar {
    pub filter: Entity<InputState>,
    pub sort: Entity<InputState>,
    pub projection: Entity<InputState>,
}

impl FilterBar {
    pub fn new(window: &mut Window, cx: &mut Context<WorkTab>) -> (Self, Vec<Subscription>) {
        // 占位写成「例如 …」,避免看起来像已经填了条件
        let mk = |example: &str, window: &mut Window, cx: &mut Context<WorkTab>| {
            let ph = t!("mongo.example", example = example).to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };
        let filter = mk("{ status: \"paid\" }", window, cx);
        let sort = mk("{ createdAt: -1 }", window, cx);
        let projection = mk("{ name: 1 }", window, cx);
        let subs = [&filter, &sort, &projection]
            .into_iter()
            .map(|e| {
                cx.subscribe_in(e, window, |tab, _, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. }) {
                        tab.mongo_apply_filter(window, cx);
                    }
                })
            })
            .collect();
        (
            Self {
                filter,
                sort,
                projection,
            },
            subs,
        )
    }
}

fn labeled(label: &'static str, input: &Entity<InputState>, cx: &gpui_kit::App) -> AnyElement {
    h_flex()
        .flex_1()
        .min_w_0()
        .gap_1p5()
        .child(
            div()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.palette().fg_faint)
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(Input::new(input).xsmall()))
        .into_any_element()
}

pub fn render_bar(tab: &WorkTab, cx: &mut Context<WorkTab>) -> AnyElement {
    let Some(m) = tab.mongo.as_ref() else {
        return div().into_any_element();
    };
    let Some(bar) = m.bar.as_ref() else {
        return div().into_any_element();
    };
    // 选中行且带 _id 才能编辑 / 删除
    let has_id = tab
        .selected_row
        .and_then(|ix| tab.mongo_doc_at(ix))
        .is_some_and(|(_, id)| id.is_some());
    v_flex()
        .px_2p5()
        .py_1p5()
        .gap_1p5()
        .bg(cx.palette().panel)
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .gap_2()
                .child(labeled("Filter", &bar.filter, cx))
                .child(div().w(px(220.)).child(labeled("Sort", &bar.sort, cx)))
                .child(
                    div()
                        .w(px(240.))
                        .child(labeled("Projection", &bar.projection, cx)),
                )
                .child(
                    Button::new("mongo-apply")
                        .primary()
                        .xsmall()
                        .label(t!("mongo.apply"))
                        .on_click(cx.listener(|t, _, w, cx| t.mongo_apply_filter(w, cx))),
                )
                .child(
                    Button::new("mongo-reset")
                        .ghost()
                        .xsmall()
                        .label(t!("mongo.reset"))
                        .on_click(cx.listener(|t, _, w, cx| t.mongo_reset_filter(w, cx))),
                ),
        )
        .when_some(m.bar_error.clone(), |this, e| {
            this.child(div().text_xs().text_color(cx.theme().danger).child(e))
        })
        .child(
            h_flex()
                .gap_1p5()
                .child(
                    Button::new("mongo-new-doc")
                        .ghost()
                        .xsmall()
                        .icon(ui::icon("plus"))
                        .label(t!("mongo.new_doc"))
                        .on_click(cx.listener(|t, _, w, cx| t.open_doc_editor(None, w, cx))),
                )
                .child(
                    Button::new("mongo-edit-doc")
                        .ghost()
                        .xsmall()
                        .icon(ui::icon("pencil"))
                        .label(t!("mongo.edit_doc"))
                        .disabled(!has_id)
                        .tooltip(t!("mongo.need_id_hint"))
                        .on_click(cx.listener(|t, _, w, cx| {
                            if let Some(ix) = t.selected_row {
                                t.open_doc_editor(Some(ix), w, cx);
                            }
                        })),
                )
                .child(
                    Button::new("mongo-delete-doc")
                        .ghost()
                        .xsmall()
                        .icon(ui::icon("trash"))
                        .label(t!("mongo.delete_doc"))
                        .disabled(!has_id)
                        .on_click(cx.listener(|t, _, w, cx| {
                            if let Some(ix) = t.selected_row {
                                t.confirm_delete_doc(ix, w, cx);
                            }
                        })),
                )
                .child(div().flex_1())
                .child(
                    Button::new("mongo-view-toggle")
                        .ghost()
                        .xsmall()
                        .icon(ui::icon(if m.json_view { "table" } else { "braces" }))
                        .label(if m.json_view {
                            t!("mongo.table_view")
                        } else {
                            t!("mongo.json_view")
                        })
                        .on_click(cx.listener(|t, _, _, cx| {
                            if let Some(m) = t.mongo.as_mut() {
                                m.json_view = !m.json_view;
                            }
                            cx.notify();
                        })),
                ),
        )
        .into_any_element()
}

pub fn render_pager(tab: &WorkTab, cx: &mut Context<WorkTab>) -> AnyElement {
    let Some(m) = tab.mongo.as_ref().filter(|m| m.coll.is_some()) else {
        return div().into_any_element();
    };
    let shown = tab.result.as_ref().map_or(0, |r| r.rows.len());
    let can_next = shown == m.page_size;
    let total = match m.total {
        Some((n, true)) => t!("mongo.total_exact", n = n).to_string(),
        Some((n, false)) => t!("mongo.total_estimated", n = n).to_string(),
        None => String::new(),
    };
    let filtered = m
        .bar
        .as_ref()
        .is_some_and(|b| !b.filter.read(cx).value().trim().is_empty());
    h_flex()
        .h(px(34.))
        .flex_shrink_0()
        .px_3()
        .gap_1p5()
        .text_xs()
        .bg(cx.palette().panel)
        .border_t_1()
        .border_color(cx.theme().border)
        .child(
            Button::new("mongo-prev")
                .ghost()
                .xsmall()
                .icon(ui::icon("chevron-left"))
                .disabled(m.page == 0 || tab.running)
                .on_click(cx.listener(|t, _, w, cx| {
                    let p = t.mongo.as_ref().map_or(0, |m| m.page);
                    t.mongo_goto_page(p.saturating_sub(1), w, cx);
                })),
        )
        .child(div().child(t!("mongo.page", n = m.page + 1).to_string()))
        .child(
            Button::new("mongo-next")
                .ghost()
                .xsmall()
                .icon(ui::icon("chevron-right"))
                .disabled(!can_next || tab.running)
                .on_click(cx.listener(|t, _, w, cx| {
                    let p = t.mongo.as_ref().map_or(0, |m| m.page);
                    t.mongo_goto_page(p + 1, w, cx);
                })),
        )
        .child(div().w(px(8.)))
        .children(PAGE_SIZES.iter().map(|n| {
            let n = *n;
            Button::new(("mongo-size", n))
                .xsmall()
                .when(n == m.page_size, |b| b.outline())
                .when(n != m.page_size, |b| b.ghost())
                .label(t!("mongo.per_page", n = n).to_string())
                .on_click(cx.listener(move |t, _, w, cx| {
                    if let Some(m) = t.mongo.as_mut() {
                        m.page_size = n;
                    }
                    t.mongo_goto_page(0, w, cx);
                }))
        }))
        .child(div().flex_1())
        .child(div().text_color(cx.theme().muted_foreground).child(total))
        .when(filtered && !matches!(m.total, Some((_, true))), |this| {
            this.child(
                Button::new("mongo-count")
                    .ghost()
                    .xsmall()
                    .label(t!("mongo.count"))
                    .on_click(cx.listener(|t, _, w, cx| t.mongo_count(w, cx))),
            )
        })
        .into_any_element()
}
