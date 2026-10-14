//! 工作区的命令实现:导出、切库/删库、建表语句、连接的增删改、打开各类对话框。

use super::{Workspace, show_toast};
use crate::dialogs::{
    self,
    risk::{RiskGate, confirm_destructive, sql_view},
};
use crate::runtime;
use crate::store::{Stores, Toast};
use crate::ui;
use datum_core::core::connection::{Connection, DbKind, Env};
use datum_core::grid::{file_timestamp, to_csv, to_markdown};
use datum_core::service;
use datum_core::sql::Dialect;
use datum_core::sql::build::{build_insert_row, ident};
use datum_core::sql::risk::{RiskFinding, RiskLevel};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::Editor;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::{
    App, AsyncWindowContext, ClipboardItem, Context, ParentElement as _, Styled as _, Window, div,
    px,
};
use rust_i18n::t;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Sql,
    Markdown,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 3] = [Self::Csv, Self::Sql, Self::Markdown];

    pub fn menu_label(self) -> String {
        t!("ops.export_as", format = self.short()).to_string()
    }

    pub fn table_menu_label(self) -> String {
        t!("ops.export_table_as", format = self.short()).to_string()
    }

    fn ext(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Sql => "sql",
            Self::Markdown => "md",
        }
    }

    fn short(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Sql => "SQL",
            Self::Markdown => "Markdown",
        }
    }

    fn render(
        self,
        table: &str,
        columns: &[String],
        rows: &[&Vec<Option<String>>],
        dialect: Dialect,
    ) -> String {
        match self {
            Self::Csv => to_csv(columns, rows),
            Self::Markdown => to_markdown(columns, rows),
            Self::Sql => rows
                .iter()
                .map(|r| build_insert_row(table, columns, r, dialect))
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// 弹出系统保存对话框(默认下载目录),写入文本并提示。
pub fn save_text(
    file_name: String,
    text: String,
    success: String,
    window: &mut Window,
    cx: &mut App,
) {
    let dir = dirs::download_dir().unwrap_or_else(|| PathBuf::from("."));
    let rx = cx.prompt_for_new_path(&dir, Some(&file_name));
    window
        .spawn(cx, async move |cx: &mut AsyncWindowContext| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let result = std::fs::write(&path, text);
            let _ = cx.update(|window, cx| match result {
                Ok(()) => show_toast(&Toast::Success(success.into()), window, cx),
                Err(e) => show_toast(
                    &Toast::Error(t!("ops.write_failed", error = e).into()),
                    window,
                    cx,
                ),
            });
        })
        .detach();
}

/// 同 [`save_text`],写入二进制(PNG 等)。
pub fn save_bytes(
    file_name: String,
    bytes: Vec<u8>,
    success: String,
    window: &mut Window,
    cx: &mut App,
) {
    let dir = dirs::download_dir().unwrap_or_else(|| PathBuf::from("."));
    let rx = cx.prompt_for_new_path(&dir, Some(&file_name));
    window
        .spawn(cx, async move |cx: &mut AsyncWindowContext| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let result = std::fs::write(&path, bytes);
            let _ = cx.update(|window, cx| match result {
                Ok(()) => show_toast(&Toast::Success(success.into()), window, cx),
                Err(e) => show_toast(
                    &Toast::Error(t!("ops.write_failed", error = e).into()),
                    window,
                    cx,
                ),
            });
        })
        .detach();
}

impl Workspace {
    // ------------------------------------------------------------ 导出

    /// 导出当前结果(已应用筛选后的行)。
    pub fn export_filtered(
        &mut self,
        format: ExportFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab(cx) else {
            return;
        };
        let t = tab.read(cx);
        let (Some(result), Some(dialect)) = (t.result.clone(), t.dialect().or(Some(Dialect::Pg)))
        else {
            return;
        };
        let rows = t.view_rows();
        if rows.is_empty() {
            return;
        }
        let table = t.export_name(cx);
        let text = format.render(&table, &result.columns, &rows, dialect);
        save_text(
            format!("{table}_{}.{}", file_timestamp(), format.ext()),
            text,
            t!("ops.exported", format = format.short()).to_string(),
            window,
            cx,
        );
    }

    /// 表右键「导出整表」:查询全部行(不受行数上限)。
    pub fn export_table(
        &mut self,
        conn_id: String,
        table: String,
        format: ExportFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = Stores::global(cx).conns.read(cx).get(&conn_id).cloned() else {
            return;
        };
        let Some(dialect) = Dialect::from_kind(&conn.kind) else {
            return;
        };
        let kind = conn.kind;
        cx.spawn_in(window, async move |_, cx: &mut AsyncWindowContext| {
            let sql = format!("SELECT * FROM {}", ident(&table, dialect));
            let id = conn_id.clone();
            let result = runtime::backend(move |s| async move {
                service::query(&s, &kind, &id, &sql, None).await
            })
            .await;
            let _ = cx.update(|window, cx| match result {
                Ok(r) if r.rows.is_empty() => {
                    show_toast(&Toast::Info(t!("ops.table_empty").into()), window, cx)
                }
                Ok(r) => {
                    let rows: Vec<&Vec<Option<String>>> = r.rows.iter().collect();
                    let text = format.render(&table, &r.columns, &rows, dialect);
                    save_text(
                        format!("{table}_{}.{}", file_timestamp(), format.ext()),
                        text,
                        t!("ops.exported", format = format.short()).to_string(),
                        window,
                        cx,
                    );
                }
                Err(e) => show_toast(&Toast::Error(e.display().into()), window, cx),
            });
        })
        .detach();
    }

    /// 导出整个数据库:逐表查询全部行,合成一个 SQL 文件。
    pub fn export_database(
        &mut self,
        conn_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.export_database_then(conn_id, None, window, cx);
    }

    /// `cleanup`:导出完成后删除的临时克隆连接(导出其他库时用)。
    fn export_database_then(
        &mut self,
        conn_id: String,
        cleanup: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.exporting_db {
            show_toast(&Toast::Info(t!("ops.export_busy").into()), window, cx);
            return;
        }
        let Some(conn) = Stores::global(cx).conns.read(cx).get(&conn_id).cloned() else {
            return;
        };
        let kind = conn.kind;
        let cached = Stores::global(cx)
            .conns
            .read(cx)
            .tables
            .get(&conn_id)
            .cloned();
        self.exporting_db = true;
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let id = conn_id.clone();
            // 侧栏没展开过这个连接时表名还没加载,这里补一次
            let tables = match cached.filter(|t| !t.is_empty()) {
                Some(t) => Ok(t),
                None => {
                    let id = id.clone();
                    runtime::backend(move |s| async move {
                        service::connect(&s, &kind, &id).await?;
                        service::list_objects(&s, &kind, &id).await
                    })
                    .await
                }
            };
            let tables = match tables {
                Ok(t) if !t.is_empty() => t,
                Ok(_) => {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.exporting_db = false;
                        show_toast(&Toast::Info(t!("ops.no_tables").into()), window, cx);
                    });
                    return;
                }
                Err(e) => {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.exporting_db = false;
                        show_toast(&Toast::Error(e.display().into()), window, cx);
                    });
                    return;
                }
            };
            let n = tables.len();
            let _ = cx.update(|window, cx| {
                show_toast(
                    &Toast::Info(t!("ops.exporting_n", count = n).into()),
                    window,
                    cx,
                )
            });
            let text = runtime::backend(move |s| async move {
                service::export_database(&s, &kind, &id, &tables).await
            })
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.exporting_db = false;
                save_text(
                    format!("{}_{}.sql", conn.name, file_timestamp()),
                    text,
                    t!("ops.exported_db", count = n).to_string(),
                    window,
                    cx,
                );
                if let Some(tmp) = cleanup {
                    let conns = Stores::global(cx).conns.clone();
                    let _ = conns.update(cx, |s, cx| s.delete(&tmp, cx));
                }
            });
        })
        .detach();
    }

    // ------------------------------------------------------------ 库切换

    /// 选库:同实例已有该库的连接则切换过去,否则克隆一个(密码一并复制)再切换。
    pub fn pick_database(&mut self, db: &str, window: &mut Window, cx: &mut Context<Self>) {
        let conns = Stores::global(cx).conns.clone();
        let Some(active) = conns.read(cx).active().cloned() else {
            return;
        };
        if db == active.database {
            return;
        }
        let result = conns.update(cx, |s, cx| s.switch_database(&active.id, db, cx));
        match result {
            Ok(next) => conns.update(cx, |s, cx| {
                s.activate(&next.id, cx);
                s.load_tables(&next.id, cx).detach();
            }),
            Err(e) => show_toast(
                &Toast::Error(t!("ops.switch_db_failed", error = e.display()).into()),
                window,
                cx,
            ),
        }
    }

    /// 库列表右键「编辑连接」:打开连接表单,库名预填为右键的库。
    pub fn edit_conn_with_db(&mut self, db: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut conn) = Stores::global(cx).conns.read(cx).active().cloned() else {
            return;
        };
        conn.database = db.to_string();
        self.open_connection_form(Some(conn), window, cx);
    }

    /// 导出右键的库:当前库直接导;其他库先拿到(复用或克隆的)连接,不切换侧栏焦点。
    /// 若连接是为导出新克隆的,导出完成后删掉。
    pub fn export_db_by_name(&mut self, db: &str, window: &mut Window, cx: &mut Context<Self>) {
        let conns = Stores::global(cx).conns.clone();
        let Some(active) = conns.read(cx).active().cloned() else {
            return;
        };
        if db == active.database {
            self.export_database(active.id, window, cx);
            return;
        }
        let existed = !conns.read(cx).siblings(&active, db).is_empty();
        match conns.update(cx, |s, cx| s.switch_database(&active.id, db, cx)) {
            Ok(next) => {
                let cleanup = (!existed).then(|| next.id.clone());
                self.export_database_then(next.id, cleanup, window, cx);
            }
            Err(e) => show_toast(
                &Toast::Error(t!("ops.prepare_export_failed", error = e.display()).into()),
                window,
                cx,
            ),
        }
    }

    /// DROP 右键的库:当前库拒绝;其余走破坏性审查,确认后先断开指向目标库的残留连接再执行。
    pub fn drop_db_by_name(&mut self, db: &str, window: &mut Window, cx: &mut Context<Self>) {
        let conns = Stores::global(cx).conns.clone();
        let Some(active) = conns.read(cx).active().cloned() else {
            return;
        };
        if db == active.database {
            show_toast(
                &Toast::Error(t!("ops.cannot_drop_current").into()),
                window,
                cx,
            );
            return;
        }
        let Some(dialect) = Dialect::from_kind(&active.kind) else {
            return;
        };
        let sql = format!("DROP DATABASE {}", ident(db, dialect));
        let gate = RiskGate {
            findings: vec![RiskFinding {
                sql: sql.clone(),
                level: RiskLevel::Danger,
                kind: "DROP DATABASE".into(),
                object: Some(db.to_string()),
            }],
            target_label: active.name.clone(),
            database: Some(db.to_string()),
            dialect: Some(dialect),
        };
        let db = db.to_string();
        let this = cx.entity().downgrade();
        confirm_destructive(
            gate,
            move |window, cx| {
                let (sql, db, active) = (sql.clone(), db.clone(), active.clone());
                let _ = this.update(cx, |_, cx| {
                    let stale = Stores::global(cx).conns.read(cx).siblings(&active, &db);
                    let kind = active.kind;
                    cx.spawn_in(window, async move |_, cx: &mut AsyncWindowContext| {
                        let stale_ids: Vec<String> = stale.iter().map(|c| c.id.clone()).collect();
                        let ids = stale_ids.clone();
                        let id = active.id.clone();
                        let result = runtime::backend(move |s| async move {
                            // 活跃会话会让 PG 拒绝 DROP:先强制断开
                            for sid in &ids {
                                service::disconnect(&s, &kind, sid).await;
                            }
                            service::query(&s, &kind, &id, &sql, None).await
                        })
                        .await;
                        let _ = cx.update(|window, cx| match result {
                            Ok(_) => {
                                let conns = Stores::global(cx).conns.clone();
                                conns.update(cx, |s, cx| {
                                    for sid in &stale_ids {
                                        let _ = s.delete(sid, cx);
                                    }
                                    if let Some(dbs) = s.databases.get_mut(&active.id) {
                                        dbs.retain(|d| *d != db);
                                    }
                                    cx.notify();
                                });
                                show_toast(
                                    &Toast::Success(t!("ops.dropped_db", name = db).into()),
                                    window,
                                    cx,
                                );
                            }
                            Err(e) => show_toast(&Toast::Error(e.display().into()), window, cx),
                        });
                    })
                    .detach();
                });
            },
            window,
            cx,
        );
    }

    // ------------------------------------------------------------ 表

    /// 右键表「查看表详情」:聚焦(或新开)该表 tab,清掉行选中后展开右栏。
    pub fn show_table_detail(
        &mut self,
        conn_id: &str,
        table: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.open_table(conn_id, table, window, cx) {
            tab.update(cx, |t, cx| t.clear_selection(cx));
            self.detail_open = true;
            cx.notify();
        }
    }

    /// 查看建表语句。
    pub fn show_ddl(
        &mut self,
        conn_id: &str,
        table: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = Stores::global(cx).conns.read(cx).get(conn_id).cloned() else {
            return;
        };
        let (kind, id, t) = (conn.kind, conn_id.to_string(), table.to_string());
        let table = table.to_string();
        cx.spawn_in(window, async move |_, cx: &mut AsyncWindowContext| {
            let ddl = runtime::backend(move |s| async move {
                service::create_table_ddl(&s, &kind, &id, &t).await
            })
            .await;
            let _ = cx.update(|window, cx| match ddl {
                Ok(ddl) => open_ddl_dialog(&table, ddl, Dialect::from_kind(&kind), window, cx),
                Err(e) => show_toast(&Toast::Error(e.display().into()), window, cx),
            });
        })
        .detach();
    }

    // ------------------------------------------------------------ 连接

    pub fn open_connection_form(
        &mut self,
        conn: Option<Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        dialogs::connection_form::open(conn, window, cx);
    }

    pub fn confirm_delete_connection(
        &mut self,
        conn: Connection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        let name = conn.name.clone();
        let keychain = conn.env != Env::Local;
        window.open_alert_dialog(cx, move |alert, _, _| {
            let conn = conn.clone();
            let this = this.clone();
            alert
                .title(t!("ops.delete_conn_title", name = name))
                .description(if keychain {
                    t!("ops.delete_conn_desc_keychain")
                } else {
                    t!("ops.delete_conn_desc")
                })
                .show_cancel(true)
                .cancel_text(t!("ops.cancel"))
                .ok_text(t!("ops.delete"))
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let conn = conn.clone();
                    let _ = this.update(cx, |ws, cx| {
                        let conns = Stores::global(cx).conns.clone();
                        match conns.update(cx, |s, cx| s.delete(&conn.id, cx)) {
                            Ok(()) => {
                                ws.tabs.retain(|t| t.read(cx).conn_id != conn.id);
                                ws.active_by_conn.remove(&conn.id);
                                cx.notify();
                            }
                            Err(e) => show_toast(&Toast::Error(e.display().into()), window, cx),
                        }
                    });
                    true
                })
        });
    }

    pub fn open_import(
        &mut self,
        conn_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        dialogs::import::open(conn_id, window, cx);
    }

    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        dialogs::settings::open(window, cx);
    }

    pub fn open_erd(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let conn = Stores::global(cx)
            .conns
            .read(cx)
            .active()
            .cloned()
            .filter(|c| c.kind != DbKind::Redis);
        let this = cx.entity().downgrade();
        crate::erd::open(conn.map(|c| c.id), this, window, cx);
    }
}

/// 建表语句弹窗:只读高亮 + 复制。
fn open_ddl_dialog(
    table: &str,
    ddl: String,
    dialect: Option<Dialect>,
    window: &mut Window,
    cx: &mut App,
) {
    let view = sql_view(&ddl, dialect.or(Some(Dialect::Pg)), window, cx);
    let title = t!("ops.ddl_title", name = table).to_string();
    window.open_dialog(cx, move |dialog, _, cx| {
        let ddl = ddl.clone();
        dialog
            .w(px(640.))
            .title(title.clone())
            .child(
                div()
                    .h(px(380.))
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .overflow_hidden()
                    .child(
                        Editor::new(&view)
                            .readonly(true)
                            .bordered(false)
                            .h(gpui_kit::relative(1.)),
                    ),
            )
            .footer(
                h_flex().gap_2().justify_end().child(
                    Button::new("ddl-copy")
                        .primary()
                        .small()
                        .icon(ui::icon("copy"))
                        .label(t!("ops.copy"))
                        .on_click(move |_, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(ddl.clone()));
                            show_toast(&Toast::Success(t!("workspace.copied").into()), window, cx);
                        }),
                ),
            )
    });
}
