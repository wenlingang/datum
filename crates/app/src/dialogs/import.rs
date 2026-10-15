//! 导入数据库:选择目标库,从 .sql 文件读入或直接粘贴 SQL,单事务执行
//! (全成功提交,任一失败回滚并报告第几条)。

use crate::runtime;
use crate::store::Stores;
use crate::theme::ActivePalette as _;
use crate::ui;
use datum_core::commands::read_sql_file;
use datum_core::core::connection::DbKind;
use datum_core::service;
use datum_core::sql::stmt::statements;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, AsyncWindowContext, Context, Entity, FontWeight, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Subscription, Task,
    Window, div, px,
};
use rust_i18n::t;

struct Report {
    ok: bool,
    text: String,
}

pub struct ImportView {
    conn_id: Option<String>,
    editor: Entity<EditorState>,
    file_name: Option<String>,
    running: bool,
    report: Option<Report>,
    _task: Option<Task<()>>,
    _sub: Subscription,
}

impl ImportView {
    fn new(conn_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .placeholder(t!("import.placeholder"))
        });
        let sub = cx.subscribe(&editor, |this, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                this.report = None;
                cx.notify();
            }
        });
        // 只接受关系库
        let conn_id = conn_id.filter(|id| {
            Stores::global(cx)
                .conns
                .read(cx)
                .get(id)
                .is_some_and(|c| c.kind.supports_sql())
        });
        Self {
            conn_id,
            editor,
            file_name: None,
            running: false,
            report: None,
            _task: None,
            _sub: sub,
        }
    }

    fn pick_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("import.choose_file").to_string().into()),
        });
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let text = read_sql_file(path.to_string_lossy().to_string());
            let _ = this.update_in(cx, |this, window, cx| {
                match text {
                    Ok(text) => {
                        this.editor
                            .update(cx, |e, cx| e.set_value(text, window, cx));
                        this.file_name = path.file_name().map(|n| n.to_string_lossy().to_string());
                        this.report = None;
                    }
                    Err(e) => this.report = Some(Report { ok: false, text: e }),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conn) = self
            .conn_id
            .as_ref()
            .and_then(|id| Stores::global(cx).conns.read(cx).get(id).cloned())
        else {
            return;
        };
        let stmts = statements(&self.editor.read(cx).value());
        if stmts.is_empty() {
            return;
        }
        self.running = true;
        self.report = None;
        cx.notify();
        let (kind, id) = (conn.kind, conn.id.clone());
        self._task = Some(
            cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
                let task_id = id.clone();
                let result = runtime::backend(move |s| async move {
                    service::connect(&s, &kind, &task_id).await?;
                    service::import_sql(&s, &kind, &task_id, stmts).await
                })
                .await;
                let _ = this.update(cx, |this, cx| {
                    this.running = false;
                    match result {
                        Ok(n) => {
                            this.report = Some(Report {
                                ok: true,
                                text: t!("import.done", count = n).to_string(),
                            });
                            // 结构可能已变:刷新侧栏表列表、失效 ER 关系缓存
                            let conns = Stores::global(cx).conns.clone();
                            conns.update(cx, |s, cx| {
                                s.invalidate_erd(&id);
                                s.load_tables(&id, cx).detach();
                            });
                        }
                        Err(e) => {
                            this.report = Some(Report {
                                ok: false,
                                text: t!("import.failed", error = e.display()).to_string(),
                            })
                        }
                    }
                    cx.notify();
                });
            }),
        );
    }
}

impl Render for ImportView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *cx.palette();
        let store = Stores::global(cx).conns.read(cx);
        let choices: Vec<(String, String, DbKind)> = store
            .connections
            .iter()
            .filter(|c| c.kind.supports_sql())
            .map(|c| (c.id.clone(), c.name.clone(), c.kind))
            .collect();
        let current = self
            .conn_id
            .as_ref()
            .and_then(|id| choices.iter().find(|c| c.0 == *id))
            .cloned();
        let n = statements(&self.editor.read(cx).value()).len();
        let this = cx.entity().downgrade();
        let label = |text: SharedString| {
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(cx.theme().muted_foreground)
                .child(text)
        };

        v_flex()
            .gap_3p5()
            .child(
                v_flex()
                    .gap_1p5()
                    .child(label(t!("import.target").into()))
                    .child(
                        Button::new("import-target")
                            .outline()
                            .w_full()
                            .justify_start()
                            .when_some(current.clone(), |b, (_, _, kind)| b.icon(ui::db_icon(kind)))
                            .label(
                                current
                                    .as_ref()
                                    .map(|c| c.1.clone())
                                    .unwrap_or_else(|| t!("import.choose_db").to_string()),
                            )
                            .dropdown_caret(true)
                            .disabled(self.running)
                            .dropdown_menu(move |menu: PopupMenu, _, _| {
                                choices.iter().fold(menu, |menu, (id, name, kind)| {
                                    let (this, id) = (this.clone(), id.clone());
                                    menu.item(
                                        PopupMenuItem::new(name.clone())
                                            .icon(ui::db_icon(*kind))
                                            .on_click(move |_, _, cx| {
                                                let id = id.clone();
                                                let _ = this.update(cx, |v, cx| {
                                                    v.conn_id = Some(id);
                                                    v.report = None;
                                                    cx.notify();
                                                });
                                            }),
                                    )
                                })
                            }),
                    ),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(label(t!("import.sql_content").into()))
                            .child(
                                Button::new("import-file")
                                    .outline()
                                    .xsmall()
                                    .icon(ui::icon("file-up"))
                                    .label(t!("import.choose_file"))
                                    .disabled(self.running)
                                    .on_click(
                                        cx.listener(|v, _, window, cx| v.pick_file(window, cx)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .h(px(220.))
                            .rounded(cx.theme().radius_lg)
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(palette.editor)
                            .overflow_hidden()
                            .child(
                                Editor::new(&self.editor)
                                    .bordered(false)
                                    .h(gpui_kit::relative(1.)),
                            ),
                    )
                    .child(div().text_xs().text_color(palette.fg_faint).child(format!(
                            "{}{}",
                            self.file_name
                                .as_ref()
                                .map(|f| t!("import.loaded", file = f).to_string())
                                .unwrap_or_default(),
                            if n > 0 {
                                t!("import.statements_n", count = n).to_string()
                            } else {
                                t!("import.no_statements").to_string()
                            }
                        ))),
            )
            .when_some(self.report.as_ref(), |this, r| {
                this.child(
                    div()
                        .px_3()
                        .py_2p5()
                        .rounded(cx.theme().radius_lg)
                        .text_sm()
                        .bg(if r.ok {
                            palette.selection
                        } else {
                            palette.delete_bg
                        })
                        .text_color(if r.ok {
                            cx.theme().foreground
                        } else {
                            palette.negative
                        })
                        .child(r.text.clone()),
                )
            })
            .child(
                h_flex()
                    .gap_2p5()
                    .justify_end()
                    .child(
                        Button::new("import-close")
                            .outline()
                            .label(t!("import.close"))
                            .disabled(self.running)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("import-run")
                            .primary()
                            .loading(self.running)
                            .label(if self.running {
                                t!("import.importing")
                            } else {
                                t!("import.start")
                            })
                            .disabled(current.is_none() || n == 0 || self.running)
                            .on_click(cx.listener(|v, _, window, cx| v.run(window, cx))),
                    ),
            )
    }
}

pub fn open(conn_id: Option<String>, window: &mut Window, cx: &mut gpui_kit::App) {
    let view = cx.new(|cx| ImportView::new(conn_id, window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .w(px(640.))
            .title(t!("import.title").to_string())
            .overlay_closable(false)
            .child(view.clone())
    });
}
