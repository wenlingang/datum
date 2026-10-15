//! 主窗口:顶栏 + 侧栏 + 标签页工作区 + 详情栏 + AI 面板 + 状态栏。
//!
//! 标签页按连接隔离:中间栏只显示当前聚焦连接的 tab,每个连接各自记住激活的 tab。

mod chrome;
mod detail;
mod ops;
mod sidebar;

use crate::actions::*;
use crate::chat::{ChatEvent as ChatPanelEvent, ChatPanel};
use crate::store::{Stores, Toast};
use crate::tab::{TabEvent, TabKind, WorkTab};
use crate::theme::{self, ActivePalette as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::{
    AppContext as _, Context, Entity, EntityId, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, ScrollHandle, SharedString, Styled as _, Subscription,
    Window, div, px,
};
pub use ops::save_bytes;
use rust_i18n::t;
pub use sidebar::Sidebar;
use std::collections::HashMap;

pub struct Workspace {
    focus: FocusHandle,
    sidebar: Entity<Sidebar>,
    chat: Entity<ChatPanel>,
    tabs: Vec<Entity<WorkTab>>,
    /// 每个连接记住的激活 tab
    active_by_conn: HashMap<String, EntityId>,
    /// 正在双击重命名的 tab
    renaming: Option<(EntityId, Entity<InputState>)>,
    sidebar_open: bool,
    detail_open: bool,
    ai_open: bool,
    tab_scroll: ScrollHandle,
    /// 整库导出是长任务,防重入
    exporting_db: bool,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let this = cx.entity().downgrade();
        let sidebar = cx.new(|cx| Sidebar::new(this.clone(), window, cx));
        let chat = cx.new(|cx| ChatPanel::new(this.clone(), window, cx));
        let stores = Stores::global(cx).clone();
        let mut subs = vec![
            cx.observe(&stores.conns, |_, _, cx| cx.notify()),
            cx.observe(&stores.update, |_, _, cx| cx.notify()),
            cx.subscribe_in(&stores.conns, window, |_, _, toast: &Toast, window, cx| {
                show_toast(toast, window, cx)
            }),
            cx.subscribe_in(&stores.ai, window, |_, _, toast: &Toast, window, cx| {
                show_toast(toast, window, cx)
            }),
            cx.subscribe_in(
                &chat,
                window,
                |this, _, event: &ChatPanelEvent, window, cx| match event {
                    ChatPanelEvent::InsertAndRun {
                        sql,
                        conn_id,
                        confirmed,
                    } => this.insert_and_run(sql.clone(), conn_id, *confirmed, window, cx),
                    ChatPanelEvent::OpenSettings => this.open_settings(window, cx),
                    ChatPanelEvent::QuoteSelection => {
                        let text = this
                            .active_tab(cx)
                            .map(|t| t.read(cx).selection_text(cx).trim().to_string())
                            .unwrap_or_default();
                        if text.is_empty() {
                            show_toast(
                                &Toast::Error(t!("workspace.quote_select_first").into()),
                                window,
                                cx,
                            );
                        } else {
                            this.send_to_ai(text, cx);
                        }
                    }
                    ChatPanelEvent::Toast(t) => show_toast(t, window, cx),
                },
            ),
            // 跟随系统外观
            cx.observe_window_appearance(window, |_, window, cx| {
                if crate::prefs::Prefs::global(cx).theme == crate::prefs::ThemePreference::System {
                    theme::apply(Some(window.appearance()), cx);
                }
            }),
        ];
        subs.push(cx.observe_global::<crate::prefs::Prefs>(|_, cx| cx.notify()));
        // macOS 惯例:点关闭只隐藏应用,留在 Dock;点 Dock 图标唤回,工作区状态不丢
        #[cfg(target_os = "macos")]
        window.on_window_should_close(cx, |_, cx| {
            cx.hide();
            false
        });
        let focus = cx.focus_handle();
        // 启动即让工作区持有焦点:否则没有焦点路径,Workspace 上下文的快捷键全部失效
        focus.focus(window, cx);
        Self {
            focus,
            sidebar,
            chat,
            tabs: vec![],
            active_by_conn: HashMap::new(),
            renaming: None,
            sidebar_open: true,
            detail_open: false,
            ai_open: false,
            tab_scroll: ScrollHandle::new(),
            exporting_db: false,
            _subscriptions: subs,
        }
    }

    // ------------------------------------------------------------ 标签页

    fn active_conn_id(&self, cx: &gpui_kit::App) -> Option<String> {
        Stores::global(cx).conns.read(cx).active_id.clone()
    }

    fn visible_tabs(&self, cx: &gpui_kit::App) -> Vec<Entity<WorkTab>> {
        let Some(active) = self.active_conn_id(cx) else {
            return vec![];
        };
        self.tabs
            .iter()
            .filter(|t| t.read(cx).conn_id == active)
            .cloned()
            .collect()
    }

    /// 当前 tab:该连接记住的 tab,记忆失效时回退到该连接最后一个 tab。
    pub fn active_tab(&self, cx: &gpui_kit::App) -> Option<Entity<WorkTab>> {
        let visible = self.visible_tabs(cx);
        let active = self.active_conn_id(cx)?;
        let remembered = self.active_by_conn.get(&active);
        visible
            .iter()
            .find(|t| Some(&t.entity_id()) == remembered)
            .cloned()
            .or_else(|| visible.last().cloned())
    }

    /// 顶栏/AI 用的"当前连接":当前 tab 的连接,否则侧栏聚焦的连接。
    fn head_conn(&self, cx: &gpui_kit::App) -> Option<datum_core::core::connection::Connection> {
        let store = Stores::global(cx).conns.read(cx);
        self.active_tab(cx)
            .and_then(|t| store.get(&t.read(cx).conn_id).cloned())
            .or_else(|| store.active().cloned())
    }

    fn focus_tab(&mut self, tab: &Entity<WorkTab>, cx: &mut Context<Self>) {
        let conn_id = tab.read(cx).conn_id.clone();
        self.active_by_conn.insert(conn_id, tab.entity_id());
        cx.notify();
    }

    fn add_tab(&mut self, tab: Entity<WorkTab>, window: &mut Window, cx: &mut Context<Self>) {
        let sub = cx.subscribe_in(&tab, window, |this, tab, event: &TabEvent, window, cx| {
            this.on_tab_event(tab, event, window, cx)
        });
        self._subscriptions.push(sub);
        self.tabs.push(tab.clone());
        self.focus_tab(&tab, cx);
        if let Some(ix) = self
            .visible_tabs(cx)
            .iter()
            .position(|t| t.entity_id() == tab.entity_id())
        {
            self.tab_scroll.scroll_to_item(ix);
        }
    }

    fn on_tab_event(
        &mut self,
        tab: &Entity<WorkTab>,
        event: &TabEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TabEvent::ShowDetail => {
                self.detail_open = true;
                cx.notify();
            }
            TabEvent::SchemaChanged => {
                let conn_id = tab.read(cx).conn_id.clone();
                let conns = Stores::global(cx).conns.clone();
                conns.update(cx, |s, cx| {
                    s.invalidate_erd(&conn_id);
                    if s.tables.contains_key(&conn_id) {
                        s.load_tables(&conn_id, cx).detach();
                    }
                });
            }
            TabEvent::SendToAi(sql) => self.send_to_ai(sql.clone(), cx),
            TabEvent::Toast(t) => show_toast(t, window, cx),
            TabEvent::Changed => cx.notify(),
        }
    }

    /// 点表/key:该连接下已有同表 tab 则聚焦,否则新开。
    pub fn open_table(
        &mut self,
        conn_id: &str,
        table: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<WorkTab>> {
        let conns = Stores::global(cx).conns.clone();
        conns.update(cx, |s, cx| s.activate(conn_id, cx));
        if let Some(existing) = self
            .tabs
            .iter()
            .find(|t| {
                let t = t.read(cx);
                t.kind == TabKind::Table
                    && t.conn_id == conn_id
                    && t.table.as_deref() == Some(table)
            })
            .cloned()
        {
            self.focus_tab(&existing, cx);
            return Some(existing);
        }
        let conn = conns.read(cx).get(conn_id).cloned()?;
        let tab = cx.new(|cx| WorkTab::new_table(&conn, table, window, cx));
        self.add_tab(tab.clone(), window, cx);
        Some(tab)
    }

    /// 集合右键「新建查询」:新开查询 tab 并预填 `db.<集合>.find({})`。
    pub fn new_mongo_query(
        &mut self,
        conn_id: &str,
        coll: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let conns = Stores::global(cx).conns.clone();
        conns.update(cx, |s, cx| s.activate(conn_id, cx));
        if let Some(tab) = self.new_query_tab(window, cx) {
            let sql = format!("{}.find({{}})", datum_core::mongo::parse::coll_ref(coll));
            tab.update(cx, |t, cx| t.set_sql(&sql, window, cx));
        }
    }

    /// 集合右键「插入文档」:打开(或聚焦)集合 tab 并弹出新增文档编辑器。
    pub fn insert_document(
        &mut self,
        conn_id: &str,
        coll: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.open_table(conn_id, coll, window, cx) {
            tab.update(cx, |t, cx| t.open_doc_editor(None, window, cx));
        }
    }

    /// 在当前连接下新开一个查询 tab;连接未建立时顺带建立并缓存表列表。
    pub fn new_query_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<WorkTab>> {
        let Some(conn) = self.head_conn(cx) else {
            show_toast(
                &Toast::Error(t!("workspace.select_conn_first").into()),
                window,
                cx,
            );
            return None;
        };
        let n = self
            .tabs
            .iter()
            .filter(|t| t.read(cx).kind == TabKind::Query && t.read(cx).conn_id == conn.id)
            .count();
        let base = if conn.kind == datum_core::core::connection::DbKind::Mongo {
            "Mongo Query"
        } else {
            "SQL Query"
        };
        let title: SharedString = if n == 0 {
            base.into()
        } else {
            format!("{base} {}", n + 1).into()
        };
        let tab = cx.new(|cx| WorkTab::new_query(&conn, title, window, cx));
        self.add_tab(tab.clone(), window, cx);
        let conns = Stores::global(cx).conns.clone();
        conns.update(cx, |s, cx| {
            s.activate(&conn.id, cx);
            if !s.connected.contains(&conn.id) {
                s.load_tables(&conn.id, cx).detach();
            }
        });
        if let Some(editor) = tab.read(cx).editor() {
            editor.read(cx).focus_handle(cx).focus(window, cx);
        }
        Some(tab)
    }

    pub fn close_tab(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pos) = self.tabs.iter().position(|t| t.entity_id() == id) else {
            return;
        };
        let conn_id = self.tabs[pos].read(cx).conn_id.clone();
        // 邻位在同连接的 tab 里找
        let siblings: Vec<EntityId> = self
            .tabs
            .iter()
            .filter(|t| t.read(cx).conn_id == conn_id)
            .map(|t| t.entity_id())
            .collect();
        let ix = siblings.iter().position(|s| *s == id).unwrap_or(0);
        let rest: Vec<EntityId> = siblings.into_iter().filter(|s| *s != id).collect();
        let neighbor = rest
            .get(ix)
            .or_else(|| ix.checked_sub(1).and_then(|i| rest.get(i)))
            .copied();
        self.tabs.remove(pos);
        if self.active_by_conn.get(&conn_id) == Some(&id) {
            match neighbor {
                Some(n) => {
                    self.active_by_conn.insert(conn_id, n);
                }
                None => {
                    self.active_by_conn.remove(&conn_id);
                }
            }
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn close_other_tabs(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let Some(keep) = self.tabs.iter().find(|t| t.entity_id() == id).cloned() else {
            return;
        };
        let conn_id = keep.read(cx).conn_id.clone();
        self.tabs
            .retain(|t| t.entity_id() == id || t.read(cx).conn_id != conn_id);
        self.focus_tab(&keep, cx);
    }

    fn start_rename(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter().find(|t| t.entity_id() == id).cloned() else {
            return;
        };
        let title = tab.read(cx).title.to_string();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    let title = input.read(cx).value().trim().to_string();
                    if !title.is_empty() {
                        tab.update(cx, |t, cx| {
                            t.title = title.into();
                            cx.notify();
                        });
                    }
                    this.renaming = None;
                    this.focus.focus(window, cx);
                    cx.notify();
                }
                _ => {}
            },
        );
        self._subscriptions.push(sub);
        input.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
        self.renaming = Some((id, input));
        cx.notify();
    }

    // ------------------------------------------------------------ 面板

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    fn toggle_detail(&mut self, cx: &mut Context<Self>) {
        if !self.detail_open {
            // 顶栏图标打开右栏时默认展示表详情:清掉遗留的行选中
            if let Some(tab) = self.active_tab(cx) {
                tab.update(cx, |t, cx| t.clear_selection(cx));
            }
        }
        self.detail_open = !self.detail_open;
        cx.notify();
    }

    fn toggle_ai(&mut self, cx: &mut Context<Self>) {
        self.ai_open = !self.ai_open;
        cx.notify();
    }

    /// 编辑器选中 SQL → AI 助手的引用卡片(交给用户确认后再发送)。
    fn send_to_ai(&mut self, sql: String, cx: &mut Context<Self>) {
        let ai = Stores::global(cx).ai.clone();
        ai.update(cx, |ai, cx| ai.set_pending_ref(Some(sql), cx));
        self.ai_open = true;
        cx.notify();
    }

    /// AI 生成的 SQL「编辑器运行」:只在对话绑定的连接上执行 —— 当前查询 tab 属于该连接则复用,
    /// 否则切到该连接并新开查询 tab,绝不打到别的库。
    fn insert_and_run(
        &mut self,
        sql: String,
        conn_id: &str,
        confirmed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let conns = Stores::global(cx).conns.clone();
        if conns.read(cx).get(conn_id).is_none() {
            show_toast(
                &Toast::Error(t!("workspace.chat_conn_gone").into()),
                window,
                cx,
            );
            return;
        }
        conns.update(cx, |s, cx| s.activate(conn_id, cx));
        let tab = match self
            .active_tab(cx)
            .filter(|t| t.read(cx).kind == TabKind::Query && t.read(cx).conn_id == conn_id)
        {
            Some(t) => t,
            None => match self.new_query_tab(window, cx) {
                Some(t) => t,
                None => return,
            },
        };
        if tab.read(cx).conn_id != conn_id {
            return;
        }
        self.focus_tab(&tab, cx);
        tab.update(cx, |t, cx| {
            t.set_sql(&sql, window, cx);
            // 未经确认的(静态检查判定为只读而免确认)在只读模式下执行:
            // 静态检查看不穿视图里的写函数等副作用,由数据库兜底拒绝写入
            t.read_only_next = !confirmed;
            // 仅当卡片已弹过破坏性确认时跳过审查;其余一律走审查门
            t.run(Some(sql.clone()), false, confirmed, window, cx);
        });
    }

    fn with_active_tab(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut WorkTab, &mut Context<WorkTab>),
    ) {
        if let Some(tab) = self.active_tab(cx) {
            tab.update(cx, f);
        }
    }

    // ------------------------------------------------------------ 动作

    fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        // 当前连接有 tab 则关它;全局无任何 tab 才关闭窗口
        if let Some(tab) = self.active_tab(cx) {
            self.close_tab(tab.entity_id(), window, cx);
        } else if self.tabs.is_empty() {
            #[cfg(target_os = "macos")]
            cx.hide();
            #[cfg(not(target_os = "macos"))]
            window.remove_window();
        }
    }

    fn on_refresh(&mut self, _: &Refresh, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_tab(cx) {
            tab.update(cx, |t, cx| t.refresh(window, cx));
        }
        if let Some(id) = self.active_conn_id(cx) {
            let conns = Stores::global(cx).conns.clone();
            conns.update(cx, |s, cx| {
                if s.tables.contains_key(&id) {
                    s.load_tables(&id, cx).detach();
                }
            });
        }
    }
}

pub fn show_toast(toast: &Toast, window: &mut Window, cx: &mut gpui_kit::App) {
    let note = match toast {
        Toast::Success(m) => Notification::success(m.clone()),
        Toast::Error(m) => Notification::error(m.clone()),
        Toast::Info(m) => Notification::info(m.clone()),
    };
    window.push_notification(note, cx);
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_weight = gpui_kit::FontWeight(crate::prefs::Prefs::global(cx).font_weight as f32);
        let center = v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(self.render_tab_strip(window, cx))
            .child(div().flex_1().min_h_0().child(match self.active_tab(cx) {
                Some(tab) => tab.into_any_element(),
                None => chrome::empty_workspace(cx).into_any_element(),
            }));

        v_flex()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_weight(font_weight)
            .on_action(cx.listener(|this, _: &NewQuery, window, cx| {
                this.new_query_tab(window, cx);
            }))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &ToggleDetail, _, cx| this.toggle_detail(cx)))
            .on_action(cx.listener(|this, _: &ToggleAi, _, cx| this.toggle_ai(cx)))
            .on_action(cx.listener(|this, _: &OpenErd, window, cx| this.open_erd(window, cx)))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(|this, _: &ToggleFilter, window, cx| {
                if let Some(tab) = this.active_tab(cx) {
                    tab.update(cx, |t, cx| t.toggle_filter(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &CommitEdits, window, cx| {
                if let Some(tab) = this.active_tab(cx) {
                    tab.update(cx, |t, cx| t.commit(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleHistory, _, cx| {
                this.with_active_tab(cx, |t, cx| {
                    if t.kind == TabKind::Query {
                        t.toolbar.history_open = !t.toolbar.history_open;
                        cx.notify();
                    }
                });
            }))
            .on_action(cx.listener(|this, _: &FormatSql, window, cx| {
                if let Some(tab) = this.active_tab(cx) {
                    tab.update(cx, |t, cx| t.beautify(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &ExplainQuery, window, cx| {
                if let Some(tab) = this.active_tab(cx) {
                    tab.update(cx, |t, cx| t.explain(window, cx));
                }
            }))
            .child(self.render_top_bar(window, cx))
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("workspace-cols")
                        .child(
                            resizable_panel()
                                .visible(self.sidebar_open)
                                .size(px(320.))
                                .size_range(px(260.)..px(520.))
                                // 固定宽度栏不随其他栏开合而伸缩,由中间结果区伸缩
                                .flex_none()
                                .child(self.sidebar.clone()),
                        )
                        .child(resizable_panel().child(center))
                        .child(
                            resizable_panel()
                                .visible(self.detail_open)
                                .size(px(320.))
                                .size_range(px(200.)..px(640.))
                                .flex_none()
                                .child(self.render_detail(cx)),
                        )
                        .child(
                            resizable_panel()
                                .visible(self.ai_open)
                                .size(px(400.))
                                .size_range(px(280.)..px(900.))
                                .flex_none()
                                .child(self.chat.clone()),
                        ),
                ),
            )
            .child(self.render_status_bar(cx))
    }
}

/// 让 palette 在 workspace 内可直接借用(渲染辅助用)。
#[allow(dead_code)]
fn palette(cx: &gpui_kit::App) -> crate::theme::Palette {
    *cx.palette()
}

#[cfg(feature = "snapshot")]
impl Workspace {
    /// 截图脚本的单步命令(仅开发用)。
    pub fn snapshot_step(
        &mut self,
        cmd: &str,
        arg: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match cmd {
            "conn" => {
                let conns = Stores::global(cx).conns.clone();
                conns.update(cx, |s, cx| {
                    s.activate(arg, cx);
                    s.load_tables(arg, cx).detach();
                });
            }
            "table" => {
                if let Some(id) = self.active_conn_id(cx) {
                    self.open_table(&id, arg, window, cx);
                }
            }
            "query" => {
                self.new_query_tab(window, cx);
            }
            "sql" => {
                if let Some(t) = self.active_tab(cx) {
                    t.update(cx, |t, cx| t.set_sql(&arg.replace("\\n", "\n"), window, cx));
                }
            }
            "run" => {
                if let Some(t) = self.active_tab(cx) {
                    t.update(cx, |t, cx| t.run(None, false, false, window, cx));
                }
            }
            "select" => {
                if let Some(t) = self.active_tab(cx) {
                    let ix = arg.parse().unwrap_or(0);
                    t.update(cx, |t, cx| t.open_row_detail(ix, window, cx));
                }
            }
            "cell" => {
                if let (Some(t), Some((r, c))) = (self.active_tab(cx), arg.split_once(',')) {
                    let (r, c) = (r.parse().unwrap_or(0), c.parse().unwrap_or(0));
                    t.update(cx, |t, cx| t.open_cell(r, c, window, cx));
                }
            }
            "filter" => {
                if let Some(t) = self.active_tab(cx) {
                    t.update(cx, |t, cx| t.toggle_filter(window, cx));
                }
            }
            "history" => self.with_active_tab(cx, |t, cx| {
                t.toolbar.history_open = true;
                cx.notify();
            }),
            "detail" => self.toggle_detail(cx),
            "ai" => self.toggle_ai(cx),
            // 延后到本次 Workspace 更新之外:发送时要读 Workspace(真实场景由面板事件触发)
            "ai-send" => {
                let (chat, text) = (self.chat.clone(), arg.to_string());
                window.defer(cx, move |window, cx| {
                    chat.update(cx, |c, cx| c.snapshot_send(&text, window, cx))
                });
            }
            "sidebar" => self.toggle_sidebar(cx),
            "theme" => {
                let pref = if arg == "light" {
                    crate::prefs::ThemePreference::Light
                } else {
                    crate::prefs::ThemePreference::Dark
                };
                theme::set_preference(pref, window, cx);
            }
            "settings" => self.open_settings(window, cx),
            "lang" => {
                let pref = match arg {
                    "en" => crate::prefs::LanguagePreference::En,
                    "zh-TW" => crate::prefs::LanguagePreference::ZhTw,
                    "zh-CN" => crate::prefs::LanguagePreference::ZhCn,
                    _ => crate::prefs::LanguagePreference::System,
                };
                crate::i18n::set_preference(pref, cx);
            }
            "update-check" => {
                let u = Stores::global(cx).update.clone();
                u.update(cx, |u, cx| u.check(cx));
            }
            "update-install" => {
                let u = Stores::global(cx).update.clone();
                u.update(cx, |u, cx| u.install(cx));
            }
            "newconn" => self.open_connection_form(None, window, cx),
            // mfilter:<文本>:写入 Mongo 过滤栏的 Filter 并应用
            "mfilter" => {
                if let Some(t) = self.active_tab(cx) {
                    t.update(cx, |t, cx| t.snapshot_mongo_filter(arg, window, cx));
                }
            }
            // mproj:<文本>:写入 Projection 并应用
            "mproj" => {
                if let Some(t) = self.active_tab(cx) {
                    t.update(cx, |t, cx| t.snapshot_mongo_projection(arg, window, cx));
                }
            }
            // msort:<列名>:模拟点表头排序
            "msort" => {
                if let Some(t) = self.active_tab(cx) {
                    t.update(cx, |t, cx| t.cycle_sort(arg, window, cx));
                }
            }
            // medit:<视图行>:打开文档编辑器(负数 = 新增)
            "medit" => {
                if let Some(t) = self.active_tab(cx) {
                    let ix: i64 = arg.parse().unwrap_or(-1);
                    t.update(cx, |t, cx| {
                        t.open_doc_editor((ix >= 0).then_some(ix as usize), window, cx)
                    });
                }
            }
            "mjson" => self.with_active_tab(cx, |t, cx| {
                if let Some(m) = t.mongo.as_mut() {
                    m.json_view = !m.json_view;
                }
                cx.notify();
            }),
            "mpage" => {
                if let Some(t) = self.active_tab(cx) {
                    let p = arg.parse().unwrap_or(0);
                    t.update(cx, |t, cx| t.mongo_goto_page(p, window, cx));
                }
            }
            // editconn:<连接 id>:打开该连接的编辑表单
            "editconn" => {
                let conn = Stores::global(cx).conns.read(cx).get(arg).cloned();
                if let Some(c) = conn {
                    self.open_connection_form(Some(c), window, cx);
                }
            }
            "import" => self.open_import(None, window, cx),
            "erd" => self.open_erd(window, cx),
            "ddl" => {
                if let Some(id) = self.active_conn_id(cx) {
                    self.show_ddl(&id, arg, window, cx);
                }
            }
            "close-dialog" => window.close_all_dialogs(cx),
            "erd-pan" => crate::erd::bench_pan(cx),
            "erd-view" => crate::erd::bench_view(arg, cx),
            "focus-editor" => {
                if let Some(e) = self
                    .active_tab(cx)
                    .and_then(|t| t.read(cx).editor().cloned())
                {
                    e.update(cx, |e, cx| e.focus(window, cx));
                }
            }
            "selectall" => {
                if let Some(e) = self
                    .active_tab(cx)
                    .and_then(|t| t.read(cx).editor().cloned())
                {
                    e.update(cx, |e, cx| {
                        e.focus(window, cx);
                        e.select_all(window, cx)
                    });
                }
            }
            "limit" => self.with_active_tab(cx, |t, cx| {
                t.toolbar.limit_open = true;
                cx.notify();
            }),
            _ => eprintln!("unknown snapshot step: {cmd}"),
        }
        cx.notify();
    }
}
