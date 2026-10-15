//! Mongo 结果的 JSON 视图:每篇文档一块,单击选中、双击编辑(集合 tab)。

use super::WorkTab;
use crate::theme::ActivePalette as _;
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div,
};

pub fn render(tab: &WorkTab, cx: &mut Context<WorkTab>) -> AnyElement {
    let Some(m) = tab.mongo.as_ref() else {
        return div().into_any_element();
    };
    let docs = m.docs.clone();
    let editable = m.coll.is_some();
    let view = tab.view.clone();
    let selected = tab.selected_row;
    let mono = cx.theme().mono_font_family.clone();
    v_flex()
        .id("doc-view")
        .size_full()
        .overflow_y_scroll()
        .p_3()
        .gap_2()
        .children(view.iter().enumerate().filter_map(|(vix, ri)| {
            let text = docs.get(*ri)?.clone();
            let active = selected == Some(vix);
            Some(
                div()
                    .id(("doc", vix))
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(if active {
                        cx.theme().primary
                    } else {
                        cx.theme().border
                    })
                    .bg(cx.palette().panel)
                    .font_family(mono.clone())
                    .text_xs()
                    .child(text)
                    .on_click(cx.listener(move |t, e: &ClickEvent, w, cx| {
                        t.selected_row = Some(vix);
                        if editable && e.click_count() >= 2 {
                            t.open_doc_editor(Some(vix), w, cx);
                        }
                        cx.notify();
                    })),
            )
        }))
        .into_any_element()
}
