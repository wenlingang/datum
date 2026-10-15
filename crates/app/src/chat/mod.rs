//! 右侧 AI 聊天面板。按连接分会话;会话首条消息时绑定当前连接,
//! 之后 SQL 执行与 schema 上下文都固定在该连接上——切换活跃连接不影响已生成卡片的执行目标。

mod sql_card;

use crate::prefs::Prefs;
use crate::runtime;
use crate::store::{AiRole, BoundConn, Stores, Toast};
use crate::theme::ActivePalette as _;
use crate::ui;
use datum_core::ai::markdown::{Segment, parse_segments};
use datum_core::ai::prompt::{PromptContext, build_system_prompt, mentioned_tables};
use datum_core::core::connection::{Connection, DbKind};
use datum_core::pg::browse::TableDetail;
use datum_core::service;
use datum_core::sql::Dialect;
use datum_core::sql::stmt::{is_query, statements};
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AppContext as _, AsyncWindowContext, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};
use rust_i18n::t;
use sql_card::SqlCard;
use std::collections::{BTreeMap, HashMap};

pub enum ChatEvent {
    /// 「编辑器运行」:在对话绑定的连接上替换查询编辑器内容并执行。
    /// `confirmed` 为卡片已弹过破坏性确认,工作区不再二次审查。
    InsertAndRun {
        sql: String,
        conn_id: String,
        confirmed: bool,
    },
    /// 「引用选中」:把编辑器当前选区作为引用
    QuoteSelection,
    OpenSettings,
    Toast(Toast),
}

pub struct ChatPanel {
    input: Entity<TextareaState>,
    scroll: ScrollHandle,
    /// 按需拉取过的表结构缓存(key = connId:table),对话期间复用
    details: HashMap<String, TableDetail>,
    /// SQL 卡片实体:(消息 id, 段序号) → 卡片
    cards: HashMap<(u64, usize), Entity<SqlCard>>,
    workspace: gpui_kit::WeakEntity<crate::workspace::Workspace>,
    last_count: usize,
    _subscriptions: Vec<Subscription>,
    _send: Option<Task<()>>,
}

impl EventEmitter<ChatEvent> for ChatPanel {}

fn conn_label(c: &Connection) -> String {
    let db = if c.kind == DbKind::Sqlite {
        c.file_path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "—".into())
    } else if c.database.is_empty() {
        "—".into()
    } else {
        c.database.clone()
    };
    format!("{} / {db}", c.name)
}

fn truncate_sql(sql: &str) -> String {
    if sql.chars().count() > 200 {
        format!("{}…", sql.chars().take(200).collect::<String>())
    } else {
        sql.to_string()
    }
}

impl ChatPanel {
    pub fn new(
        workspace: gpui_kit::WeakEntity<crate::workspace::Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder(t!("chat.placeholder_ask").to_string())
        });
        let stores = Stores::global(cx).clone();
        let subs = vec![
            cx.subscribe_in(
                &input,
                window,
                |this, _, e: &InputEvent, window, cx| match e {
                    InputEvent::PressEnter { shift: false, .. } => this.send(window, cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
                },
            ),
            cx.observe(&stores.ai, |_, _, cx| cx.notify()),
            // 切换会话(连接)时清空未发送的引用,避免误发到不相关的连接下
            cx.observe(&stores.conns, {
                let mut last: Option<String> = None;
                move |_, conns, cx| {
                    let active = conns.read(cx).active_id.clone();
                    if active != last {
                        last = active;
                        let ai = Stores::global(cx).ai.clone();
                        if ai.read(cx).pending_ref.is_some() {
                            ai.update(cx, |ai, cx| ai.set_pending_ref(None, cx));
                        }
                    }
                    cx.notify();
                }
            }),
        ];
        Self {
            input,
            scroll: ScrollHandle::new(),
            details: HashMap::new(),
            cards: HashMap::new(),
            workspace,
            last_count: 0,
            _subscriptions: subs,
            _send: None,
        }
    }

    /// 会话 key:按连接分会话。切表、开新查询 tab 都不打断当前对话。
    fn convo_key(&self, cx: &gpui_kit::App) -> Option<String> {
        Stores::global(cx)
            .conns
            .read(cx)
            .active_id
            .as_ref()
            .map(|id| format!("conn:{id}"))
    }

    /// 当前 tab 正在浏览的表及其结构(零成本的上下文来源)。
    fn current_table(&self, cx: &gpui_kit::App) -> Option<(String, String, TableDetail)> {
        let ws = self.workspace.upgrade()?;
        let tab = ws.read(cx).active_tab(cx)?;
        let t = tab.read(cx);
        Some((t.conn_id.clone(), t.table.clone()?, t.detail.clone()?))
    }

    fn target(&self, cx: &gpui_kit::App) -> Option<BoundConn> {
        let key = self.convo_key(cx)?;
        let ai = Stores::global(cx).ai.read(cx);
        if let Some(b) = ai.convo(&key).and_then(|c| c.bound.clone()) {
            return Some(b);
        }
        let conn = Stores::global(cx)
            .conns
            .read(cx)
            .active()
            .cloned()
            .filter(|c| c.kind.supports_sql())?;
        Some(BoundConn {
            conn_id: conn.id.clone(),
            kind: conn.kind,
            label: conn_label(&conn),
        })
    }

    fn can_send(&self, cx: &gpui_kit::App) -> bool {
        let ai = Stores::global(cx).ai.read(cx);
        let ready = ai.status.as_ref().is_some_and(|s| s.ready);
        ready
            && self.target(cx).is_some()
            && ai.streaming_key.is_none()
            && self.convo_key(cx).is_some()
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.send_text(None, window, cx);
    }

    /// 截图脚本:在输入框填入文本并按发送,走与回车完全相同的路径。
    #[cfg(feature = "snapshot")]
    pub fn snapshot_send(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |i, cx| i.set_value(text.to_string(), window, cx));
        self.send(window, cx);
    }

    /// 发送:输入框文本(或快捷动作文本)+ 引用 SQL。schema 上下文按需拉取后再发。
    fn send_text(&mut self, text: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let typed = text.unwrap_or_else(|| self.input.read(cx).value().trim().to_string());
        let ai = Stores::global(cx).ai.clone();
        let pending = ai.read(cx).pending_ref.clone();
        if (typed.is_empty() && pending.is_none()) || !self.can_send(cx) {
            return;
        }
        let (Some(key), Some(target)) = (self.convo_key(cx), self.target(cx)) else {
            return;
        };
        let final_text = match &pending {
            Some(sql) => format!(
                "{}{}\n```sql\n{sql}\n```",
                if typed.is_empty() {
                    String::new()
                } else {
                    format!("{typed}\n\n")
                },
                t!("chat.quoted_sql")
            ),
            None => typed,
        };
        if ai
            .read(cx)
            .convo(&key)
            .and_then(|c| c.bound.clone())
            .is_none()
        {
            ai.update(cx, |ai, cx| ai.bind(&key, target.clone(), cx));
        }
        self.input.update(cx, |i, cx| i.set_value("", window, cx));
        ai.update(cx, |ai, cx| ai.set_pending_ref(None, cx));

        if let Some((conn_id, table, detail)) = self.current_table(cx) {
            if conn_id == target.conn_id {
                self.details.insert(format!("{conn_id}:{table}"), detail);
            }
        }
        let conns = Stores::global(cx).conns.clone();
        let cached_tables = conns.read(cx).tables.get(&target.conn_id).cloned();
        let load = match cached_tables.filter(|t| !t.is_empty()) {
            Some(t) => Task::ready(t),
            None => conns.update(cx, |s, cx| s.load_tables(&target.conn_id, cx)),
        };
        self._send = Some(
            cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
                let tables = load.await;
                // 消息中命中的表:按需拉取结构
                let wanted: Vec<String> = this
                    .update(cx, |this, _| {
                        mentioned_tables(&final_text, &tables)
                            .into_iter()
                            .filter(|t| {
                                !this
                                    .details
                                    .contains_key(&format!("{}:{t}", target.conn_id))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let (kind, id) = (target.kind, target.conn_id.clone());
                let fetched: Vec<(String, TableDetail)> = runtime::backend(move |s| async move {
                    let mut out = vec![];
                    for t in wanted {
                        if let Ok(d) = service::table_detail(&s, &kind, &id, &t).await {
                            out.push((t, d));
                        }
                    }
                    out
                })
                .await;
                let _ = this.update(cx, |this, cx| {
                    for (t, d) in fetched {
                        this.details.insert(format!("{}:{t}", target.conn_id), d);
                    }
                    let prefix = format!("{}:", target.conn_id);
                    let details: BTreeMap<String, TableDetail> = this
                        .details
                        .iter()
                        .filter_map(|(k, d)| {
                            k.strip_prefix(&prefix).map(|t| (t.to_string(), d.clone()))
                        })
                        .collect();
                    let ai = Stores::global(cx).ai.clone();
                    // 知识库文档:现读现取,保证指南更新后下一条消息立即用上最新内容
                    let doc = ai.read(cx).knowledge();
                    let columns = Stores::global(cx)
                        .conns
                        .read(cx)
                        .columns
                        .get(&target.conn_id)
                        .cloned();
                    let prompt = build_system_prompt(&PromptContext {
                        dialect: Dialect::from_kind(&target.kind).unwrap_or(Dialect::Pg),
                        database: &target.label,
                        tables: &tables,
                        details: &details,
                        columns_brief: columns.as_ref(),
                        knowledge: Some((&doc.name, &doc.content)),
                    });
                    ai.update(cx, |ai, cx| ai.send(&key, prompt, final_text, cx));
                });
            }),
        );
        cx.notify();
    }

    /// SQL 卡片的执行入口:预切分逐条执行,任一条失败即中止;结果以隐藏消息回灌对话。
    pub fn execute_sql(&mut self, sql: String, cx: &mut Context<Self>) -> Task<(bool, String)> {
        let Some(key) = self.convo_key(cx) else {
            return Task::ready((false, t!("chat.not_bound").to_string()));
        };
        let Some(bound) = Stores::global(cx)
            .ai
            .read(cx)
            .convo(&key)
            .and_then(|c| c.bound.clone())
        else {
            return Task::ready((false, t!("chat.not_bound").to_string()));
        };
        let stmts = statements(&sql);
        let max_rows = Prefs::global(cx).max_rows;
        let (kind, id) = (bound.kind, bound.conn_id.clone());
        cx.spawn(async move |_, cx| {
            let run = runtime::backend(move |s| async move {
                let mut parts = vec![];
                let mut ok = true;
                let multi = stmts.len() > 1;
                for (i, stmt) in stmts.iter().enumerate() {
                    let label = if multi {
                        t!("chat.stmt_n", n = i + 1).to_string()
                    } else {
                        String::new()
                    };
                    match service::query(&s, &kind, &id, stmt, Some(max_rows)).await {
                        Ok(r) => {
                            let brief = if is_query(stmt) {
                                t!("chat.returned_rows", count = r.rows.len()).to_string()
                            } else {
                                format!(
                                    "{}{}",
                                    t!("chat.succeeded"),
                                    r.affected
                                        .filter(|n| *n > 0)
                                        .map(|n| t!("chat.affected_rows", count = n).to_string())
                                        .unwrap_or_default()
                                )
                            };
                            parts.push(if label.is_empty() {
                                brief
                            } else {
                                t!("chat.labeled", label = label, brief = brief).to_string()
                            });
                        }
                        Err(e) => {
                            let label = if label.is_empty() {
                                t!("chat.execution").to_string()
                            } else {
                                label
                            };
                            parts.push(
                                t!("chat.failed", label = label, error = e.display()).to_string(),
                            );
                            ok = false;
                            break;
                        }
                    }
                }
                (ok, parts.join(" · "))
            });
            let (ok, summary) = run.await;
            let _ = cx.update(|cx| {
                // 显式传会话 key:执行完成时用户可能已切到别的表,结果必须回灌到发起会话
                let ai = Stores::global(cx).ai.clone();
                ai.update(cx, |ai, cx| {
                    ai.append(
                        &key,
                        AiRole::User,
                        format!(
                            "{} {}\n{}{} {summary}",
                            t!("chat.executed_tag"),
                            truncate_sql(&sql),
                            t!("chat.result_label"),
                            if ok { "✓" } else { "✗" }
                        ),
                        true,
                        cx,
                    )
                });
                // DDL 落库后刷新侧边栏表列表
                let lower = sql.to_lowercase();
                if ok
                    && (lower.contains("create")
                        || lower.contains("drop")
                        || lower.contains("alter"))
                {
                    let conns = Stores::global(cx).conns.clone();
                    conns.update(cx, |s, cx| {
                        s.invalidate_erd(&bound.conn_id);
                        s.load_tables(&bound.conn_id, cx).detach();
                    });
                }
            });
            (ok, summary)
        })
    }

    pub fn insert_and_run(&mut self, sql: String, confirmed: bool, cx: &mut Context<Self>) {
        // 永远打到会话绑定的连接,而不是当前激活 tab 的连接
        let Some(conn_id) = self
            .convo_key(cx)
            .and_then(|k| {
                Stores::global(cx)
                    .ai
                    .read(cx)
                    .convo(&k)
                    .and_then(|c| c.bound.clone())
            })
            .map(|b| b.conn_id)
        else {
            cx.emit(ChatEvent::Toast(Toast::Error(
                t!("chat.not_bound").to_string().into(),
            )));
            return;
        };
        cx.emit(ChatEvent::InsertAndRun {
            sql,
            conn_id,
            confirmed,
        });
    }

    fn render_message(
        &mut self,
        msg: &crate::store::AiMessage,
        streaming: bool,
        bound: Option<&BoundConn>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let palette = *cx.palette();
        if msg.hidden {
            return div()
                .px_1()
                .text_xs()
                .text_color(palette.fg_faint)
                .child(msg.content.clone())
                .into_any_element();
        }
        if msg.role == AiRole::User {
            return h_flex()
                .justify_end()
                .child(
                    div()
                        .max_w(gpui_kit::relative(0.88))
                        .px_3()
                        .py_2()
                        .rounded(px(12.))
                        .rounded_br(px(4.))
                        .bg(cx.theme().primary)
                        .text_color(cx.theme().primary_foreground)
                        .text_sm()
                        .child(msg.content.clone()),
                )
                .into_any_element();
        }
        if msg.content.is_empty() && streaming {
            return h_flex()
                .gap_2()
                .px_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(ui::icon("loader-circle").xsmall().text_color(palette.ai))
                .child(t!("chat.thinking").to_string())
                .into_any_element();
        }
        let kind = bound.map(|b| b.kind).unwrap_or(DbKind::Pg);
        let label = bound.map(|b| b.label.clone()).unwrap_or_default();
        let panel = cx.entity().downgrade();
        let segments = parse_segments(&msg.content);
        v_flex()
            .gap_2()
            .children(segments.into_iter().enumerate().map(|(i, seg)| {
                match seg {
                    Segment::Text(text) => div()
                        .text_sm()
                        .child(TextView::markdown(
                            SharedString::from(format!("ai-{}-{i}", msg.id)),
                            text,
                        ))
                        .into_any_element(),
                    Segment::Sql(sql) => {
                        let key = (msg.id, i);
                        let card = match self.cards.get(&key) {
                            Some(card) => {
                                card.update(cx, |c, cx| c.sync(&sql, streaming, window, cx));
                                card.clone()
                            }
                            None => {
                                let card = cx.new(|cx| {
                                    SqlCard::new(
                                        panel.clone(),
                                        sql.clone(),
                                        kind,
                                        label.clone(),
                                        streaming,
                                        window,
                                        cx,
                                    )
                                });
                                self.cards.insert(key, card.clone());
                                card
                            }
                        };
                        card.into_any_element()
                    }
                }
            }))
            .into_any_element()
    }
}

impl ChatPanel {
    /// 输入框下方的模型切换:按服务商分组列出设置里勾选启用的模型,选中即写回全局配置。
    fn render_model_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ai = Stores::global(cx).ai.clone();
        let settings = ai.read(cx).settings.clone();
        let current = settings.current.clone().unwrap_or_default();
        let label = if current.model.is_empty() {
            t!("chat.select_model").to_string()
        } else {
            current.model.clone()
        };
        let tooltip = settings
            .provider(&current.provider_id)
            .map(|p| format!("{} · {}", p.name, t!("chat.switch_model")))
            .unwrap_or_else(|| t!("chat.switch_model").to_string());
        let panel = cx.entity().downgrade();
        Button::new("ai-model")
            .ghost()
            .xsmall()
            .icon(ui::icon("cpu"))
            .label(label)
            .dropdown_caret(true)
            .tooltip(tooltip)
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu: PopupMenu, _, _| {
                let mut menu = menu.min_w(px(240.)).max_h(px(400.)).scrollable(true);
                let groups: Vec<_> = settings
                    .providers
                    .iter()
                    .filter(|p| p.enabled && p.enabled_models().next().is_some())
                    .collect();
                for (gi, p) in groups.iter().enumerate() {
                    if gi > 0 {
                        menu = menu.separator();
                    }
                    menu = menu.label(p.name.clone());
                    for m in p.enabled_models() {
                        let (ai, pid, model) = (ai.clone(), p.id.clone(), m.to_string());
                        let checked = current.provider_id == p.id && current.model == m;
                        menu =
                            menu.item(PopupMenuItem::new(m.to_string()).checked(checked).on_click(
                                move |_, _, cx| {
                                    ai.update(cx, |ai, cx| ai.set_current(&pid, &model, cx))
                                },
                            ));
                    }
                }
                if !groups.is_empty() {
                    menu = menu.separator();
                }
                let panel = panel.clone();
                menu.item(
                    PopupMenuItem::new(t!("chat.manage_providers").to_string())
                        .icon(ui::icon("settings"))
                        .on_click(move |_, _, cx| {
                            let _ = panel.update(cx, |_, cx| cx.emit(ChatEvent::OpenSettings));
                        }),
                )
            })
    }
}

impl Render for ChatPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *cx.palette();
        let key = self.convo_key(cx);
        let ai = Stores::global(cx).ai.clone();
        let (status, convo, streaming_key, pending) = {
            let a = ai.read(cx);
            (
                a.status.clone(),
                key.as_ref().and_then(|k| a.convo(k).cloned()),
                a.streaming_key.clone(),
                a.pending_ref.clone(),
            )
        };
        let messages = convo
            .as_ref()
            .map(|c| c.messages.clone())
            .unwrap_or_default();
        let bound = convo.as_ref().and_then(|c| c.bound.clone());
        let streaming = streaming_key.is_some() && streaming_key == key;
        let busy_elsewhere = streaming_key.is_some() && streaming_key != key;
        let ready = status.as_ref().is_some_and(|s| s.ready);
        let target = self.target(cx);
        let active = Stores::global(cx).conns.read(cx).active().cloned();
        let mismatch = bound
            .as_ref()
            .zip(active.as_ref())
            .is_some_and(|(b, a)| b.conn_id != a.id);
        let can_send = self.can_send(cx);
        let has_input = !self.input.read(cx).value().trim().is_empty() || pending.is_some();
        if messages.len() != self.last_count || streaming {
            self.last_count = messages.len();
            self.scroll.scroll_to_bottom();
        }
        let last = messages.last().cloned();
        let show_fix = !streaming
            && last
                .as_ref()
                .is_some_and(|m| m.hidden && m.content.contains('✗'));
        let placeholder = if target.is_none() {
            t!("chat.placeholder_no_conn")
        } else if !ready {
            t!("chat.placeholder_not_ready")
        } else if busy_elsewhere {
            t!("chat.placeholder_busy")
        } else {
            t!("chat.placeholder_ask")
        };
        self.input.update(cx, |i, cx| {
            i.set_placeholder(placeholder.to_string(), window, cx)
        });

        let rendered: Vec<gpui_kit::AnyElement> = messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                self.render_message(
                    m,
                    streaming && i == messages.len() - 1,
                    bound.as_ref().or(target.as_ref()),
                    window,
                    cx,
                )
            })
            .collect();

        v_flex()
            .size_full()
            .bg(palette.panel)
            .border_l_1()
            // AI 专属紫:面板左缘带一点紫色调,与原版一致
            .border_color(palette.ai.opacity(0.28))
            .child(
                h_flex()
                    .h(px(40.))
                    .flex_shrink_0()
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(ui::icon("sparkles").small().text_color(palette.ai))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .whitespace_nowrap()
                            .child(t!("chat.title").to_string()),
                    )
                    .when_some(bound.clone(), |this, b| {
                        this.child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_xs()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_color(cx.theme().muted_foreground)
                                .child(b.label),
                        )
                    })
                    .child(div().flex_1())
                    .when(!messages.is_empty(), |this| {
                        let key = key.clone();
                        this.child(
                            Button::new("ai-clear")
                                .ghost()
                                .xsmall()
                                .icon(ui::icon("eraser"))
                                .label(t!("chat.clear").to_string())
                                .tooltip(t!("chat.clear_tooltip").to_string())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(k) = &key {
                                        let ai = Stores::global(cx).ai.clone();
                                        ai.update(cx, |ai, cx| ai.clear(k, cx));
                                    }
                                    this.details.clear();
                                    this.cards.clear();
                                })),
                        )
                    }),
            )
            .when(status.is_some() && !ready, |this| {
                this.child(
                    v_flex()
                        .px_3()
                        .py_2p5()
                        .gap_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(ui::hint(
                            status
                                .as_ref()
                                .and_then(|s| s.error.clone())
                                .unwrap_or_else(|| t!("chat.unavailable").to_string()),
                            cx,
                        ))
                        .child(
                            h_flex().child(
                                Button::new("ai-open-settings")
                                    .outline()
                                    .xsmall()
                                    .label(t!("chat.open_settings").to_string())
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(ChatEvent::OpenSettings)),
                                    ),
                            ),
                        ),
                )
            })
            .when(mismatch, |this| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            t!(
                                "chat.bound_notice",
                                label = bound.as_ref().map(|b| b.label.clone()).unwrap_or_default()
                            )
                            .to_string(),
                        ),
                )
            })
            .child(
                v_flex()
                    .id("ai-messages")
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .gap_3()
                    .overflow_y_scroll()
                    .when(messages.is_empty(), |this| {
                        this.justify_center().child(
                            v_flex()
                                .items_center()
                                .gap_1()
                                .px_4()
                                .text_sm()
                                .text_color(palette.fg_faint)
                                .child(t!("chat.empty_intro").to_string())
                                .child(t!("chat.empty_example_create").to_string())
                                .child(t!("chat.empty_example_seed").to_string())
                                .child(t!("chat.empty_confirm").to_string()),
                        )
                    })
                    .children(rendered)
                    .when(show_fix, |this| {
                        this.child(
                            h_flex().child(
                                Button::new("ai-fix")
                                    .outline()
                                    .xsmall()
                                    .label(t!("chat.fix").to_string())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.send_text(
                                            Some(t!("chat.fix_prompt").to_string()),
                                            window,
                                            cx,
                                        )
                                    })),
                            ),
                        )
                    }),
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .p_2p5()
                    .gap_1p5()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .when_some(pending.clone(), |this, sql| {
                        this.child(
                            h_flex()
                                .items_start()
                                .gap_1p5()
                                .px_2()
                                .py_1p5()
                                .rounded(px(8.))
                                .bg(palette.raised)
                                .border_1()
                                .border_color(cx.theme().border)
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_color(palette.fg_faint)
                                        .child(t!("chat.quote").to_string()),
                                )
                                .child(
                                    div()
                                        .id("pending-ref")
                                        .flex_1()
                                        .max_h(px(60.))
                                        .overflow_y_scroll()
                                        .text_xs()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_color(cx.theme().muted_foreground)
                                        .child(sql),
                                )
                                .child(
                                    Button::new("remove-ref")
                                        .ghost()
                                        .xsmall()
                                        .icon(ui::icon("x"))
                                        .tooltip(t!("chat.remove_quote").to_string())
                                        .on_click(|_, _, cx| {
                                            let ai = Stores::global(cx).ai.clone();
                                            ai.update(cx, |ai, cx| ai.set_pending_ref(None, cx));
                                        }),
                                ),
                        )
                    })
                    .child(
                        h_flex().child(
                            Button::new("quote-selection")
                                .ghost()
                                .xsmall()
                                .icon(ui::icon("quote"))
                                .label(t!("chat.quote_selection").to_string())
                                .tooltip(t!("chat.quote_selection_tooltip").to_string())
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(ChatEvent::QuoteSelection)),
                                ),
                        ),
                    )
                    .child(
                        h_flex()
                            .items_end()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(
                                Textarea::new(&self.input).disabled(!can_send && !streaming),
                            ))
                            .child(
                                Button::new("ai-send")
                                    .custom(
                                        ButtonCustomVariant::new(cx)
                                            .color(palette.ai)
                                            .foreground(palette.on_ai)
                                            .hover(palette.ai.opacity(0.85))
                                            .active(palette.ai.opacity(0.7)),
                                    )
                                    .rounded(px(999.))
                                    .small()
                                    .icon(ui::icon("arrow-up"))
                                    .tooltip(t!("chat.send_tooltip").to_string())
                                    .disabled(!can_send || !has_input)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.send(window, cx)),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(self.render_model_picker(cx))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(palette.fg_faint)
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(t!("chat.input_hint").to_string()),
                            ),
                    ),
            )
    }
}
