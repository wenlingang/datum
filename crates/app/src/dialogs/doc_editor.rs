//! 文档编辑 / 新增弹窗:整篇文档以 mongosh 文本编辑,实时校验,保存时 replaceOne / insertOne。
//! 保存失败时弹窗保持打开,编辑内容不丢。

use crate::runtime;
use crate::store::Toast;
use crate::tab::{TabEvent, WorkTab};
use crate::theme::ActivePalette as _;
use datum_core::mongo::bson::{Bson, Document};
use datum_core::mongo::parse::parse_document;
use datum_core::service;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, AsyncWindowContext, Context, Entity, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Task, WeakEntity, Window, div, px,
};
use rust_i18n::t;

/// 编辑的目标:哪个 tab(保存后刷新)、哪个连接与集合。
pub struct DocTarget {
    pub tab: WeakEntity<WorkTab>,
    pub conn_id: String,
    pub coll: String,
}

/// 编辑后的文档 → 用于 replaceOne 的文档:删掉 `_id` 时补回原值;改了 `_id`(含改类型)时拒绝,
/// 绝不能让一次"编辑"变成插入另一篇文档。
pub fn prepare_replacement(original_id: &Bson, doc: Document) -> Result<Document, String> {
    match doc.get("_id") {
        None => {
            let mut out = Document::new();
            out.insert("_id", original_id.clone());
            out.extend(doc);
            Ok(out)
        }
        Some(id) if id == original_id => Ok(doc),
        Some(_) => Err(t!("mongo.id_changed").to_string()),
    }
}

struct DocEditor {
    target: DocTarget,
    original_id: Option<Bson>,
    editor: Entity<EditorState>,
    /// 当前文本的解析结果:只在内容变化时重算,不在每帧渲染时重新解析整篇文档
    parsed: Result<Document, String>,
    /// 保存失败或 `_id` 校验失败的提示;编辑后清除
    error: Option<String>,
    saving: bool,
    _task: Option<Task<()>>,
    _sub: Subscription,
}

fn parse_text(text: &str) -> Result<Document, String> {
    parse_document(text).map_err(|e| e.display())
}

impl DocEditor {
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let doc = match self.parsed.clone().and_then(|doc| match &self.original_id {
            Some(id) => prepare_replacement(id, doc),
            None => Ok(doc),
        }) {
            Ok(d) => d,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        let (id, coll, original) = (
            self.target.conn_id.clone(),
            self.target.coll.clone(),
            self.original_id.clone(),
        );
        let tab = self.target.tab.clone();
        self.saving = true;
        cx.notify();
        self._task = Some(
            cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
                let r = runtime::backend(move |s| async move {
                    match original {
                        Some(doc_id) => {
                            service::mongo_replace_one(&s, &id, &coll, doc_id, doc).await
                        }
                        None => service::mongo_insert_one(&s, &id, &coll, doc)
                            .await
                            .map(|_| ()),
                    }
                })
                .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.saving = false;
                    match r {
                        Ok(()) => {
                            window.close_dialog(cx);
                            let _ = tab.update(cx, |t, cx| {
                                cx.emit(TabEvent::Toast(Toast::Success(
                                    t!("mongo.saved").to_string().into(),
                                )));
                                t.refresh(window, cx);
                            });
                        }
                        Err(e) => {
                            this.error = Some(e.display());
                            cx.notify();
                        }
                    }
                });
            }),
        );
    }
}

impl Render for DocEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let parse_err = self.parsed.as_ref().err().cloned();
        let shown_err = self.error.clone().or(parse_err.clone());
        v_flex()
            .w_full()
            .h(px(560.))
            .gap_2()
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
                            .bordered(false)
                            .h(gpui_kit::relative(1.)),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .child(div().flex_1().min_w_0().text_xs().map(|d| {
                        match &shown_err {
                            Some(e) => d.text_color(cx.theme().danger).child(e.clone()),
                            None => d
                                .text_color(cx.palette().data)
                                .child(t!("mongo.doc_valid").to_string()),
                        }
                    }))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("doc-cancel")
                                    .ghost()
                                    .label(t!("mongo.cancel"))
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("doc-save")
                                    .primary()
                                    .label(t!("mongo.save"))
                                    .loading(self.saving)
                                    .disabled(parse_err.is_some())
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.save(window, cx)),
                                    ),
                            ),
                    ),
            )
    }
}

/// `original_id` 为 None 表示新增。
pub fn open(
    target: DocTarget,
    initial: String,
    original_id: Option<Bson>,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) {
    let editing = original_id.is_some();
    let parsed = parse_text(&initial);
    let view = cx.new(|cx| {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("javascript")
                .line_number(true)
                .soft_wrap(false)
                .default_value(initial)
        });
        let sub = cx.subscribe(
            &editor,
            |this: &mut DocEditor, editor, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    this.parsed = parse_text(&editor.read(cx).value());
                    this.error = None;
                    cx.notify();
                }
            },
        );
        DocEditor {
            target,
            original_id,
            editor,
            error: None,
            saving: false,
            _task: None,
            parsed,
            _sub: sub,
        }
    });
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .w(px(880.))
            .title(if editing {
                t!("mongo.edit_title").to_string()
            } else {
                t!("mongo.new_title").to_string()
            })
            .child(view.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::prepare_replacement;
    use datum_core::mongo::bson::{Bson, doc};

    #[test]
    fn missing_id_is_restored() {
        let out = prepare_replacement(&Bson::Int32(7), doc! { "a": 1 }).unwrap();
        assert_eq!(out, doc! { "_id": 7, "a": 1 });
    }

    #[test]
    fn same_id_passes_changed_id_is_rejected() {
        assert!(prepare_replacement(&Bson::Int32(7), doc! { "_id": 7, "a": 1 }).is_ok());
        assert!(prepare_replacement(&Bson::Int32(7), doc! { "_id": 8 }).is_err());
        // 类型不同也算改了 _id
        assert!(prepare_replacement(&Bson::Int32(7), doc! { "_id": 7_i64 }).is_err());
    }
}
