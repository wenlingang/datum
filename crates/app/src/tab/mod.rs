//! 工作标签页:绑定一个连接,持有各自的 SQL、结果、分页、筛选与暂存编辑。
//!
//! 两种形态:
//! - 表浏览(`TabKind::Table`):纯网格,服务端分页与排序,可暂存编辑/删除后 ⌘S 提交;
//! - 查询(`TabKind::Query`):编辑器 + 工具条 + 结果,运行光标所在语句或选区。

mod cell_dialog;
pub mod editor;
mod filter;
mod grid;
mod history;
mod selection_chip;
mod toolbar;

use crate::actions::{CommitEdits, ExplainQuery, FormatSql, RunQuery, SendSelectionToAi};
use crate::dialogs::risk::{RiskGate, confirm_destructive};
use crate::prefs::Prefs;
use crate::runtime;
use crate::store::{Stores, Toast};
use crate::theme::ActivePalette as _;
use crate::ui;
use datum_core::completion::uppercase_keyword_before;
use datum_core::core::connection::{Connection, DbKind};
use datum_core::core::history::HistoryEntry;
use datum_core::error::AppResult;
use datum_core::grid::{self as grid_logic, Cell};
use datum_core::pg::browse::TableDetail;
use datum_core::pg::client::QueryResult;
use datum_core::pg::edit::{CellEdit, RowDelete};
use datum_core::rds::client::KeyDetail;
use datum_core::service;
use datum_core::sql::build::{build_insert_row, guess_table_name, page_sql};
use datum_core::sql::risk::{analyze_redis, analyze_sql};
use datum_core::sql::stmt::{current_statement, is_query};
use datum_core::sql::{Dialect, format::format_sql};
use editor::{SchemaHandle, new_editor};
use filter::FilterState;
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, AsyncWindowContext, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, div, px,
};
pub use grid::GridData;
use grid::ResultDelegate;
use rust_i18n::t;
use std::cell::Cell as StdCell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 表浏览每页行数。
pub const PAGE_SIZE: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    /// 表浏览(纯网格)
    Table,
    /// SQL 查询工作区(编辑器 + 结果)
    Query,
}

/// 标签页向工作区报告的事件。
pub enum TabEvent {
    /// 请求在右栏显示行详情/表详情
    ShowDetail,
    /// 结构可能变化(DDL / 导入):刷新表列表并失效 ER 缓存
    SchemaChanged,
    /// 把选中的 SQL 作为引用发给 AI 助手
    SendToAi(String),
    Toast(Toast),
    /// 标题/结果/状态变化,工作区需要重绘标签条与状态栏
    Changed,
}

pub struct WorkTab {
    /// 下一次执行在只读模式下进行(AI 生成、免确认执行的语句),由数据库强制拒绝写入
    pub read_only_next: bool,
    pub kind: TabKind,
    pub conn_id: String,
    pub db_kind: DbKind,
    pub title: SharedString,
    /// 表浏览的表名(Redis 为 key 名)
    pub table: Option<String>,
    focus: FocusHandle,
    editor: Option<Entity<EditorState>>,
    chip: Option<Entity<selection_chip::SelectionChip>>,
    schema: Option<SchemaHandle>,
    /// 表浏览当前页的 SQL
    browse_sql: String,
    pub result: Option<Arc<QueryResult>>,
    pub detail: Option<TableDetail>,
    pub redis_detail: Option<KeyDetail>,
    pub page: usize,
    /// 非空表示处于分页浏览(仅关系库表 tab)
    pub browse_table: Option<String>,
    /// 暂存的编辑:(主键值, 列名) → 编辑
    dirty: BTreeMap<(String, String), CellEdit>,
    /// 暂存删除的行(主键值)
    deletes: Vec<String>,
    pub err: Option<String>,
    /// 视图中选中的行(右栏行详情)
    pub selected_row: Option<usize>,
    /// 当前结果来自查询(SELECT)还是非查询语句(DDL/DML)
    pub result_is_query: bool,
    /// 排序:(列名, 是否倒序)
    sort: Option<(String, bool)>,
    pub elapsed_ms: Option<u128>,
    pub filter: FilterState,
    /// 当前视图行(筛选后)在 result.rows 中的下标
    pub view: Arc<Vec<usize>>,
    grid: Entity<TableState<ResultDelegate>>,
    pub running: bool,
    pub toolbar: toolbar::ToolbarState,
    /// 结果区实测宽度(列宽铺满用);`fitted_width` 是最近一次排布列宽时用的宽度
    measured_width: Rc<StdCell<f32>>,
    fitted_width: f32,
    _refit: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    _task: Option<Task<()>>,
}

impl EventEmitter<TabEvent> for WorkTab {}

impl Focusable for WorkTab {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl WorkTab {
    fn base(
        kind: TabKind,
        conn: &Connection,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let weak = cx.entity().downgrade();
        let grid = cx.new(|cx| {
            TableState::new(ResultDelegate::new(weak), window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .sortable(false)
                .col_movable(false)
        });
        let mut subs =
            vec![
                cx.subscribe_in(&grid, window, |this, _, event: &TableEvent, _, cx| {
                    if let TableEvent::SelectRow(ix) = event {
                        this.selected_row = Some(*ix);
                        cx.emit(TabEvent::Changed);
                    }
                }),
            ];
        let (editor, schema) = if kind == TabKind::Query {
            let (editor, schema) = new_editor(conn.kind, window, cx);
            subs.push(cx.subscribe_in(
                &editor,
                window,
                |this, editor, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.uppercase_keyword(editor, window, cx);
                    }
                },
            ));
            (Some(editor), Some(schema))
        } else {
            (None, None)
        };
        let toolbar = toolbar::ToolbarState::new(window, cx);
        let chip = editor
            .clone()
            .map(|e| cx.new(|cx| selection_chip::SelectionChip::new(e, cx)));
        // schema 到达后刷新补全上下文
        let stores = Stores::global(cx).conns.clone();
        subs.push(cx.observe(&stores, |this, _, cx| this.sync_schema(cx)));
        let mut this = Self {
            kind,
            conn_id: conn.id.clone(),
            db_kind: conn.kind,
            title,
            table: None,
            focus: cx.focus_handle(),
            editor,
            chip,
            schema,
            browse_sql: String::new(),
            result: None,
            detail: None,
            redis_detail: None,
            page: 0,
            browse_table: None,
            dirty: BTreeMap::new(),
            deletes: vec![],
            err: None,
            selected_row: None,
            result_is_query: true,
            sort: None,
            elapsed_ms: None,
            filter: FilterState::default(),
            view: Arc::new(vec![]),
            grid,
            running: false,
            read_only_next: false,
            toolbar,
            measured_width: Rc::new(StdCell::new(0.)),
            fitted_width: 0.,
            _refit: None,
            _subscriptions: subs,
            _task: None,
        };
        this.sync_schema(cx);
        this
    }

    /// 新建查询 tab。
    pub fn new_query(
        conn: &Connection,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::base(TabKind::Query, conn, title, window, cx)
    }

    /// 新建表浏览 tab 并加载首页(关系库)或 key 值(Redis)。
    pub fn new_table(
        conn: &Connection,
        table: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::base(TabKind::Table, conn, table.to_string().into(), window, cx);
        this.table = Some(table.to_string());
        if conn.kind.is_relational() {
            this.browse_table = Some(table.to_string());
        }
        this.refresh(window, cx);
        this
    }

    pub fn dialect(&self) -> Option<Dialect> {
        Dialect::from_kind(&self.db_kind)
    }

    fn connection(&self, cx: &gpui_kit::App) -> Option<Connection> {
        Stores::global(cx)
            .conns
            .read(cx)
            .get(&self.conn_id)
            .cloned()
    }

    fn max_rows(cx: &gpui_kit::App) -> u32 {
        Prefs::global(cx).max_rows
    }

    fn sync_schema(&mut self, cx: &mut Context<Self>) {
        let Some(schema) = &self.schema else { return };
        let store = Stores::global(cx).conns.read(cx);
        let mut s = schema.borrow_mut();
        s.tables = store.tables.get(&self.conn_id).cloned().unwrap_or_default();
        s.columns = store
            .columns
            .get(&self.conn_id)
            .cloned()
            .unwrap_or_default();
    }

    /// 输入边界字符时,把紧邻前面的 SQL 关键字转为大写(长度不变,光标不跳)。
    fn uppercase_keyword(
        &mut self,
        editor: &Entity<EditorState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.db_kind == DbKind::Redis {
            return;
        }
        let (text, cursor, sel) = {
            let e = editor.read(cx);
            (e.value(), e.cursor(), e.selected_range())
        };
        if !sel.is_empty() {
            return;
        }
        let Some((range, upper)) = uppercase_keyword_before(&text, cursor) else {
            return;
        };
        editor.update(cx, |e, cx| {
            e.set_selected_range(range, cx);
            e.replace(upper, window, cx);
            e.set_selected_range(cursor..cursor, cx);
        });
    }

    // ------------------------------------------------------------ 编辑器文本

    pub fn editor(&self) -> Option<&Entity<EditorState>> {
        self.editor.as_ref()
    }

    pub fn set_sql(&mut self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = &self.editor {
            editor.update(cx, |e, cx| e.set_value(sql.to_string(), window, cx));
        }
    }

    pub fn sql_text(&self, cx: &gpui_kit::App) -> String {
        self.editor
            .as_ref()
            .map(|e| e.read(cx).value().to_string())
            .unwrap_or_else(|| self.browse_sql.clone())
    }

    /// 编辑器选区文本(无选区为空串)。
    pub fn selection_text(&self, cx: &gpui_kit::App) -> String {
        self.editor
            .as_ref()
            .map(|e| e.read(cx).selected_text().to_string())
            .unwrap_or_default()
    }

    /// 运行时取:有选区跑选区,否则跑光标所在的那条语句。
    fn sql_to_run(&self, cx: &gpui_kit::App) -> String {
        let Some(editor) = &self.editor else {
            return self.browse_sql.clone();
        };
        let e = editor.read(cx);
        let sel = e.selected_text().to_string();
        if !sel.trim().is_empty() {
            return sel.trim().to_string();
        }
        if self.db_kind == DbKind::Redis {
            // Redis 一行一条命令:跑光标所在行
            let text = e.value().to_string();
            let cursor = e.cursor().min(text.len());
            let start = text[..cursor].rfind('\n').map_or(0, |i| i + 1);
            let end = text[cursor..].find('\n').map_or(text.len(), |i| cursor + i);
            return text[start..end].trim().to_string();
        }
        current_statement(&e.value(), e.cursor())
    }

    // ------------------------------------------------------------ 结果与视图

    fn set_result(&mut self, result: QueryResult, cx: &mut Context<Self>) {
        self.result = Some(Arc::new(result));
        self.selected_row = None;
        self.recompute_view(cx);
    }

    /// 按已应用的筛选重算视图行,并把数据快照推给网格。
    pub fn recompute_view(&mut self, cx: &mut Context<Self>) {
        let Some(result) = self.result.clone() else {
            self.view = Arc::new(vec![]);
            cx.emit(TabEvent::Changed);
            cx.notify();
            return;
        };
        self.view = Arc::new(grid_logic::filter_indices(
            &result.rows,
            &result.columns,
            &self.filter.applied,
            self.filter.applied_conj,
        ));
        self.push_grid(cx);
        cx.emit(TabEvent::Changed);
        cx.notify();
    }

    fn pk_col(&self) -> Option<String> {
        self.detail
            .as_ref()?
            .columns
            .iter()
            .find(|c| c.is_pk)
            .map(|c| c.name.clone())
    }

    fn push_grid(&mut self, cx: &mut Context<Self>) {
        let Some(result) = self.result.clone() else {
            return;
        };
        let pk_col = self.pk_col();
        let pk_index = pk_col
            .as_ref()
            .and_then(|pk| result.columns.iter().position(|c| c == pk));
        let data = GridData {
            columns: result.columns.clone(),
            rows: Arc::new(result.rows.clone()),
            view: self.view.clone(),
            types: self
                .detail
                .as_ref()
                .map(|d| {
                    d.columns
                        .iter()
                        .map(|c| (c.name.clone(), c.data_type.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            pk_index,
            dirty: self.dirty.keys().cloned().collect(),
            deleted: self.deletes.iter().cloned().collect::<HashSet<_>>(),
            sort: self.sort.clone(),
            deletable: self.table.is_some() && pk_index.is_some(),
        };
        let width = self.fitted_width;
        self.grid.update(cx, |grid, cx| {
            grid.delegate_mut().set_data(data, width);
            grid.refresh(cx);
        });
    }

    /// 视图中第 `view_ix` 行的原始数据。
    pub fn view_row(&self, view_ix: usize) -> Option<&Vec<Cell>> {
        let result = self.result.as_ref()?;
        self.view.get(view_ix).and_then(|ri| result.rows.get(*ri))
    }

    pub fn column_types(&self) -> std::collections::HashMap<String, String> {
        self.detail
            .as_ref()
            .map(|d| {
                d.columns
                    .iter()
                    .map(|c| (c.name.clone(), c.data_type.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    // ------------------------------------------------------------ 执行

    fn spawn_query(
        &mut self,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_ok: impl FnOnce(&mut Self, QueryResult, u128, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let (kind, id, max_rows) = (self.db_kind, self.conn_id.clone(), Self::max_rows(cx));
        // 一次性标记:只作用于紧接着的这一次执行
        let read_only = std::mem::take(&mut self.read_only_next);
        self.running = true;
        cx.notify();
        let started = Instant::now();
        self._task = Some(
            cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
                let result = runtime::backend(move |s| async move {
                    if read_only {
                        service::query_read_only(&s, &kind, &id, &sql, Some(max_rows)).await
                    } else {
                        service::query(&s, &kind, &id, &sql, Some(max_rows)).await
                    }
                })
                .await;
                let elapsed = started.elapsed().as_millis();
                let _ = this.update_in(cx, |this, window, cx| {
                    this.running = false;
                    match result {
                        Ok(r) => {
                            this.err = None;
                            on_ok(this, r, elapsed, window, cx);
                        }
                        Err(e) => {
                            this.err = Some(e.display());
                            this.result = None;
                            this.recompute_view(cx);
                        }
                    }
                    cx.notify();
                });
            }),
        );
    }

    /// 运行编辑器里的 SQL(或 Redis 命令)。`override_sql` 用于「插入并运行」「再次执行历史」。
    pub fn run(
        &mut self,
        override_sql: Option<String>,
        skip_history: bool,
        skip_risk: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sql = override_sql.unwrap_or_else(|| self.sql_to_run(cx));
        if sql.trim().is_empty() {
            self.err = Some(if self.db_kind == DbKind::Redis {
                t!("tab.empty_command").to_string().into()
            } else {
                t!("tab.empty_sql").to_string().into()
            });
            cx.notify();
            return;
        }
        if !skip_risk {
            let findings = if self.db_kind == DbKind::Redis {
                analyze_redis(&sql)
            } else {
                analyze_sql(&sql)
            };
            if !findings.is_empty() {
                // 转为由用户在确认框里明确决定:不再沿用免确认的只读标记,
                // 也避免取消后标记残留到下一次手动执行
                self.read_only_next = false;
                let conn = self.connection(cx);
                let this = cx.entity().downgrade();
                let sql_again = sql.clone();
                confirm_destructive(
                    RiskGate {
                        findings,
                        target_label: conn.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
                        database: conn
                            .as_ref()
                            .map(|c| {
                                if c.kind == DbKind::Sqlite {
                                    c.file_path.clone().unwrap_or_default()
                                } else {
                                    c.database.clone()
                                }
                            })
                            .filter(|d| !d.is_empty()),
                        dialect: self.dialect(),
                    },
                    move |window, cx| {
                        let sql = sql_again.clone();
                        let _ = this.update(cx, |this, cx| {
                            this.run(Some(sql), skip_history, true, window, cx)
                        });
                    },
                    window,
                    cx,
                );
                return;
            }
        }
        if self.db_kind == DbKind::Redis {
            self.run_redis(sql, skip_history, window, cx);
            return;
        }
        let query = is_query(&sql);
        let sql_for_history = sql.clone();
        self.spawn_query(sql, window, cx, move |this, r, elapsed, _, cx| {
            let (rows, affected) = (r.rows.len(), r.affected);
            this.browse_table = None;
            this.page = 0;
            this.result_is_query = query;
            this.sort = None;
            this.filter.reset_all();
            this.elapsed_ms = Some(elapsed);
            this.set_result(r, cx);
            // 非查询语句可能改变表结构:刷新表列表、失效 ER 缓存
            if !query {
                cx.emit(TabEvent::SchemaChanged);
            }
            if !skip_history {
                this.record_history(
                    &sql_for_history,
                    elapsed,
                    query.then_some(rows as u64),
                    if query { None } else { affected },
                    cx,
                );
            }
        });
    }

    fn run_redis(
        &mut self,
        cmd: String,
        skip_history: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.conn_id.clone();
        self.running = true;
        cx.notify();
        let started = Instant::now();
        let cmd_for_history = cmd.clone();
        self._task = Some(
            cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
                let result =
                    runtime::backend(
                        move |s| async move { service::redis_exec(&s, &id, &cmd).await },
                    )
                    .await;
                let elapsed = started.elapsed().as_millis();
                let _ = this.update(cx, |this, cx| {
                    this.running = false;
                    match result {
                        Ok(reply) => {
                            this.err = None;
                            this.elapsed_ms = Some(elapsed);
                            let rows = if reply.is_empty() {
                                vec![]
                            } else {
                                reply.lines().map(|l| vec![Some(l.to_string())]).collect()
                            };
                            this.set_result(
                                QueryResult {
                                    columns: vec![t!("tab.redis_result_column").to_string().into()],
                                    rows,
                                    affected: None,
                                },
                                cx,
                            );
                            if !skip_history {
                                this.record_history(&cmd_for_history, elapsed, None, None, cx);
                            }
                        }
                        Err(e) => {
                            this.err = Some(e.display());
                            this.result = None;
                            this.recompute_view(cx);
                        }
                    }
                });
            }),
        );
    }

    /// 执行成功才记历史(失败 / EXPLAIN / 翻页都不记)。
    fn record_history(
        &self,
        sql: &str,
        elapsed: u128,
        row_count: Option<u64>,
        affected: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let conn_name = self.connection(cx).map(|c| c.name).unwrap_or_default();
        let entry = HistoryEntry {
            id: String::new(),
            sql: sql.to_string(),
            conn_id: self.conn_id.clone(),
            kind: match self.db_kind {
                DbKind::Pg => "pg",
                DbKind::Mysql => "mysql",
                DbKind::Sqlite => "sqlite",
                DbKind::Redis => "redis",
            }
            .into(),
            conn_name,
            executed_at: chrono::Utc::now().timestamp_millis(),
            elapsed_ms: elapsed as u64,
            row_count,
            affected,
        };
        let history = Stores::global(cx).history.clone();
        history.update(cx, |h, cx| h.record(entry, cx));
    }

    /// EXPLAIN 当前/选中的 SQL(纯 EXPLAIN 不执行查询)。
    pub fn explain(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.db_kind == DbKind::Redis || self.kind != TabKind::Query {
            return;
        }
        let sql = self.sql_to_run(cx);
        if sql.trim().is_empty() {
            self.err = Some(t!("tab.empty_sql").to_string().into());
            cx.notify();
            return;
        }
        self.spawn_query(
            format!("EXPLAIN {sql}"),
            window,
            cx,
            |this, r, elapsed, _, cx| {
                this.browse_table = None;
                this.page = 0;
                this.result_is_query = true;
                this.sort = None;
                this.filter.reset_all();
                this.elapsed_ms = Some(elapsed);
                this.set_result(r, cx);
            },
        );
    }

    /// 格式化整篇 SQL(保留撤销历史)。
    pub fn beautify(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.db_kind == DbKind::Redis {
            return;
        }
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let source = editor.read(cx).value().to_string();
        let formatted = format_sql(&source);
        if formatted != source {
            editor.update(cx, |e, cx| e.replace_all(formatted, window, cx));
        }
    }

    /// 跳页(仅表浏览)。
    pub fn goto_page(&mut self, page: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(table), Some(d)) = (self.browse_table.clone(), self.dialect()) else {
            return;
        };
        let sort = self.sort.clone();
        let sql = page_sql(
            &table,
            page,
            PAGE_SIZE,
            d,
            sort.as_ref().map(|(c, desc)| (c.as_str(), *desc)),
        );
        self.browse_sql = sql.clone();
        self.spawn_query(sql, window, cx, move |this, r, elapsed, _, cx| {
            this.page = page;
            this.result_is_query = true;
            this.elapsed_ms = Some(elapsed);
            this.set_result(r, cx);
        });
    }

    pub fn can_prev(&self) -> bool {
        self.browse_table.is_some() && self.page > 0
    }

    pub fn can_next(&self) -> bool {
        self.browse_table.is_some()
            && self
                .result
                .as_ref()
                .is_some_and(|r| r.rows.len() == PAGE_SIZE)
    }

    /// 点击表头:浏览表时循环 倒序 → 升序 → 无 并重查第一页;其余结果做客户端排序(升 ↔ 降)。
    pub fn cycle_sort(&mut self, col: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.browse_table.is_some() {
            let next = match &self.sort {
                Some((c, true)) if c == col => Some((col.to_string(), false)),
                Some((c, false)) if c == col => None,
                _ => Some((col.to_string(), true)),
            };
            self.sort = next;
            self.goto_page(0, window, cx);
            return;
        }
        let Some(result) = self.result.clone() else {
            return;
        };
        let Some(ci) = result.columns.iter().position(|c| c == col) else {
            return;
        };
        let desc = matches!(&self.sort, Some((c, false)) if c == col);
        let rows = grid_logic::sort_rows(&result.rows, ci, desc);
        self.sort = Some((col.to_string(), desc));
        self.set_result(
            QueryResult {
                columns: result.columns.clone(),
                rows,
                affected: result.affected,
            },
            cx,
        );
    }

    /// 刷新当前 tab:表 tab 重拉结构与当前页;查询 tab 只重跑查询类语句(写语句不隐式重放)。
    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind == TabKind::Query {
            let sql = self.sql_to_run(cx);
            if sql.trim().is_empty() {
                cx.emit(TabEvent::Toast(Toast::Info(
                    t!("tab.refreshed_tables").to_string().into(),
                )));
            } else if self.db_kind != DbKind::Redis && !is_query(&sql) {
                cx.emit(TabEvent::Toast(Toast::Info(
                    t!("tab.refresh_skip_write").to_string().into(),
                )));
            } else {
                cx.emit(TabEvent::Toast(Toast::Info(
                    t!("tab.rerunning").to_string().into(),
                )));
                self.run(None, false, false, window, cx);
            }
            return;
        }
        let Some(table) = self.table.clone() else {
            return;
        };
        let (kind, id) = (self.db_kind, self.conn_id.clone());
        if kind == DbKind::Redis {
            self.running = true;
            self._task = Some(cx.spawn_in(
                window,
                async move |this, cx: &mut AsyncWindowContext| {
                    let result = runtime::backend(move |s| async move {
                        service::redis_get_key(&s, &id, &table).await
                    })
                    .await;
                    let _ = this.update(cx, |this, cx| {
                        this.running = false;
                        match result {
                            Ok((kv, detail)) => {
                                this.err = None;
                                this.redis_detail = Some(detail);
                                this.result_is_query = true;
                                let rows = kv
                                    .rows
                                    .into_iter()
                                    .map(|r| r.into_iter().map(Some).collect())
                                    .collect();
                                this.set_result(
                                    QueryResult {
                                        columns: kv.columns,
                                        rows,
                                        affected: None,
                                    },
                                    cx,
                                );
                            }
                            Err(e) => this.err = Some(e.display()),
                        }
                        cx.notify();
                    });
                },
            ));
            return;
        }
        let Some(d) = self.dialect() else { return };
        let sql = page_sql(
            &table,
            self.page,
            PAGE_SIZE,
            d,
            self.sort.as_ref().map(|(c, desc)| (c.as_str(), *desc)),
        );
        self.browse_sql = sql.clone();
        let max_rows = Self::max_rows(cx);
        self.running = true;
        cx.notify();
        let started = Instant::now();
        self._task = Some(
            cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
                let result: AppResult<(QueryResult, TableDetail)> =
                    runtime::backend(move |s| async move {
                        let r = service::query(&s, &kind, &id, &sql, Some(max_rows)).await?;
                        let detail = service::table_detail(&s, &kind, &id, &table).await?;
                        Ok((r, detail))
                    })
                    .await;
                let elapsed = started.elapsed().as_millis();
                let _ = this.update(cx, |this, cx| {
                    this.running = false;
                    match result {
                        Ok((r, detail)) => {
                            this.err = None;
                            this.detail = Some(detail);
                            this.result_is_query = true;
                            this.elapsed_ms = Some(elapsed);
                            this.set_result(r, cx);
                        }
                        Err(e) => {
                            this.err = Some(e.display());
                            cx.notify();
                        }
                    }
                });
            }),
        );
    }

    // ------------------------------------------------------------ 编辑与删除

    /// 暂存单元格修改(视图行 + 列)。
    pub fn stage_edit(
        &mut self,
        view_ix: usize,
        col_ix: usize,
        new_value: String,
        cx: &mut Context<Self>,
    ) {
        let (Some(pk_col), Some(table), Some(result)) =
            (self.pk_col(), self.table.clone(), self.result.clone())
        else {
            return;
        };
        let Some(pk_ix) = result.columns.iter().position(|c| *c == pk_col) else {
            return;
        };
        let Some(row) = self.view_row(view_ix) else {
            return;
        };
        let pk_value = row.get(pk_ix).cloned().flatten().unwrap_or_default();
        let column = result.columns[col_ix].clone();
        self.dirty.insert(
            (pk_value.clone(), column.clone()),
            CellEdit {
                table,
                pk_col,
                pk_value,
                column,
                new_value,
            },
        );
        self.push_grid(cx);
        cx.emit(TabEvent::Changed);
        cx.notify();
    }

    /// 右键「删除行 / 撤销删除」:暂存/取消暂存该行的删除。
    pub fn toggle_delete(&mut self, view_ix: usize, cx: &mut Context<Self>) {
        let (Some(pk_col), Some(result)) = (self.pk_col(), self.result.clone()) else {
            return;
        };
        let Some(pk_ix) = result.columns.iter().position(|c| *c == pk_col) else {
            return;
        };
        let Some(pk) = self
            .view_row(view_ix)
            .and_then(|r| r.get(pk_ix).cloned().flatten())
        else {
            return;
        };
        if let Some(pos) = self.deletes.iter().position(|d| *d == pk) {
            self.deletes.remove(pos);
        } else {
            self.deletes.push(pk);
        }
        self.push_grid(cx);
        cx.emit(TabEvent::Changed);
        cx.notify();
    }

    /// ⌘S:一个事务里提交暂存的编辑与删除,成功后清空暂存并重查当前页。
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(table) = self.table.clone() else {
            return;
        };
        let edits: Vec<CellEdit> = self.dirty.values().cloned().collect();
        let deletes: Vec<RowDelete> = match self.pk_col() {
            Some(pk_col) => self
                .deletes
                .iter()
                .map(|pk| RowDelete {
                    table: table.clone(),
                    pk_col: pk_col.clone(),
                    pk_value: pk.clone(),
                })
                .collect(),
            None => vec![],
        };
        if edits.is_empty() && deletes.is_empty() {
            return;
        }
        let (kind, id) = (self.db_kind, self.conn_id.clone());
        let n_deletes = deletes.len();
        self.running = true;
        cx.notify();
        self._task = Some(
            cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
                let result = runtime::backend(move |s| async move {
                    service::commit_edits(&s, &kind, &id, edits, deletes).await
                })
                .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.running = false;
                    match result {
                        Ok(_) => {
                            this.dirty.clear();
                            this.deletes.clear();
                            if n_deletes > 0 {
                                cx.emit(TabEvent::Toast(Toast::Success(
                                    t!("tab.deleted_rows", n = n_deletes).to_string().into(),
                                )));
                            }
                            this.refresh(window, cx);
                        }
                        Err(e) => {
                            this.err = Some(e.display());
                            cx.notify();
                        }
                    }
                });
            }),
        );
    }

    // ------------------------------------------------------------ 行操作

    /// 双击 / 右键「编辑 | 查看」:弹出单元格全文。
    pub fn open_cell(
        &mut self,
        view_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(result) = self.result.clone() else {
            return;
        };
        let Some(row) = self.view_row(view_ix) else {
            return;
        };
        let value = row.get(col_ix).cloned().flatten();
        let column = result.columns.get(col_ix).cloned().unwrap_or_default();
        let data_type = self.column_types().get(&column).cloned();
        let editable = self.pk_col().is_some() && self.table.is_some();
        cell_dialog::open(
            cx.entity().downgrade(),
            view_ix,
            col_ix,
            column,
            data_type,
            value,
            editable,
            window,
            cx,
        );
    }

    /// 右键「查看详情」:选中该行并打开右栏行详情。
    pub fn open_row_detail(&mut self, view_ix: usize, _: &mut Window, cx: &mut Context<Self>) {
        self.selected_row = Some(view_ix);
        cx.emit(TabEvent::ShowDetail);
        cx.notify();
    }

    /// 复制结果某一行的 INSERT(真实值)。
    pub fn copy_insert_row(&mut self, view_ix: usize, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(result), Some(d)) = (self.result.clone(), self.dialect()) else {
            return;
        };
        let Some(row) = self.view_row(view_ix) else {
            return;
        };
        let table = self.table.clone().unwrap_or_else(|| "table".into());
        cx.write_to_clipboard(ClipboardItem::new_string(build_insert_row(
            &table,
            &result.columns,
            row,
            d,
        )));
        cx.emit(TabEvent::Toast(Toast::Success(
            t!("tab.copied").to_string().into(),
        )));
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected_row.take().is_some() {
            self.grid.update(cx, |g, cx| g.clear_selection(cx));
            cx.notify();
        }
    }

    /// 导出用的表名:真实表名优先,查询 tab 从 SQL 文本里猜。
    pub fn export_name(&self, cx: &gpui_kit::App) -> String {
        self.table
            .clone()
            .or_else(|| guess_table_name(&self.sql_text(cx)))
            .unwrap_or_else(|| "export".into())
    }

    /// 当前视图行(导出)。
    pub fn view_rows(&self) -> Vec<&Vec<Cell>> {
        let Some(result) = &self.result else {
            return vec![];
        };
        self.view
            .iter()
            .filter_map(|ri| result.rows.get(*ri))
            .collect()
    }

    // ------------------------------------------------------------ 筛选

    pub fn toggle_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(result) = self.result.clone() else {
            return;
        };
        if self.filter.open {
            self.filter.open = false;
            self.filter.applied.clear();
            self.selected_row = None;
            self.recompute_view(cx);
        } else {
            self.filter.open = true;
            if self.filter.drafts.is_empty() {
                let col = result.columns.first().cloned().unwrap_or_default();
                self.filter.add(col, window, cx);
            }
            self.selected_row = None;
            cx.notify();
        }
        cx.emit(TabEvent::Changed);
    }

    pub fn filter_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let col = self
            .result
            .as_ref()
            .and_then(|r| r.columns.first().cloned())
            .unwrap_or_default();
        self.filter.add(col, window, cx);
        cx.notify();
    }

    pub fn filter_apply(&mut self, cx: &mut Context<Self>) {
        if !self.filter.is_dirty(cx) {
            return;
        }
        self.filter.apply(cx);
        self.selected_row = None;
        self.recompute_view(cx);
    }

    pub fn filter_clear(&mut self, cx: &mut Context<Self>) {
        self.filter.reset_all();
        self.selected_row = None;
        self.recompute_view(cx);
    }

    pub fn filter_preview_count(&self, cx: &gpui_kit::App) -> usize {
        let Some(result) = &self.result else { return 0 };
        let drafts = self.filter.draft_rows(cx);
        grid_logic::filter_indices(&result.rows, &result.columns, &drafts, self.filter.conj).len()
    }

    pub fn applied_filter_count(&self) -> usize {
        self.filter.applied.iter().filter(|f| f.is_active()).count()
    }

    // ------------------------------------------------------------ 渲染

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        toolbar::render(self, cx)
    }

    fn render_results(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *cx.palette();
        let max_rows = Self::max_rows(cx) as usize;
        let content = if let Some(err) = &self.err {
            div()
                .p_3()
                .text_sm()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.theme().danger)
                .child(err.clone())
                .into_any_element()
        } else if let Some(result) = &self.result {
            let rows = result.rows.len();
            if rows == 0 && !self.result_is_query {
                let affected = result
                    .affected
                    .filter(|n| *n > 0)
                    .map(|n| format!(" · {}", t!("tab.affected_rows", n = n)))
                    .unwrap_or_default();
                div()
                    .p_3()
                    .text_sm()
                    .text_color(palette.data)
                    .child(format!("✓ {}{affected}", t!("tab.exec_ok")))
                    .into_any_element()
            } else if rows == 0 && result.columns.is_empty() {
                ui::hint(t!("tab.query_ok_no_rows").to_string(), cx).into_any_element()
            } else if rows > 0 && self.view.is_empty() {
                div()
                    .p_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("tab.no_filter_match", total = rows).to_string())
                    .into_any_element()
            } else {
                v_flex()
                    .size_full()
                    .when(
                        self.browse_table.is_none() && self.result_is_query && rows >= max_rows,
                        |this| {
                            this.child(
                                div()
                                    .px_3()
                                    .py_1p5()
                                    .text_xs()
                                    .text_color(cx.theme().warning)
                                    .border_b_1()
                                    .border_color(cx.theme().border)
                                    .child(t!("tab.row_limit_reached", n = max_rows).to_string()),
                            )
                        },
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .child(self.measure_width(cx))
                            .child(
                                DataTable::new(&self.grid)
                                    .stripe(true)
                                    .bordered(false)
                                    .with_size(px(36.))
                                    .header_row_height(px(44.))
                                    .scrollbar_visible(true, true),
                            ),
                    )
                    .into_any_element()
            }
        } else if self.running {
            ui::hint(t!("tab.running").to_string(), cx).into_any_element()
        } else {
            div().into_any_element()
        };
        div()
            .id("results")
            .flex_1()
            .min_h_0()
            .size_full()
            .bg(cx.theme().background)
            .on_action(cx.listener(|this, _: &CommitEdits, window, cx| this.commit(window, cx)))
            .child(content)
    }

    /// 量结果区宽度:变化后(防抖 120ms)按新宽度重排列宽,让列铺满结果区。
    fn measure_width(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let measured = self.measured_width.clone();
        let this = cx.entity().downgrade();
        gpui_kit::canvas(
            move |bounds, window, cx| {
                let w = f32::from(bounds.size.width);
                if (w - measured.get()).abs() > 1. {
                    measured.set(w);
                    let this = this.clone();
                    window.defer(cx, move |_, cx| {
                        let _ = this.update(cx, |tab, cx| tab.schedule_refit(cx));
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }

    fn schedule_refit(&mut self, cx: &mut Context<Self>) {
        let first = self.fitted_width == 0.;
        self._refit = Some(cx.spawn(async move |this, cx| {
            if !first {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
            }
            let _ = this.update(cx, |this, cx| {
                let w = this.measured_width.get();
                if (w - this.fitted_width).abs() > 1. {
                    this.fitted_width = w;
                    this.push_grid(cx);
                }
            });
        }));
    }

    fn render_status_line(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *cx.palette();
        let mono = cx.theme().mono_font_family.clone();
        h_flex()
            .h(px(30.))
            .flex_shrink_0()
            .px_4()
            .gap_2p5()
            .bg(palette.panel)
            .border_b_1()
            .border_color(cx.theme().border)
            .text_xs()
            .map(|this| {
                if self.running {
                    this.child(ui::dot(palette.data, 6.)).child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("tab.running")),
                    )
                } else if self.err.is_some() {
                    this.child(ui::dot(cx.theme().danger, 6.)).child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .text_color(cx.theme().danger)
                            .child(t!("tab.query_error")),
                    )
                } else if let Some(result) = &self.result {
                    let summary = if self.result_is_query {
                        t!("tab.query_ok_rows", n = self.view.len()).to_string()
                    } else {
                        let affected = result
                            .affected
                            .filter(|n| *n > 0)
                            .map(|n| format!(" · {}", t!("tab.affected_rows", n = n)))
                            .unwrap_or_default();
                        format!("{}{affected}", t!("tab.exec_ok"))
                    };
                    this.child(ui::dot(palette.data, 6.))
                        .child(div().text_color(cx.theme().muted_foreground).child(summary))
                        .when_some(self.elapsed_ms, |this, ms| {
                            this.child(
                                div()
                                    .font_family(mono.clone())
                                    .text_color(palette.fg_faint)
                                    .child(t!("tab.elapsed_ms", ms = ms).to_string()),
                            )
                        })
                } else {
                    this.child(div().text_color(palette.fg_faint).child(t!("tab.ready")))
                }
            })
    }
}

impl Render for WorkTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let filter_bar = if self.filter.open && self.result.is_some() {
            Some(filter::render(self, window, cx).into_any_element())
        } else {
            None
        };
        let body = match (&self.kind, &self.editor) {
            (TabKind::Query, Some(editor)) => v_resizable("query-split")
                .child(
                    resizable_panel()
                        .size(px(220.))
                        .size_range(px(90.)..px(2000.))
                        .child(
                            v_flex()
                                .size_full()
                                .child(
                                    div()
                                        .id("sql-editor")
                                        .key_context("SqlEditor")
                                        .relative()
                                        .flex_1()
                                        .min_h_0()
                                        .bg(cx.palette().editor)
                                        .on_action(cx.listener(|this, _: &RunQuery, window, cx| {
                                            this.run(None, false, false, window, cx)
                                        }))
                                        .on_action(cx.listener(
                                            |this, _: &FormatSql, window, cx| {
                                                this.beautify(window, cx)
                                            },
                                        ))
                                        .on_action(cx.listener(
                                            |this, _: &ExplainQuery, window, cx| {
                                                this.explain(window, cx)
                                            },
                                        ))
                                        .on_action(cx.listener(
                                            |this, _: &SendSelectionToAi, _, cx| {
                                                let text =
                                                    this.selection_text(cx).trim().to_string();
                                                if text.is_empty() {
                                                    cx.emit(TabEvent::Toast(Toast::Error(
                                                        t!("tab.select_sql_for_ai")
                                                            .to_string()
                                                            .into(),
                                                    )));
                                                } else {
                                                    cx.emit(TabEvent::SendToAi(text));
                                                }
                                            },
                                        ))
                                        .child(
                                            Editor::new(editor)
                                                .h(gpui_kit::relative(1.))
                                                .bordered(false)
                                                .context_menu({
                                                    let editor = editor.clone();
                                                    let relational = self.db_kind != DbKind::Redis;
                                                    move |menu, _, cx| {
                                                        let has_selection = !editor
                                                            .read(cx)
                                                            .selected_text()
                                                            .to_string()
                                                            .trim()
                                                            .is_empty();
                                                        editor_menu(menu, has_selection, relational)
                                                    }
                                                }),
                                        )
                                        .when_some(self.chip.clone(), |this, chip| {
                                            let origin = chip.read(cx).origin.clone();
                                            this.child(
                                                gpui_kit::canvas(
                                                    move |b, _, _| origin.set(b),
                                                    |_, _, _, _| {},
                                                )
                                                .absolute()
                                                .size_full(),
                                            )
                                            .child(chip)
                                        }),
                                )
                                .child(self.render_toolbar(cx)),
                        ),
                )
                .child(
                    resizable_panel().child(
                        v_flex()
                            .size_full()
                            .child(self.render_status_line(cx))
                            .child(self.render_results(cx)),
                    ),
                )
                .into_any_element(),
            _ => v_flex()
                .size_full()
                .child(self.render_results(cx))
                .into_any_element(),
        };
        v_flex()
            .id("work-tab")
            .track_focus(&self.focus)
            .size_full()
            .children(filter_bar)
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// 编辑器右键菜单:发送给 AI / 运行 / Explain / 格式化。
fn editor_menu(
    menu: gpui_kit::component::native_menu::NativeMenu,
    has_selection: bool,
    relational: bool,
) -> gpui_kit::component::native_menu::NativeMenu {
    let menu = if has_selection {
        menu.menu(
            t!("tab.menu.send_to_ai").to_string(),
            Box::new(SendSelectionToAi),
        )
    } else {
        menu
    };
    let menu = menu.menu(t!("tab.menu.run").to_string(), Box::new(RunQuery));
    let menu = if relational {
        menu.menu(t!("tab.menu.explain").to_string(), Box::new(ExplainQuery))
            .menu(t!("tab.menu.format").to_string(), Box::new(FormatSql))
    } else {
        menu
    };
    menu.separator()
        .menu(
            t!("tab.menu.cut").to_string(),
            Box::new(gpui_kit::component::input::Cut),
        )
        .menu(
            t!("tab.menu.copy").to_string(),
            Box::new(gpui_kit::component::input::Copy),
        )
        .menu(
            t!("tab.menu.paste").to_string(),
            Box::new(gpui_kit::component::input::Paste),
        )
}
