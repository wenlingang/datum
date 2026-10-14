//! 单元格全文弹窗:查看完整值(JSON 自动格式化),有主键时可修改并暂存。

use super::WorkTab;
use crate::theme::ActivePalette as _;
use crate::ui;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, FontWeight, IntoElement, ParentElement as _,
    Render, Styled as _, Subscription, Task, WeakEntity, Window, div, px,
};
use rust_i18n::t;
use std::time::Duration;

/// 值能解析为 JSON 对象/数组时返回 2 空格缩进的格式化文本。
fn try_format_json(v: &str) -> Option<String> {
    let s = v.trim();
    if s.len() < 2 || !(s.starts_with('{') || s.starts_with('[')) {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(s).ok()?;
    (parsed.is_object() || parsed.is_array())
        .then(|| serde_json::to_string_pretty(&parsed).ok())
        .flatten()
}

struct CellEditor {
    tab: WeakEntity<WorkTab>,
    view_ix: usize,
    col_ix: usize,
    column: String,
    data_type: Option<String>,
    is_null: bool,
    editable: bool,
    editor: Entity<EditorState>,
    /// 打开/暂存后的基准值,用于判断是否有改动
    base: String,
    staged: bool,
    copied: bool,
    _reset_copied: Option<Task<()>>,
    _sub: Subscription,
}

impl CellEditor {
    fn draft(&self, cx: &gpui_kit::App) -> String {
        self.editor.read(cx).value().to_string()
    }

    fn stage(&mut self, cx: &mut Context<Self>) {
        let draft = self.draft(cx);
        if !self.editable || draft == self.base {
            return;
        }
        let (view_ix, col_ix) = (self.view_ix, self.col_ix);
        let value = draft.clone();
        let _ = self
            .tab
            .update(cx, |tab, cx| tab.stage_edit(view_ix, col_ix, value, cx));
        self.base = draft;
        self.staged = true;
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.draft(cx)));
        self.copied = true;
        self._reset_copied = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1600))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.copied = false;
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Render for CellEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let draft_len = self.draft(cx).chars().count();
        let dirty = self.draft(cx) != self.base;
        let palette = *cx.palette();
        let mono = cx.theme().mono_font_family.clone();
        v_flex()
            .w_full()
            .h(px(560.))
            .child(
                h_flex()
                    .gap_2()
                    .pb_2()
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.column.clone()),
                    )
                    .when_some(self.data_type.clone(), |this, t| {
                        this.child(
                            div()
                                .font_family(mono.clone())
                                .text_size(px(9.5))
                                .text_color(palette.fg_faint)
                                .child(t.to_uppercase()),
                        )
                    })
                    .when(self.is_null, |this| {
                        this.child(div().text_xs().text_color(palette.fg_faint).child("(NULL)"))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .overflow_hidden()
                    .child(
                        Editor::new(&self.editor)
                            .readonly(!self.editable)
                            .bordered(false)
                            .h(gpui_kit::relative(1.)),
                    ),
            )
            .child(
                h_flex()
                    .pt_3()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_family(mono)
                            .text_color(palette.fg_faint)
                            .child(t!("cell.chars", n = draft_len).to_string()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("cell-copy")
                                    .outline()
                                    .small()
                                    .icon(ui::icon(if self.copied { "check" } else { "copy" }))
                                    .label(if self.copied {
                                        t!("cell.copied")
                                    } else {
                                        t!("cell.copy")
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
                            )
                            .when(self.editable, |this| {
                                this.child(
                                    Button::new("cell-stage")
                                        .small()
                                        .label(if self.staged && !dirty {
                                            t!("cell.staged")
                                        } else {
                                            t!("cell.stage")
                                        })
                                        .when(dirty, |b| b.primary())
                                        .when(!dirty, |b| b.outline())
                                        .tooltip(t!("cell.stage_tooltip"))
                                        .on_click(cx.listener(|this, _, _, cx| this.stage(cx))),
                                )
                            }),
                    ),
            )
    }
}

#[allow(clippy::too_many_arguments)]
pub fn open(
    tab: WeakEntity<WorkTab>,
    view_ix: usize,
    col_ix: usize,
    column: String,
    data_type: Option<String>,
    value: Option<String>,
    editable: bool,
    window: &mut Window,
    cx: &mut Context<WorkTab>,
) {
    let is_null = value.is_none();
    let raw = value.unwrap_or_default();
    // 能解析成 JSON 的值默认以格式化形态展示;基准值同步取格式化文本,避免刚打开就判为已修改
    let initial = try_format_json(&raw).unwrap_or(raw);
    let base = initial.clone();
    let view = cx.new(|cx| {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("text")
                .line_number(false)
                .soft_wrap(true)
                .default_value(initial)
        });
        let sub = cx.subscribe(&editor, |this: &mut CellEditor, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                this.staged = false;
                cx.notify();
            }
        });
        CellEditor {
            tab,
            view_ix,
            col_ix,
            column,
            data_type,
            is_null,
            editable,
            editor,
            base,
            staged: false,
            copied: false,
            _reset_copied: None,
            _sub: sub,
        }
    });
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .w(px(880.))
            .title(if editable {
                t!("cell.edit_title").to_string()
            } else {
                t!("cell.view_title").to_string()
            })
            .child(view.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::try_format_json;

    #[test]
    fn formats_only_objects_and_arrays() {
        assert_eq!(
            try_format_json("{\"a\":1}").as_deref(),
            Some("{\n  \"a\": 1\n}")
        );
        assert_eq!(try_format_json("42"), None);
        assert_eq!(try_format_json("{bad"), None);
    }
}
