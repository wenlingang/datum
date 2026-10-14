//! 结果区筛选条:条件之间用统一的连接词(且/或),编辑只改草稿,
//! 点「应用」或在值输入框回车才作用到结果。

use super::{TabEvent, WorkTab};
use crate::theme::ActivePalette as _;
use crate::ui;
use datum_core::grid::{Conj, FilterOp, FilterRow, filters_signature};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
    px,
};
use rust_i18n::t;

pub struct DraftFilter {
    pub id: u64,
    pub col: String,
    pub op: FilterOp,
    pub value: Entity<InputState>,
    _sub: Subscription,
}

#[derive(Default)]
pub struct FilterState {
    pub open: bool,
    pub drafts: Vec<DraftFilter>,
    pub conj: Conj,
    /// 已应用的条件(真正作用于结果行)
    pub applied: Vec<FilterRow>,
    pub applied_conj: Conj,
    seq: u64,
}

impl FilterState {
    pub fn add(&mut self, col: String, window: &mut Window, cx: &mut Context<WorkTab>) {
        self.seq += 1;
        let value =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("filter.value_placeholder")));
        let sub = cx.subscribe(&value, |tab, _, event: &InputEvent, cx| match event {
            InputEvent::PressEnter { .. } => tab.filter_apply(cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        });
        self.drafts.push(DraftFilter {
            id: self.seq,
            col,
            op: FilterOp::Contains,
            value,
            _sub: sub,
        });
    }

    pub fn draft_rows(&self, cx: &App) -> Vec<FilterRow> {
        self.drafts
            .iter()
            .map(|d| FilterRow {
                id: d.id,
                col: d.col.clone(),
                op: d.op,
                value: d.value.read(cx).value().to_string(),
            })
            .collect()
    }

    /// 草稿与已应用是否不一致(决定「应用」按钮态)。
    pub fn is_dirty(&self, cx: &App) -> bool {
        filters_signature(&self.draft_rows(cx), self.conj)
            != filters_signature(&self.applied, self.applied_conj)
    }

    pub fn apply(&mut self, cx: &App) {
        self.applied = self.draft_rows(cx);
        self.applied_conj = self.conj;
    }

    /// 清除:草稿与已应用一并清空,并收起筛选条。
    pub fn reset_all(&mut self) {
        self.drafts.clear();
        self.applied.clear();
        self.conj = Conj::And;
        self.applied_conj = Conj::And;
        self.open = false;
    }
}

/// 筛选操作符的界面文案(core 的 `FilterOp::label` 是固定中文,这里按当前语言翻译)。
fn op_label(op: FilterOp) -> String {
    match op {
        FilterOp::Contains => t!("filter.op.contains"),
        FilterOp::Eq => t!("filter.op.eq"),
        FilterOp::Ne => t!("filter.op.ne"),
        FilterOp::Starts => t!("filter.op.starts"),
        FilterOp::Ends => t!("filter.op.ends"),
        FilterOp::Gt => t!("filter.op.gt"),
        FilterOp::Lt => t!("filter.op.lt"),
        FilterOp::Gte => t!("filter.op.gte"),
        FilterOp::Lte => t!("filter.op.lte"),
        FilterOp::Empty => t!("filter.op.empty"),
        FilterOp::NotEmpty => t!("filter.op.not_empty"),
    }
    .to_string()
}

fn conj_label(conj: Conj) -> String {
    match conj {
        Conj::And => t!("filter.and"),
        Conj::Or => t!("filter.or"),
    }
    .to_string()
}

pub fn render(tab: &WorkTab, _: &mut Window, cx: &mut Context<WorkTab>) -> impl IntoElement {
    let columns = tab
        .result
        .as_ref()
        .map(|r| r.columns.clone())
        .unwrap_or_default();
    let total = tab.result.as_ref().map_or(0, |r| r.rows.len());
    let preview = tab.filter_preview_count(cx);
    let dirty = tab.filter.is_dirty(cx);
    let can_clear = !tab.filter.drafts.is_empty() || !tab.filter.applied.is_empty();
    let palette = *cx.palette();
    let conj = tab.filter.conj;
    let mono = cx.theme().mono_font_family.clone();

    let rows = tab.filter.drafts.iter().enumerate().map(|(i, d)| {
        let id = d.id;
        let conj_cell = if i == 0 {
            div()
                .w(px(36.))
                .flex_shrink_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(t!("filter.where"))
                .into_any_element()
        } else {
            div()
                .id(("conj", id))
                .w(px(36.))
                .h(px(28.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(cx.theme().radius)
                .bg(palette.selection)
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(palette.data)
                .cursor_pointer()
                .child(conj_label(conj))
                .on_click(cx.listener(|tab, _, _, cx| {
                    tab.filter.conj = tab.filter.conj.toggled();
                    cx.notify();
                }))
                .into_any_element()
        };
        let col_menu = {
            let columns = columns.clone();
            let this = cx.entity().downgrade();
            move |menu: gpui_kit::component::menu::PopupMenu,
                  _: &mut Window,
                  _: &mut Context<gpui_kit::component::menu::PopupMenu>| {
                columns
                    .iter()
                    .fold(menu.scrollable(true).max_h(px(320.)), |menu, c| {
                        let this = this.clone();
                        let c2 = c.clone();
                        menu.item(PopupMenuItem::new(c.clone()).on_click(move |_, _, cx| {
                            let c = c2.clone();
                            let _ = this.update(cx, |tab, cx| {
                                if let Some(d) = tab.filter.drafts.iter_mut().find(|d| d.id == id) {
                                    d.col = c;
                                }
                                cx.notify();
                            });
                        }))
                    })
            }
        };
        let op_menu = {
            let this = cx.entity().downgrade();
            move |menu: gpui_kit::component::menu::PopupMenu,
                  _: &mut Window,
                  _: &mut Context<gpui_kit::component::menu::PopupMenu>| {
                FilterOp::ALL.iter().fold(menu, |menu, op| {
                    let this = this.clone();
                    let op = *op;
                    menu.item(PopupMenuItem::new(op_label(op)).on_click(move |_, _, cx| {
                        let _ = this.update(cx, |tab, cx| {
                            if let Some(d) = tab.filter.drafts.iter_mut().find(|d| d.id == id) {
                                d.op = op;
                            }
                            cx.notify();
                        });
                    }))
                })
            }
        };
        h_flex()
            .gap_2()
            .child(conj_cell)
            .child(
                Button::new(("filter-col", id))
                    .small()
                    .outline()
                    .w(px(184.))
                    .label(if d.col.is_empty() {
                        t!("filter.pick_column").to_string()
                    } else {
                        d.col.clone()
                    })
                    .dropdown_caret(true)
                    .dropdown_menu(col_menu),
            )
            .child(
                Button::new(("filter-op", id))
                    .small()
                    .outline()
                    .w(px(120.))
                    .label(op_label(d.op))
                    .dropdown_caret(true)
                    .dropdown_menu(op_menu),
            )
            .child(if d.op.needs_value() {
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&d.value).small())
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_w_0()
                    .px_3()
                    .text_xs()
                    .italic()
                    .text_color(palette.fg_faint)
                    .child(t!("filter.no_value_needed"))
                    .into_any_element()
            })
            .child(
                Button::new(("filter-remove", id))
                    .ghost()
                    .xsmall()
                    .icon(ui::icon("x"))
                    .tooltip(t!("filter.remove_condition"))
                    .on_click(cx.listener(move |tab, _, _, cx| {
                        tab.filter.drafts.retain(|d| d.id != id);
                        cx.notify();
                    })),
            )
    });

    v_flex()
        .flex_shrink_0()
        .max_h(px(220.))
        .px_5()
        .pt_3()
        .pb_3()
        .gap_2()
        .bg(palette.panel)
        .border_b_1()
        .border_color(cx.theme().border)
        .children(rows)
        .when(tab.filter.drafts.is_empty(), |this| {
            this.child(
                div()
                    .py_2()
                    .text_sm()
                    .text_color(palette.fg_faint)
                    .child(t!("filter.empty")),
            )
        })
        .child(
            h_flex()
                .justify_between()
                .child(
                    Button::new("filter-add")
                        .ghost()
                        .small()
                        .icon(ui::icon("plus"))
                        .label(t!("filter.add_condition"))
                        .on_click(cx.listener(|tab, _, window, cx| tab.filter_add(window, cx))),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .child(
                            h_flex()
                                .gap_1()
                                .text_xs()
                                .text_color(palette.fg_faint)
                                .child(t!("filter.preview_match"))
                                .child(
                                    div()
                                        .font_family(mono)
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(palette.fg_soft)
                                        .child(preview.to_string()),
                                )
                                .child(t!("filter.of_total_rows", total = total)),
                        )
                        .when(can_clear, |this| {
                            this.child(
                                Button::new("filter-clear")
                                    .ghost()
                                    .small()
                                    .label(t!("filter.clear"))
                                    .on_click(cx.listener(|tab, _, _, cx| tab.filter_clear(cx))),
                            )
                        })
                        .child(
                            Button::new("filter-apply")
                                .small()
                                .icon(ui::icon("check"))
                                .label(if dirty {
                                    t!("filter.apply")
                                } else {
                                    t!("filter.applied")
                                })
                                .when(dirty, |b| b.primary())
                                .when(!dirty, |b| b.outline())
                                .disabled(!dirty)
                                .on_click(cx.listener(|tab, _, _, cx| {
                                    tab.filter_apply(cx);
                                    cx.emit(TabEvent::Changed);
                                })),
                        ),
                ),
        )
}
