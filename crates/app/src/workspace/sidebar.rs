//! 左侧栏:连接图标竖轨 + 表栏(连接头 / 库切换 / 搜索 / 表列表)。

use super::Workspace;
use crate::store::Stores;
use crate::theme::ActivePalette as _;
use crate::ui::{self, EnvBadge, MiddleEllipsis};
use datum_core::core::connection::{Connection, DbKind};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, WeakEntity, Window, div, px,
};
use rust_i18n::t;

pub struct Sidebar {
    workspace: WeakEntity<Workspace>,
    search: Entity<InputState>,
    /// 库列表是否展开
    db_open: bool,
    db_loading: bool,
    /// 搜索按连接隔离:切换连接时清空
    last_conn: Option<String>,
    pending_clear: bool,
    _subscriptions: Vec<Subscription>,
}

fn display_db(kind: DbKind, db: &str) -> String {
    if kind == DbKind::Redis {
        format!("db{db}")
    } else {
        db.to_string()
    }
}

impl Sidebar {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("sidebar.search_tables")));
        let conns = Stores::global(cx).conns.clone();
        let subs = vec![
            cx.observe(&conns, |this, conns, cx| {
                let active = conns.read(cx).active_id.clone();
                if active != this.last_conn {
                    this.last_conn = active;
                    this.db_open = false;
                    this.pending_clear = true;
                }
                cx.notify();
            }),
            cx.subscribe(&search, |_, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        Self {
            workspace,
            search,
            db_open: false,
            db_loading: false,
            last_conn: None,
            pending_clear: false,
            _subscriptions: subs,
        }
    }

    fn ws(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Workspace, &mut Context<Workspace>)) {
        let _ = self.workspace.update(cx, f);
    }

    /// 点竖轨图标:聚焦该连接,首次点击懒加载表列表。
    fn select_conn(&mut self, id: &str, cx: &mut Context<Self>) {
        let conns = Stores::global(cx).conns.clone();
        conns.update(cx, |s, cx| {
            s.activate(id, cx);
            if !s.tables.contains_key(id) {
                s.load_tables(id, cx).detach();
            }
        });
    }

    fn toggle_db_list(&mut self, conn: &Connection, cx: &mut Context<Self>) {
        if self.db_open {
            self.db_open = false;
            cx.notify();
            return;
        }
        self.db_open = true;
        self.db_loading = true;
        let conns = Stores::global(cx).conns.clone();
        let task = conns.update(cx, |s, cx| s.load_databases(&conn.id, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.db_loading = false;
                if let Err(e) = result {
                    this.db_open = false;
                    let msg = t!("sidebar.load_dbs_failed", error = e.display()).to_string();
                    let conns = Stores::global(cx).conns.clone();
                    conns.update(cx, |_, cx| cx.emit(crate::store::Toast::Error(msg.into())));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_rail(
        &self,
        conns: &[Connection],
        active: Option<&str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = *cx.palette();
        let this = cx.entity().downgrade();
        v_flex()
            .id("conn-rail")
            .w(px(48.))
            .flex_shrink_0()
            .h_full()
            .py_2()
            .gap_1p5()
            .items_center()
            .overflow_y_scroll()
            .bg(palette.rail)
            .border_r_1()
            .border_color(cx.theme().border)
            .children(conns.iter().map(|c| {
                let is_active = active == Some(c.id.as_str());
                let id = c.id.clone();
                let menu_conn = c.clone();
                let ws = self.workspace.clone();
                let env_color = ui::env_color(c.env, cx);
                div()
                    .id(SharedString::from(format!("rail-{}", c.id)))
                    .relative()
                    .size(px(32.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .cursor_pointer()
                    .when(is_active, |d| d.bg(cx.theme().primary))
                    .when(!is_active, |d| {
                        d.border_1()
                            .border_color(palette.hairline)
                            .hover(|s| s.bg(palette.raised))
                    })
                    .tooltip({
                        let label: SharedString =
                            format!("{} · {}", c.name, ui::env_label(c.env).to_lowercase()).into();
                        move |window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(label.clone())
                                .build(window, cx)
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.select_conn(&id, cx)))
                    .child(ui::db_icon(c.kind).size(px(16.)).text_color(if is_active {
                        cx.theme().primary_foreground
                    } else {
                        cx.theme().muted_foreground
                    }))
                    .child(
                        div()
                            .absolute()
                            .right(px(-1.))
                            .bottom(px(-1.))
                            .size(px(9.))
                            .rounded_full()
                            .bg(env_color)
                            .border_2()
                            .border_color(palette.rail),
                    )
                    .context_menu(move |menu: PopupMenu, _, _| conn_menu(menu, &ws, &menu_conn))
            }))
            .when(!conns.is_empty(), |this| {
                this.child(
                    div()
                        .w(px(20.))
                        .h(px(1.))
                        .flex_shrink_0()
                        .my_1()
                        .bg(palette.hairline),
                )
            })
            .child(
                Button::new("new-conn")
                    .ghost()
                    .icon(ui::icon("plus"))
                    .tooltip(t!("sidebar.new_connection"))
                    .on_click(move |_, window, cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.ws(cx, |ws, cx| ws.open_connection_form(None, window, cx))
                        });
                    }),
            )
    }

    fn render_tables(
        &mut self,
        conn: &Connection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if std::mem::take(&mut self.pending_clear) {
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
        }
        let palette = *cx.palette();
        let store = Stores::global(cx).conns.read(cx);
        let tables = store.tables.get(&conn.id).cloned();
        let loading = store.is_loading(&conn.id);
        let databases = store.databases.get(&conn.id).cloned();
        let ws_entity = self.workspace.upgrade();
        let active_table = ws_entity.as_ref().and_then(|ws| {
            let tab = ws.read(cx).active_tab(cx)?;
            let t = tab.read(cx);
            (t.conn_id == conn.id).then(|| t.table.clone()).flatten()
        });
        let query = self.search.read(cx).value().trim().to_lowercase();
        let filtered: Vec<String> = tables
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|t| query.is_empty() || t.to_lowercase().contains(&query))
            .collect();
        let mono = cx.theme().mono_font_family.clone();

        let header = {
            let ws = self.workspace.clone();
            let menu_conn = conn.clone();
            h_flex()
                .h(px(44.))
                .flex_shrink_0()
                .pl_3()
                .pr_2()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .font_weight(FontWeight::BOLD)
                        .child(MiddleEllipsis::new(conn.name.clone())),
                )
                .child(EnvBadge::new(conn.env))
                // 关系图针对当前连接的库;Redis 没有关系图
                .when(conn.kind != DbKind::Redis, |this| this.child(erd_button()))
                .child(
                    Button::new("conn-actions")
                        .ghost()
                        .xsmall()
                        .icon(ui::icon("ellipsis"))
                        .tooltip(t!("sidebar.conn_actions"))
                        .dropdown_menu(move |menu: PopupMenu, _, _| {
                            conn_menu(menu, &ws, &menu_conn)
                        }),
                )
        };

        let switcher = (conn.kind != DbKind::Sqlite).then(|| {
            let conn2 = conn.clone();
            let open = self.db_open;
            v_flex()
                .flex_shrink_0()
                .px_2()
                .pb_1p5()
                .gap_1p5()
                .child(
                    h_flex().gap_1p5().child(
                        h_flex()
                            .id("db-switcher")
                            .flex_1()
                            .min_w_0()
                            .h(px(30.))
                            .pl_2p5()
                            .pr_2()
                            .gap_2()
                            .rounded(px(6.))
                            .bg(palette.raised)
                            .border_1()
                            .border_color(if open {
                                cx.theme().primary
                            } else {
                                palette.hairline
                            })
                            .cursor_pointer()
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_db_list(&conn2, cx)),
                            )
                            .child(
                                ui::db_icon(conn.kind)
                                    .size(px(13.))
                                    .text_color(palette.data),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_family(mono.clone())
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(MiddleEllipsis::new(display_db(
                                        conn.kind,
                                        &conn.database,
                                    ))),
                            )
                            .when_some(tables.as_ref().map(Vec::len), |this, n| {
                                this.child(
                                    div()
                                        .font_family(mono.clone())
                                        .text_size(px(10.))
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(palette.fg_faint)
                                        .child(n.to_string()),
                                )
                            })
                            .child(
                                ui::icon(if open { "chevron-up" } else { "chevron-down" })
                                    .xsmall()
                                    .text_color(cx.theme().muted_foreground),
                            ),
                    ),
                )
                .when(open, |this| {
                    this.child(self.render_db_list(conn, databases.as_deref(), cx))
                })
        });

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .children(switcher)
            .child(
                h_flex().px_2().pb_2().gap_1p5().flex_shrink_0().child(
                    div().flex_1().min_w_0().child(
                        Input::new(&self.search)
                            .small()
                            .prefix(ui::icon("search").small().text_color(palette.fg_faint))
                            .cleanable(true),
                    ),
                ),
            )
            .child(
                v_flex()
                    .id("table-list")
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .pb_1p5()
                    .overflow_y_scroll()
                    .when(self.db_open, |this| this.opacity(0.4))
                    .when(tables.is_none(), |this| {
                        let conn_id = conn.id.clone();
                        this.child(if loading {
                            div()
                                .px_3()
                                .py_2()
                                .text_xs()
                                .text_color(palette.fg_faint)
                                .child(t!("sidebar.loading"))
                                .into_any_element()
                        } else {
                            h_flex()
                                .px_3()
                                .py_2()
                                .gap_2()
                                .text_xs()
                                .text_color(palette.fg_faint)
                                .child(t!("sidebar.not_loaded"))
                                .child(
                                    Button::new("retry-tables")
                                        .ghost()
                                        .xsmall()
                                        .label(t!("sidebar.retry"))
                                        .on_click(move |_, _, cx| {
                                            let conns = Stores::global(cx).conns.clone();
                                            conns.update(cx, |s, cx| {
                                                s.load_tables(&conn_id, cx).detach()
                                            });
                                        }),
                                )
                                .into_any_element()
                        })
                    })
                    .children(filtered.into_iter().map(|t| {
                        let is_active = active_table.as_deref() == Some(t.as_str());
                        let (open_t, menu_t) = (t.clone(), t.clone());
                        let conn_id = conn.id.clone();
                        let kind = conn.kind;
                        let ws = self.workspace.clone();
                        let ws2 = self.workspace.clone();
                        h_flex()
                            .id(SharedString::from(format!("tbl-{t}")))
                            .relative()
                            .h(px(30.))
                            .flex_shrink_0()
                            .pl_3()
                            .pr_2p5()
                            .gap_2()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .when(is_active, |d| d.bg(cx.theme().sidebar_accent))
                            .when(!is_active, |d| d.hover(|s| s.bg(cx.theme().list_hover)))
                            .on_click({
                                let conn_id = conn_id.clone();
                                move |_, window, cx| {
                                    let _ = ws.update(cx, |ws, cx| {
                                        ws.open_table(&conn_id, &open_t, window, cx);
                                    });
                                }
                            })
                            .when(is_active, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .left_0()
                                        .top(px(5.))
                                        .bottom(px(5.))
                                        .w(px(3.))
                                        .rounded_r(px(3.))
                                        .bg(cx.theme().primary),
                                )
                            })
                            .child(
                                ui::icon(if kind == DbKind::Redis {
                                    "key-round"
                                } else {
                                    "table"
                                })
                                .size(px(13.))
                                .text_color(if is_active {
                                    palette.data
                                } else {
                                    cx.theme().primary
                                })
                                .when(!is_active, |i| i.opacity(0.72)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_weight(if is_active {
                                        FontWeight::BOLD
                                    } else {
                                        FontWeight::MEDIUM
                                    })
                                    .text_color(if is_active {
                                        cx.theme().foreground
                                    } else {
                                        palette.fg_soft
                                    })
                                    .child(MiddleEllipsis::new(t.clone())),
                            )
                            .context_menu(move |menu: PopupMenu, _, _| {
                                table_menu(menu, &ws2, &conn_id, kind, &menu_t)
                            })
                    })),
            )
    }

    fn render_db_list(
        &self,
        conn: &Connection,
        databases: Option<&[String]>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = *cx.palette();
        let mono = cx.theme().mono_font_family.clone();
        v_flex()
            .id("db-list")
            .max_h(px(260.))
            .overflow_y_scroll()
            .p_1()
            .rounded(px(8.))
            .bg(palette.panel)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_md()
            .map(|this| match databases {
                None => this.child(
                    div()
                        .h(px(28.))
                        .pl_7()
                        .flex()
                        .items_center()
                        .text_xs()
                        .text_color(palette.fg_faint)
                        .child(t!("sidebar.loading")),
                ),
                Some(dbs) => this.children(dbs.iter().map(|db| {
                    let current = *db == conn.database;
                    let pick_db = db.clone();
                    let menu_db = db.clone();
                    let ws = self.workspace.clone();
                    let ws2 = self.workspace.clone();
                    let kind = conn.kind;
                    h_flex()
                        .id(SharedString::from(format!("db-{db}")))
                        .h(px(28.))
                        .flex_shrink_0()
                        .px_2()
                        .gap_2()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(|s| s.bg(cx.theme().list_hover))
                        .when(!current, |d| d.pl_7())
                        .when(current, |d| {
                            d.child(ui::icon("check").xsmall().text_color(palette.data))
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_family(mono.clone())
                                .text_xs()
                                .font_weight(if current {
                                    FontWeight::BOLD
                                } else {
                                    FontWeight::NORMAL
                                })
                                .text_color(if current {
                                    cx.theme().foreground
                                } else {
                                    palette.fg_soft
                                })
                                .child(MiddleEllipsis::new(display_db(kind, db))),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.db_open = false;
                            let db = pick_db.clone();
                            let _ = ws.update(cx, |ws, cx| ws.pick_database(&db, window, cx));
                            cx.notify();
                        }))
                        .context_menu(move |menu: PopupMenu, _, _| {
                            db_menu(menu, &ws2, kind, &menu_db)
                        })
                })),
            })
            .child(div().h(px(1.)).mx_1p5().my_1().bg(cx.theme().border))
            .child(
                div()
                    .min_h(px(26.))
                    .py_1()
                    .pl_7()
                    .pr_2()
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(palette.fg_faint)
                    .child(t!("sidebar.db_switch_hint")),
            )
    }
}

/// 连接右键 / 「…」菜单。
fn conn_menu(menu: PopupMenu, ws: &WeakEntity<Workspace>, conn: &Connection) -> PopupMenu {
    let item =
        |label: SharedString,
         f: std::rc::Rc<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>| {
            let ws = ws.clone();
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let f = f.clone();
                let _ = ws.update(cx, |ws, cx| f(ws, window, cx));
            })
        };
    let (c1, c2, c3, c4, c5) = (
        conn.clone(),
        conn.clone(),
        conn.clone(),
        conn.clone(),
        conn.clone(),
    );
    let mut menu = menu
        .item(item(
            t!("sidebar.edit").into(),
            std::rc::Rc::new(move |ws, w, cx| ws.open_connection_form(Some(c1.clone()), w, cx)),
        ))
        .item(item(
            t!("sidebar.refresh_tables").into(),
            std::rc::Rc::new(move |_, _, cx| {
                let conns = Stores::global(cx).conns.clone();
                conns.update(cx, |s, cx| s.load_tables(&c2.id, cx).detach());
            }),
        ));
    if conn.kind.is_relational() {
        menu = menu
            .item(item(
                t!("sidebar.import_sql").into(),
                std::rc::Rc::new(move |ws, w, cx| ws.open_import(Some(c3.id.clone()), w, cx)),
            ))
            .item(item(
                t!("sidebar.export_database").into(),
                std::rc::Rc::new(move |ws, w, cx| ws.export_database(c4.id.clone(), w, cx)),
            ));
    }
    menu.separator().item(item(
        t!("sidebar.delete").into(),
        std::rc::Rc::new(move |ws, w, cx| ws.confirm_delete_connection(c5.clone(), w, cx)),
    ))
}

/// 表 / key 右键菜单。
fn table_menu(
    menu: PopupMenu,
    ws: &WeakEntity<Workspace>,
    conn_id: &str,
    kind: DbKind,
    table: &str,
) -> PopupMenu {
    let item =
        |label: SharedString,
         f: std::rc::Rc<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>| {
            let ws = ws.clone();
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let f = f.clone();
                let _ = ws.update(cx, |ws, cx| f(ws, window, cx));
            })
        };
    let copy = table.to_string();
    let copy_item = |label: SharedString| {
        let copy = copy.clone();
        PopupMenuItem::new(label).on_click(move |_, window, cx| {
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(copy.clone()));
            super::show_toast(
                &crate::store::Toast::Success(t!("workspace.copied").into()),
                window,
                cx,
            );
        })
    };
    if kind == DbKind::Redis {
        return menu.item(copy_item(t!("sidebar.copy_key_name").into()));
    }
    let (c1, t1, c2, t2, c3, t3) = (
        conn_id.to_string(),
        table.to_string(),
        conn_id.to_string(),
        table.to_string(),
        conn_id.to_string(),
        table.to_string(),
    );
    let mut menu = menu
        .item(item(
            t!("sidebar.view_table_detail").into(),
            std::rc::Rc::new(move |ws, w, cx| ws.show_table_detail(&c1, &t1, w, cx)),
        ))
        .item(copy_item(t!("workspace.copy_table_name").into()))
        .item(item(
            t!("sidebar.view_ddl").into(),
            std::rc::Rc::new(move |ws, w, cx| ws.show_ddl(&c2, &t2, w, cx)),
        ))
        .separator();
    for f in super::ops::ExportFormat::ALL {
        let (c, t) = (c3.clone(), t3.clone());
        menu = menu.item(item(
            f.table_menu_label().into(),
            std::rc::Rc::new(move |ws, w, cx| ws.export_table(c.clone(), t.clone(), f, w, cx)),
        ));
    }
    menu
}

/// 库列表右键:编辑连接 / 导出 / DROP(Redis 库是固定索引,仅编辑)。
fn db_menu(menu: PopupMenu, ws: &WeakEntity<Workspace>, kind: DbKind, db: &str) -> PopupMenu {
    let item =
        |label: SharedString,
         f: std::rc::Rc<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>| {
            let ws = ws.clone();
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let f = f.clone();
                let _ = ws.update(cx, |ws, cx| f(ws, window, cx));
            })
        };
    let (d1, d2, d3) = (db.to_string(), db.to_string(), db.to_string());
    let menu = menu.item(item(
        t!("sidebar.edit_connection").into(),
        std::rc::Rc::new(move |ws, w, cx| ws.edit_conn_with_db(&d1, w, cx)),
    ));
    if kind == DbKind::Redis {
        return menu;
    }
    menu.item(item(
        t!("sidebar.export_database").into(),
        std::rc::Rc::new(move |ws, w, cx| ws.export_db_by_name(&d2, w, cx)),
    ))
    .separator()
    .item(item(
        t!("sidebar.drop_database").into(),
        std::rc::Rc::new(move |ws, w, cx| ws.drop_db_by_name(&d3, w, cx)),
    ))
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = Stores::global(cx).conns.read(cx);
        let conns = store.connections.clone();
        let active = store.active().cloned();
        let palette = *cx.palette();
        h_flex()
            .size_full()
            .items_stretch()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(self.render_rail(&conns, active.as_ref().map(|c| c.id.as_str()), cx))
            .child(match active {
                Some(conn) => self.render_tables(&conn, window, cx).into_any_element(),
                None => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_4()
                    .text_xs()
                    .text_color(palette.fg_faint)
                    .child(if conns.is_empty() {
                        t!("sidebar.empty_no_conns")
                    } else {
                        t!("sidebar.empty_pick_conn")
                    })
                    .into_any_element(),
            })
    }
}

/// 打开当前库的关系图。
fn erd_button() -> Button {
    Button::new("sidebar-erd")
        .ghost()
        .small()
        .icon(ui::icon("network"))
        .tooltip(t!("workspace.relationship_diagram"))
        .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::actions::OpenErd), cx))
}
