//! 右侧详情栏:选中行时显示行详情;Redis 显示 key 详情;否则显示表结构(字段 + 索引)。

use super::Workspace;
use crate::theme::ActivePalette as _;
use crate::ui;
use datum_core::pg::browse::TableDetail;
use datum_core::rds::client::KeyDetail;
use datum_core::sql::build::build_insert;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClipboardItem, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use rust_i18n::t;
use std::collections::HashMap;

fn header(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(cx.theme().border)
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

fn row_detail(
    columns: &[String],
    values: &[Option<String>],
    types: &HashMap<String, String>,
    cx: &App,
) -> impl IntoElement {
    let palette = *cx.palette();
    let mono = cx.theme().mono_font_family.clone();
    v_flex()
        .size_full()
        .child(header(t!("detail.row_detail"), cx))
        .child(
            v_flex()
                .id("row-detail")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(columns.iter().enumerate().map(|(i, c)| {
                    let value = values.get(i).cloned().flatten();
                    v_flex()
                        .px_3()
                        .py_2()
                        .gap_1()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(c.clone()),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(10.))
                                        .text_color(palette.data)
                                        .child(types.get(c).cloned().unwrap_or_else(|| "—".into())),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(if value.is_some() {
                                    cx.theme().foreground
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .child(value.unwrap_or_else(|| "NULL".into())),
                        )
                })),
        )
}

fn redis_detail(d: &KeyDetail, cx: &App) -> impl IntoElement {
    let ttl = match d.ttl {
        -1 => t!("detail.ttl_never").to_string(),
        -2 => t!("detail.ttl_missing").to_string(),
        n => format!("{n}s"),
    };
    let palette = *cx.palette();
    let line = |label: SharedString, value: gpui_kit::AnyElement| {
        h_flex()
            .gap_2()
            .text_xs()
            .child(
                div()
                    .w(px(36.))
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(div().min_w_0().child(value))
    };
    v_flex()
        .size_full()
        .child(header(t!("detail.key_detail"), cx))
        .child(
            v_flex()
                .p_3()
                .gap_2()
                .child(line(
                    t!("detail.key").into(),
                    div()
                        .font_weight(FontWeight::BOLD)
                        .child(d.key.clone())
                        .into_any_element(),
                ))
                .child(line(
                    t!("detail.type").into(),
                    div()
                        .text_color(palette.data)
                        .child(d.key_type.clone())
                        .into_any_element(),
                ))
                .child(line("TTL".into(), div().child(ttl).into_any_element()))
                .child(line(
                    t!("detail.size").into(),
                    div().child(d.size.to_string()).into_any_element(),
                )),
        )
}

fn table_detail(
    table: &str,
    detail: &TableDetail,
    insert_sql: String,
    cx: &App,
) -> impl IntoElement {
    let palette = *cx.palette();
    let check = |on: bool| {
        if on {
            div().text_color(cx.theme().primary).child("✓")
        } else {
            div()
        }
    };
    let dim = |v: &Option<String>| match v.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => div().child(s.to_string()),
        None => div().text_color(cx.theme().muted_foreground).child("—"),
    };
    let cell = |w: f32| {
        div()
            .w(px(w))
            .flex_shrink_0()
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
    };
    let head_row = |cols: &[(&'static str, f32)]| {
        h_flex()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .text_color(cx.theme().muted_foreground)
            .font_weight(FontWeight::SEMIBOLD)
            .children(cols.iter().map(|(n, w)| cell(*w).child(*n)))
    };

    let fields = v_flex()
        .id("detail-fields")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(
            div()
                .px_3()
                .py_1()
                .text_size(px(10.))
                .text_color(cx.theme().muted_foreground)
                .child(t!("detail.columns_n", count = detail.columns.len())),
        )
        .child(head_row(&[
            ("Name", 120.),
            ("Type", 110.),
            ("Default", 90.),
            ("Comment", 110.),
            ("NotNull", 56.),
            ("Primary", 56.),
        ]))
        .children(detail.columns.iter().map(|c| {
            let sql = insert_sql.clone();
            h_flex()
                .id(SharedString::from(format!("col-{}", c.name)))
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    cell(120.)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(c.name.clone()),
                )
                .child(
                    cell(110.)
                        .text_color(palette.data)
                        .child(c.data_type.clone()),
                )
                .child(cell(90.).child(dim(&c.default)))
                .child(cell(110.).child(dim(&c.comment)))
                .child(cell(56.).child(check(c.not_null)))
                .child(cell(56.).child(check(c.is_pk)))
                .context_menu(move |menu: PopupMenu, _, _| {
                    let sql = sql.clone();
                    menu.item(PopupMenuItem::new(t!("detail.copy_insert")).on_click(
                        move |_, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(sql.clone()));
                            super::show_toast(
                                &crate::store::Toast::Success(t!("workspace.copied").into()),
                                window,
                                cx,
                            );
                        },
                    ))
                })
        }));

    let indexes = v_flex()
        .id("detail-indexes")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .border_t_2()
        .border_color(cx.theme().border)
        .child(
            div()
                .px_3()
                .py_1()
                .text_size(px(10.))
                .text_color(cx.theme().muted_foreground)
                .child(t!("detail.indexes_n", count = detail.indexes.len())),
        )
        .child(head_row(&[
            ("Name", 160.),
            ("Columns", 160.),
            ("Primary", 56.),
            ("Unique", 56.),
        ]))
        .children(detail.indexes.iter().map(|i| {
            h_flex()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(cell(160.).child(i.name.clone()))
                .child(cell(160.).child(i.columns.clone()))
                .child(cell(56.).child(check(i.is_primary)))
                .child(cell(56.).child(check(i.is_unique)))
        }))
        .when(detail.indexes.is_empty(), |this| {
            this.child(
                div()
                    .px_3()
                    .py_1()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("detail.no_indexes")),
            )
        });

    v_flex()
        .size_full()
        .text_xs()
        .child(header(t!("detail.table_title", name = table), cx))
        .child(
            div()
                .id("detail-scroll-x")
                .flex_1()
                .min_h_0()
                .overflow_x_scroll()
                .child(
                    v_flex()
                        .min_w(px(560.))
                        .h_full()
                        .child(fields)
                        .child(indexes),
                ),
        )
}

impl Workspace {
    pub(super) fn render_detail(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = div()
            .size_full()
            .bg(cx.palette().panel)
            .border_l_1()
            .border_color(cx.theme().border);
        let Some(tab) = self.active_tab(cx) else {
            return panel.child(
                div()
                    .p_2()
                    .child(ui::section_label(t!("detail.details"), cx)),
            );
        };
        let t = tab.read(cx);
        if let (Some(result), Some(row_ix)) = (&t.result, t.selected_row) {
            if let Some(values) = t.view_row(row_ix) {
                if let Some(m) = &t.mongo {
                    // Mongo:该文档没有的字段不列出(缺失 ≠ null)
                    let (cols, vals): (Vec<String>, Vec<_>) = result
                        .columns
                        .iter()
                        .cloned()
                        .zip(values.iter().cloned())
                        .filter(|(_, v)| v.is_some())
                        .unzip();
                    return panel.child(row_detail(&cols, &vals, &m.types, cx));
                }
                let types = t.column_types();
                return panel.child(row_detail(&result.columns, values, &types, cx));
            }
        }
        if let Some(d) = &t.redis_detail {
            return panel.child(redis_detail(d, cx));
        }
        if let (Some(detail), Some(table), Some(dialect)) = (&t.detail, &t.table, t.dialect()) {
            let insert = build_insert(table, detail, dialect);
            return panel.child(table_detail(table, detail, insert, cx));
        }
        panel.child(
            div()
                .p_2()
                .child(ui::section_label(t!("detail.details"), cx)),
        )
    }
}
