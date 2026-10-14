//! 划词浮动按钮:编辑器有选区时,在选区结束端浮出「发给 AI」。
//! 独立成小实体:编辑器每次变化(含光标闪烁)只重绘这个按钮,不牵动整个标签页。

use crate::actions::SendSelectionToAi;
use crate::theme::ActivePalette as _;
use crate::ui;
use gpui_kit::component::input::EditorState;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex};
use gpui_kit::{
    Bounds, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, Render, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, div, px,
};
use rust_i18n::t;
use std::cell::Cell;
use std::rc::Rc;

pub struct SelectionChip {
    editor: Entity<EditorState>,
    /// 编辑器容器在窗口中的位置(由容器的测量元素写入)
    pub origin: Rc<Cell<Bounds<Pixels>>>,
    _sub: Subscription,
}

impl SelectionChip {
    pub fn new(editor: Entity<EditorState>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&editor, |_, _, cx| cx.notify());
        Self {
            editor,
            origin: Rc::new(Cell::new(Bounds::default())),
            _sub: sub,
        }
    }
}

impl Render for SelectionChip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let e = self.editor.read(cx);
        let range = e.selected_range();
        if range.is_empty() || e.selected_text().to_string().trim().is_empty() {
            return div();
        }
        let Some(b) = e.range_to_bounds(&(range.end..range.end)) else {
            return div();
        };
        let origin = self.origin.get().origin;
        let left = (b.origin.x - origin.x).max(px(0.));
        let top = (b.origin.y - origin.y - px(30.)).max(px(0.));
        let palette = *cx.palette();
        div().absolute().left(left).top(top).child(
            h_flex()
                .id("send-selection")
                .h(px(24.))
                .px_2()
                .gap_1()
                .rounded(px(6.))
                .bg(palette.ai_tint)
                .bg(cx.theme().background)
                .border_1()
                .border_color(palette.ai)
                .shadow_sm()
                .cursor_pointer()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(palette.ai)
                // 保持编辑器焦点与选区:按下时不抢焦点
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .on_click(|_, window, cx| window.dispatch_action(Box::new(SendSelectionToAi), cx))
                .child(ui::icon("sparkles").xsmall())
                .child(t!("tab.send_to_ai_chip")),
        )
    }
}
