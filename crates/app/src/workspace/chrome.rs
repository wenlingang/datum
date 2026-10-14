//! 窗口外框:顶栏(侧栏开关 + 连接路径 + 面板开关 + 主题 + 设置 + 品牌)、
//! 标签条、底部状态栏与空工作区。

use super::Workspace;
use super::ops::ExportFormat;
use crate::actions::{NewQuery, OpenSettings, ToggleAi, ToggleFilter, ToggleSidebar};
use crate::store::Stores;
use crate::tab::{PAGE_SIZE, TabKind};
use crate::theme::ActivePalette as _;
use crate::ui;
use datum_core::core::connection::DbKind;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, TitleBar, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, ClipboardItem, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window, div, img, px,
};
use rust_i18n::t;

impl Workspace {
    pub(super) fn render_top_bar(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = *cx.palette();
        let head = self.head_conn(cx);
        let table = self.active_tab(cx).and_then(|t| t.read(cx).table.clone());
        let has_update = Stores::global(cx).update.read(cx).status.has_update();
        let mono = cx.theme().mono_font_family.clone();

        let path = h_flex()
            .h(px(28.))
            .min_w_0()
            .px_3()
            .gap_2()
            .rounded_full()
            .bg(palette.raised)
            .border_1()
            .border_color(cx.theme().border)
            .text_sm()
            .overflow_hidden()
            .child(ui::dot(
                if head.is_some() {
                    cx.theme().success
                } else {
                    palette.fg_faint
                },
                6.,
            ))
            .map(|this| match &head {
                Some(c) => {
                    let db = if c.kind == DbKind::Sqlite {
                        c.file_path
                            .as_deref()
                            .and_then(|p| std::path::Path::new(p).file_name())
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "—".into())
                    } else if c.kind == DbKind::Redis {
                        format!("db{}", c.database)
                    } else {
                        c.database.clone()
                    };
                    this.child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .whitespace_nowrap()
                            .child(c.name.clone()),
                    )
                    .child(div().text_color(palette.fg_faint).child("/"))
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .child(db),
                    )
                    .when_some(table.clone(), |this, t| {
                        let copy = t.clone();
                        this.child(div().text_color(palette.fg_faint).child("/"))
                            .child(
                                div()
                                    .font_family(mono.clone())
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .whitespace_nowrap()
                                    .child(t),
                            )
                            .child(
                                Button::new("copy-table")
                                    .ghost()
                                    .xsmall()
                                    .icon(ui::icon("copy"))
                                    .tooltip(t!("workspace.copy_table_name"))
                                    .on_click(move |_, window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy.clone(),
                                        ));
                                        super::show_toast(
                                            &crate::store::Toast::Success(
                                                t!("workspace.copied").into(),
                                            ),
                                            window,
                                            cx,
                                        );
                                    }),
                            )
                    })
                }
                None => this.child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("workspace.not_connected")),
                ),
            });

        // TitleBar 把子元素放进避开红绿灯的内层,氛围层画在外包的一层上才能铺满整行
        let bar = TitleBar::new()
            .h(px(46.))
            .bg(gpui_kit::transparent_black())
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .min_w_0()
                    .gap_3()
                    .child(
                        Button::new("toggle-sidebar")
                            .ghost()
                            .small()
                            .icon(ui::icon("panel-left"))
                            .tooltip(if self.sidebar_open {
                                t!("workspace.hide_sidebar")
                            } else {
                                t!("workspace.show_sidebar")
                            })
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleSidebar), cx)
                            }),
                    )
                    .child(ui::vsep(cx))
                    .child(path),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .pr_3()
                    .gap_2()
                    .child(
                        Button::new("toggle-ai")
                            .ghost()
                            .small()
                            .selected(self.ai_open)
                            .icon(
                                ui::icon("sparkles")
                                    .when(self.ai_open, |i| i.text_color(palette.ai)),
                            )
                            .label(t!("workspace.ai_assistant"))
                            .tooltip(if self.ai_open {
                                t!("workspace.hide_ai")
                            } else {
                                t!("workspace.ai_assistant")
                            })
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleAi), cx)
                            }),
                    )
                    .child(
                        div()
                            .relative()
                            .child(
                                Button::new("open-settings")
                                    .ghost()
                                    .small()
                                    .icon(ui::icon("settings"))
                                    .tooltip(if has_update {
                                        t!("workspace.settings_update")
                                    } else {
                                        t!("workspace.settings")
                                    })
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(OpenSettings), cx)
                                    }),
                            )
                            .when(has_update, |this| {
                                this.child(
                                    div()
                                        .absolute()
                                        .top_1()
                                        .right_1()
                                        .size(px(6.))
                                        .rounded_full()
                                        .bg(palette.negative),
                                )
                            }),
                    ),
            );
        div()
            .relative()
            .flex_shrink_0()
            .bg(cx.theme().title_bar)
            .child(ui::brand_wash(cx, false))
            .child(bar)
            .child(ui::brand_line(cx, false, false))
    }

    pub(super) fn render_tab_strip(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = *cx.palette();
        let visible = self.visible_tabs(cx);
        let active = self.active_tab(cx);
        let active_ix = active
            .as_ref()
            .and_then(|a| visible.iter().position(|t| t.entity_id() == a.entity_id()));
        let this = cx.entity().downgrade();

        let tabs = visible.iter().map(|tab| {
            let t = tab.read(cx);
            let id = tab.entity_id();
            let is_active = Some(id) == active.as_ref().map(|a| a.entity_id());
            let renaming = self
                .renaming
                .as_ref()
                .filter(|(rid, _)| *rid == id)
                .map(|(_, i)| i.clone());
            let title = t.title.clone();
            let this2 = this.clone();
            let this3 = this.clone();
            let is_table = t.kind == TabKind::Table;
            Tab::new()
                .when(is_table, |tab| {
                    tab.child(div().mr_1p5().child(ui::icon("table-2").small().text_color(
                        if is_active {
                            palette.data
                        } else {
                            palette.fg_faint
                        },
                    )))
                })
                .child(match renaming {
                    Some(input) => div()
                        .w(px(140.))
                        .child(Input::new(&input).xsmall())
                        .into_any_element(),
                    None => div()
                        .id(("tab-title", id.as_u64() as usize))
                        .max_w(px(160.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_sm()
                        .when(is_active, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .on_click(move |e: &ClickEvent, window, cx| {
                            if e.click_count() >= 2 {
                                let _ = this2.update(cx, |ws, cx| ws.start_rename(id, window, cx));
                            }
                        })
                        .child(title)
                        .context_menu({
                            let ws = this.clone();
                            move |menu: PopupMenu, _, _| {
                                let (a, b, c) = (ws.clone(), ws.clone(), ws.clone());
                                menu.item(PopupMenuItem::new(t!("workspace.close_tab")).on_click(
                                    move |_, window, cx| {
                                        let _ = a.update(cx, |ws, cx| ws.close_tab(id, window, cx));
                                    },
                                ))
                                .item(
                                    PopupMenuItem::new(t!("workspace.close_other_tabs")).on_click(
                                        move |_, _, cx| {
                                            let _ =
                                                b.update(cx, |ws, cx| ws.close_other_tabs(id, cx));
                                        },
                                    ),
                                )
                                .separator()
                                .item(
                                    PopupMenuItem::new(t!("workspace.rename")).on_click(
                                        move |_, window, cx| {
                                            let _ = c.update(cx, |ws, cx| {
                                                ws.start_rename(id, window, cx)
                                            });
                                        },
                                    ),
                                )
                            }
                        })
                        .into_any_element(),
                })
                .child(
                    Button::new(("close-tab", id.as_u64() as usize))
                        .ghost()
                        .xsmall()
                        .ml_1()
                        .icon(ui::icon("x"))
                        .tooltip(t!("workspace.close"))
                        .on_click(move |_, window, cx| {
                            let _ = this3.update(cx, |ws, cx| ws.close_tab(id, window, cx));
                        }),
                )
                // 激活标签顶部的靛色条。Tab 的子元素都在内容区里,内容区几乎与 Tab 等高,
                // 绝对定位相对它(taffy 只认直接父元素);关闭按钮也放在内容区,色条才能横贯整个标签
                .when(is_active, |tab| {
                    tab.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h(px(2.))
                            .bg(cx.theme().primary),
                    )
                })
        });

        let tools = active
            .as_ref()
            .filter(|t| t.read(cx).result.is_some() && t.read(cx).err.is_none())
            .map(|tab| {
                let t = tab.read(cx);
                let filtered = t.applied_filter_count();
                let filter_active = t.filter.open || filtered > 0;
                let can_export = !t.view.is_empty();
                let ws = this.clone();
                h_flex()
                    .flex_shrink_0()
                    .h_full()
                    .px_3()
                    .gap_2()
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .when(filtered > 0, |this| {
                        this.child(
                            h_flex()
                                .h(px(22.))
                                .px_2p5()
                                .gap_1p5()
                                .rounded_full()
                                .bg(palette.selection)
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .text_color(palette.data)
                                .child(ui::icon("funnel").xsmall())
                                .child(t!("workspace.filtered_n", count = filtered)),
                        )
                    })
                    .child(
                        Button::new("toggle-filter")
                            .ghost()
                            .small()
                            .selected(filter_active)
                            .icon(ui::icon("funnel"))
                            .tooltip(t!("workspace.filter_tooltip"))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleFilter), cx)
                            }),
                    )
                    .child(
                        Button::new("export")
                            .ghost()
                            .small()
                            .icon(ui::icon("download"))
                            .tooltip(t!("workspace.export"))
                            .disabled(!can_export)
                            .dropdown_menu(move |menu: PopupMenu, _, _| {
                                ExportFormat::ALL.iter().fold(menu, |menu, f| {
                                    let f = *f;
                                    let ws = ws.clone();
                                    menu.item(PopupMenuItem::new(f.menu_label()).on_click(
                                        move |_, window, cx| {
                                            let _ = ws.update(cx, |ws, cx| {
                                                ws.export_filtered(f, window, cx)
                                            });
                                        },
                                    ))
                                })
                            }),
                    )
            });

        h_flex()
            .h(px(40.))
            .flex_shrink_0()
            .items_end()
            .bg(cx.theme().tab_bar)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex().h_full().px_2().child(
                    Button::new("new-query")
                        .ghost()
                        .small()
                        .icon(ui::icon("terminal").text_color(palette.data))
                        .tooltip(t!("workspace.new_query_tooltip"))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(NewQuery), cx)),
                ),
            )
            .child(
                div()
                    .id("tab-strip")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_end()
                    .child(
                        TabBar::new("tabs")
                            .track_scroll(&self.tab_scroll)
                            .when_some(active_ix, |bar, ix| bar.selected_index(ix))
                            .on_click({
                                let ws = this.clone();
                                let ids: Vec<_> = visible.clone();
                                move |ix: &usize, _, cx| {
                                    if let Some(tab) = ids.get(*ix).cloned() {
                                        let _ = ws.update(cx, |ws, cx| ws.focus_tab(&tab, cx));
                                    }
                                }
                            })
                            .children(tabs),
                    ),
            )
            .children(tools)
    }

    pub(super) fn render_status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *cx.palette();
        let head = self.head_conn(cx);
        let tab = self.active_tab(cx);
        let version = Stores::global(cx).update.read(cx).version.clone();
        let mono = cx.theme().mono_font_family.clone();
        let table_stats = tab
            .as_ref()
            .map(|t| t.read(cx))
            .filter(|t| t.kind == TabKind::Table && t.err.is_none() && t.result.is_some())
            .map(|t| {
                (
                    t.view.len(),
                    t.result.as_ref().map_or(0, |r| r.columns.len()),
                    t.elapsed_ms,
                )
            });
        let pager = tab
            .as_ref()
            .map(|t| t.read(cx))
            .filter(|t| t.browse_table.is_some())
            .map(|t| (t.page, t.can_prev(), t.can_next()));

        let bar = h_flex()
            .relative()
            .h(px(28.))
            .flex_shrink_0()
            .pl_3p5()
            .pr_2p5()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_xs()
            .child(
                h_flex()
                    .gap_2p5()
                    .overflow_hidden()
                    .when_some(head, |this, c| {
                        this.child(
                            h_flex()
                                .gap_1p5()
                                .child(ui::dot(cx.theme().success, 6.))
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(t!("workspace.connected")),
                                )
                                .child(
                                    div()
                                        .font_family(mono.clone())
                                        .text_color(palette.fg_faint)
                                        .child(c.kind.label()),
                                ),
                        )
                    })
                    .when_some(table_stats, |this, (rows, cols, elapsed)| {
                        this.child(div().w(px(1.)).h(px(12.)).bg(palette.hairline))
                            .child(ui::dot(palette.data, 5.))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        div()
                                            .font_family(mono.clone())
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(cx.theme().foreground)
                                            .child(rows.to_string()),
                                    )
                                    .child(t!("workspace.rows_unit"))
                                    .child(
                                        div()
                                            .font_family(mono.clone())
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(cx.theme().foreground)
                                            .child(cols.to_string()),
                                    )
                                    .child(t!("workspace.cols_unit")),
                            )
                            .when_some(elapsed, |this, ms| {
                                this.child(
                                    div()
                                        .font_family(mono.clone())
                                        .text_color(palette.fg_faint)
                                        .child(t!("workspace.elapsed_ms", ms = ms)),
                                )
                            })
                    }),
            )
            .child(
                h_flex()
                    .gap_2p5()
                    .when_some(pager, |this, (page, can_prev, can_next)| {
                        let (prev, next) = (cx.entity().downgrade(), cx.entity().downgrade());
                        this.child(
                            h_flex()
                                .gap_1()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("workspace.page_prefix"))
                                .child(
                                    div()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(cx.theme().foreground)
                                        .child((page + 1).to_string()),
                                )
                                .child(t!("workspace.page_suffix", size = PAGE_SIZE)),
                        )
                        .child(
                            Button::new("prev-page")
                                .ghost()
                                .xsmall()
                                .icon(ui::icon("chevron-left"))
                                .tooltip(t!("workspace.prev_page"))
                                .disabled(!can_prev)
                                .on_click(move |_, window, cx| {
                                    let _ = prev.update(cx, |ws, cx| {
                                        if let Some(t) = ws.active_tab(cx) {
                                            t.update(cx, |t, cx| {
                                                t.goto_page(page.saturating_sub(1), window, cx)
                                            });
                                        }
                                    });
                                }),
                        )
                        .child(
                            Button::new("next-page")
                                .ghost()
                                .xsmall()
                                .icon(ui::icon("chevron-right"))
                                .tooltip(t!("workspace.next_page"))
                                .disabled(!can_next)
                                .on_click(move |_, window, cx| {
                                    let _ = next.update(cx, |ws, cx| {
                                        if let Some(t) = ws.active_tab(cx) {
                                            t.update(cx, |t, cx| t.goto_page(page + 1, window, cx));
                                        }
                                    });
                                }),
                        )
                    })
                    .child(
                        div()
                            .font_family(mono)
                            .text_color(palette.fg_faint)
                            .child(format!("v{version}")),
                    ),
            );
        div()
            .relative()
            .flex_shrink_0()
            .bg(cx.theme().status_bar)
            .child(ui::brand_wash(cx, true))
            .child(bar)
            .child(ui::brand_line(cx, true, true))
    }
}

/// 无标签页时的空白工作区。
pub fn empty_workspace(cx: &App) -> impl IntoElement {
    let palette = *cx.palette();
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_1()
        .p_6()
        .bg(palette.editor)
        .child(img("app-icon.png").size(px(112.)).opacity(0.92))
        .child(
            div()
                .mt_6()
                .text_sm()
                .font_weight(FontWeight::BOLD)
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(palette.fg_soft)
                .child(t!("workspace.ready")),
        )
        .child(
            div()
                .mt_1p5()
                .text_sm()
                .text_color(palette.fg_faint)
                .child(t!("workspace.empty_hint")),
        )
        .child(
            div().mt_4().child(
                Button::new("empty-new-query")
                    .primary()
                    .small()
                    .icon(ui::icon("code-xml"))
                    .label(t!("workspace.new_sql_query"))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NewQuery), cx)),
            ),
        )
}
