//! AI 回复中的 SQL 卡片:语法高亮 + 可编辑(执行前可微调)+ 一键格式化。
//! 执行永远先弹确认(展示完整 SQL 与目标连接),破坏性语句红色警示。

use super::ChatPanel;
use crate::dialogs::risk::sql_view;
use crate::theme::ActivePalette as _;
use crate::ui;
use datum_core::core::connection::DbKind;
use datum_core::sql::Dialect;
use datum_core::sql::format::format_sql;
use datum_core::sql::risk::{analyze_sql, is_read_only};
use datum_core::sql::stmt::statements;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, AsyncWindowContext, ClipboardItem, Context, Entity, FontWeight, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, WeakEntity, Window, div, px,
};
use rust_i18n::t;

#[derive(Clone, PartialEq)]
pub enum CardState {
    Idle,
    Running,
    Done(String),
    Failed(String),
}

pub struct SqlCard {
    panel: WeakEntity<ChatPanel>,
    /// AI 生成的原文
    original: String,
    editor: Entity<EditorState>,
    pub streaming: bool,
    dialect: Dialect,
    kind: DbKind,
    target_label: String,
    state: CardState,
    /// 上一帧编辑器区域的宽度,用于估算折行
    width: std::rc::Rc<std::cell::Cell<f32>>,
    _sub: Subscription,
}

/// 内容高度自适应,超过约 14 行后内部滚动。
const MAX_LINES: usize = 14;
/// 编辑器行高与行号栏 + 左右内边距的宽度(实测)
const LINE_H: f32 = 20.;
const GUTTER_W: f32 = 70.;

/// 自动折行后的显示行数。编辑器没有公开折行结果,这里按它的规则模拟:
/// 等宽字符(中日韩等全角算两格)、按单词折行,单个超长单词再按列硬切。
fn visual_rows(text: &str, width: f32, char_w: f32) -> usize {
    let cols = ((width - GUTTER_W) / char_w).floor().max(8.) as usize;
    let cell = |c: char| if c as u32 >= 0x2e80 { 2 } else { 1 };
    text.lines()
        .map(|line| {
            let (mut rows, mut used) = (1, 0);
            // 词带上后面的空格一起放:空格可以悬在行尾,不额外占行
            for word in line.split_inclusive(' ') {
                let w: usize = word.trim_end().chars().map(cell).sum();
                let full: usize = word.chars().map(cell).sum();
                if used > 0 && used + w > cols {
                    rows += 1;
                    used = 0;
                }
                if w > cols {
                    rows += (used + w - 1) / cols;
                    used = (used + w) % cols;
                } else {
                    used += full;
                }
            }
            rows
        })
        .sum::<usize>()
        .max(1)
}

impl SqlCard {
    pub fn new(
        panel: WeakEntity<ChatPanel>,
        sql: String,
        kind: DbKind,
        target_label: String,
        streaming: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let original = sql.clone();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .soft_wrap(true)
                .default_value(sql)
        });
        let sub = cx.subscribe(&editor, |_, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            panel,
            original,
            editor,
            streaming,
            dialect: Dialect::from_kind(&kind).unwrap_or(Dialect::Pg),
            kind,
            target_label,
            state: CardState::Idle,
            width: std::rc::Rc::new(std::cell::Cell::new(320.)),
            _sub: sub,
        }
    }

    /// 流式期间持续跟随增量;消息结束后冻结,后续用户的编辑不再被覆盖。
    pub fn sync(
        &mut self,
        sql: &str,
        streaming: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.streaming {
            return;
        }
        if sql != self.original {
            self.original = sql.to_string();
            let s = sql.to_string();
            self.editor.update(cx, |e, cx| e.set_value(s, window, cx));
        }
        self.streaming = streaming;
        cx.notify();
    }

    fn value(&self, cx: &gpui_kit::App) -> String {
        self.editor.read(cx).value().to_string()
    }

    fn confirm(&mut self, insert: bool, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.value(cx);
        let stmts = statements(&value).len();
        let destructive = !analyze_sql(&value).is_empty();
        let view = sql_view(&value, Some(self.dialect), window, cx);
        let char_w = f32::from(cx.theme().mono_font_size) * 0.6;
        let lines = visual_rows(&value, 510., char_w).clamp(1, 16);
        let target = self.target_label.clone();
        let this = cx.entity().downgrade();
        let what = if stmts > 1 {
            t!("sql_card.n_statements", count = stmts).to_string()
        } else {
            t!("sql_card.statement").to_string()
        };
        let body = if insert {
            t!("sql_card.confirm_body_insert", target = target, what = what).to_string()
        } else {
            t!("sql_card.confirm_body", target = target, what = what).to_string()
        };
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let this = this.clone();
            let value = value.clone();
            alert
                .width(px(560.))
                .title(
                    if insert {
                        t!("sql_card.run_in_editor")
                    } else {
                        t!("sql_card.run_sql")
                    }
                    .to_string(),
                )
                .description(
                    v_flex()
                        .gap_3()
                        .child(div().text_sm().child(body.clone()))
                        .child(
                            div()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().border)
                                .overflow_hidden()
                                .child(
                                    Editor::new(&view)
                                        .readonly(true)
                                        .bordered(false)
                                        .h(px(lines as f32 * LINE_H + 16.)),
                                ),
                        )
                        .when(destructive, |this| {
                            this.child(
                                h_flex()
                                    .gap_1p5()
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(cx.theme().danger)
                                    .child(ui::icon("triangle-alert").small())
                                    .child(t!("sql_card.destructive_warning").to_string()),
                            )
                        }),
                )
                .show_cancel(true)
                .cancel_text(t!("sql_card.cancel").to_string())
                .ok_text(
                    if insert {
                        t!("sql_card.confirm_run_in_editor")
                    } else {
                        t!("sql_card.confirm_run")
                    }
                    .to_string(),
                )
                .ok_variant(if destructive {
                    ButtonVariant::Danger
                } else {
                    ButtonVariant::Primary
                })
                .on_ok(move |_, window, cx| {
                    let value = value.clone();
                    let _ = this.update(cx, |card, cx| {
                        if insert {
                            card.insert_and_run(value, true, cx);
                        } else {
                            card.execute(value, window, cx);
                        }
                    });
                    true
                })
        });
    }

    fn execute(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        self.state = CardState::Running;
        cx.notify();
        let Some(task) = self.panel.update(cx, |p, cx| p.execute_sql(sql, cx)).ok() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let (ok, summary) = task.await;
            let _ = this.update(cx, |card, cx| {
                card.state = if ok {
                    CardState::Done(summary)
                } else {
                    CardState::Failed(summary)
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn insert_and_run(&mut self, sql: String, confirmed: bool, cx: &mut Context<Self>) {
        let _ = self
            .panel
            .update(cx, |p, cx| p.insert_and_run(sql, confirmed, cx));
    }

    fn click_insert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.value(cx);
        // 只有确定是纯只读的语句才直接替换并运行;其余(含风险检测识别不出的写法)一律先确认
        if is_read_only(&value) {
            self.insert_and_run(value, false, cx);
        } else {
            self.confirm(true, window, cx);
        }
    }
}

impl Render for SqlCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *cx.palette();
        let value = self.value(cx);
        let dirty = value != self.original;
        let stmts = statements(&value).len();
        let destructive = !analyze_sql(&value).is_empty();
        let executed = matches!(self.state, CardState::Done(_));
        let running = self.state == CardState::Running;
        let char_w = f32::from(cx.theme().mono_font_size) * 0.6;
        let lines = visual_rows(&value, self.width.get(), char_w).clamp(1, MAX_LINES);
        let width = self.width.clone();

        v_flex()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(palette.ai)
            .bg(palette.ai_tint)
            .overflow_hidden()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_1p5()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(ui::icon("sparkles").xsmall().text_color(palette.ai))
                    .child(
                        ui::db_icon(self.kind)
                            .size(px(13.))
                            .text_color(palette.fg_faint),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(palette.ai)
                            .child(if stmts > 1 {
                                format!(
                                    "{} · {}",
                                    t!("sql_card.ai_generated"),
                                    t!("sql_card.n_statements", count = stmts)
                                )
                            } else {
                                t!("sql_card.ai_generated").to_string()
                            }),
                    )
                    .when(dirty, |this| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .text_xs()
                                .text_color(palette.data)
                                .child(ui::icon("pencil").xsmall())
                                .child(t!("sql_card.edited").to_string()),
                        )
                    })
                    .child(div().flex_1())
                    .when(dirty, |this| {
                        this.child(
                            Button::new("card-revert")
                                .ghost()
                                .xsmall()
                                .icon(ui::icon("rotate-ccw"))
                                .label(t!("sql_card.revert").to_string())
                                .tooltip(t!("sql_card.revert_tooltip").to_string())
                                .on_click(cx.listener(|card, _, window, cx| {
                                    let original = card.original.clone();
                                    card.editor
                                        .update(cx, |e, cx| e.set_value(original, window, cx));
                                })),
                        )
                    })
                    .when(!self.streaming, |this| {
                        this.child(
                            Button::new("card-format")
                                .ghost()
                                .xsmall()
                                .icon(ui::icon("text-align-start"))
                                .label(t!("sql_card.format").to_string())
                                .tooltip(t!("sql_card.format_tooltip").to_string())
                                .on_click(cx.listener(|card, _, window, cx| {
                                    let formatted = format_sql(&card.value(cx));
                                    card.editor
                                        .update(cx, |e, cx| e.replace_all(formatted, window, cx));
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .h(px(lines as f32 * LINE_H + 14.))
                    .bg(palette.editor)
                    // 宽度变了(面板拖宽/收窄)就记下并重绘,下一帧按新宽度重算折行
                    .relative()
                    .child(
                        gpui_kit::canvas(
                            move |bounds, window, _| {
                                let w = f32::from(bounds.size.width);
                                if (w - width.get()).abs() > 1. {
                                    width.set(w);
                                    window.refresh();
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(
                        Editor::new(&self.editor)
                            .bordered(false)
                            .readonly(self.streaming)
                            .h(gpui_kit::relative(1.)),
                    ),
            )
            .child(
                h_flex()
                    .px_2()
                    .py_1p5()
                    .gap_1p5()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("card-exec")
                            .xsmall()
                            .icon(ui::icon("play"))
                            .label(
                                match self.state {
                                    CardState::Running => t!("sql_card.running"),
                                    CardState::Done(_) => t!("sql_card.executed"),
                                    _ => t!("sql_card.run"),
                                }
                                .to_string(),
                            )
                            .map(|b| if destructive { b.danger() } else { b.primary() })
                            .disabled(executed || running || self.streaming)
                            .tooltip(
                                if destructive {
                                    t!("sql_card.run_tooltip_destructive")
                                } else {
                                    t!("sql_card.run_tooltip")
                                }
                                .to_string(),
                            )
                            .on_click(
                                cx.listener(|card, _, window, cx| card.confirm(false, window, cx)),
                            ),
                    )
                    .child(
                        Button::new("card-insert")
                            .ghost()
                            .xsmall()
                            .icon(ui::icon("square-play"))
                            .label(t!("sql_card.run_in_editor").to_string())
                            .disabled(self.streaming)
                            .tooltip(t!("sql_card.run_in_editor_tooltip").to_string())
                            .on_click(
                                cx.listener(|card, _, window, cx| card.click_insert(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("card-copy")
                            .ghost()
                            .xsmall()
                            .icon(ui::icon("copy"))
                            .label(t!("sql_card.copy").to_string())
                            .on_click(cx.listener(|card, _, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(card.value(cx)));
                                crate::workspace::show_toast(
                                    &crate::store::Toast::Success(
                                        t!("sql_card.copied").to_string().into(),
                                    ),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .when(destructive && self.state == CardState::Idle, |this| {
                        this.child(div().flex_1()).child(
                            h_flex()
                                .gap_1()
                                .text_xs()
                                .text_color(palette.negative)
                                .child(ui::icon("triangle-alert").xsmall())
                                .child(t!("sql_card.destructive").to_string()),
                        )
                    }),
            )
            .map(|this| match &self.state {
                CardState::Done(summary) => this.child(
                    h_flex()
                        .px_2p5()
                        .py_1p5()
                        .gap_1p5()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .text_color(palette.data)
                        .child(ui::icon("check").xsmall())
                        .child(summary.clone()),
                ),
                CardState::Failed(e) => this.child(
                    div()
                        .px_2p5()
                        .py_1p5()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(palette.negative)
                        .child(e.clone()),
                ),
                _ => this,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::visual_rows;

    #[test]
    fn counts_wrapped_rows() {
        // 可用 (230-70)/8 = 20 列
        assert_eq!(visual_rows("SELECT 1", 230., 8.), 1);
        // 恰好 20 列放得下
        assert_eq!(visual_rows("WHERE role = 'admin'", 230., 8.), 1);
        // 按单词折:第三个词放不下整体换行
        assert_eq!(visual_rows("SELECT id, role, created_at", 230., 8.), 2);
        // 超长单词硬切:41 个字符 → 3 行
        assert_eq!(visual_rows(&"x".repeat(41), 230., 8.), 3);
        // 全角字符占两格:12 个汉字 = 24 格 → 2 行
        assert_eq!(visual_rows(&"中".repeat(12), 230., 8.), 2);
        assert_eq!(visual_rows("a\n\nb", 230., 8.), 3);
        assert_eq!(visual_rows("", 230., 8.), 1);
    }
}
