//! `EditorKeys`:在编辑器聚焦期间绑定应用自己的快捷键,压过 nsh-base `Editor` 自带的同名键位。
//!
//! 为什么要在 Rust 里绑:nsh 查快捷键时,TSX `keymap.bind` 的绑定与 Rust 组件里 `on_key_with`
//! 的绑定在两张表里,Rust 那张总排在后面(更「新」)。`Editor` 聚焦时在 Rust 里绑了
//! `mod+enter`(下方插入新行)等键,所以 TSX 怎么绑 ⌘↵ 都轮不到。这里同样用 `on_key_with`,
//! 且在 `active` 变为 true(编辑器聚焦之后)才绑,就成了最新的那条。

use nsh_core::keymap::When;
use nsh_core::prelude::*;

/// The props of [`EditorKeys`].
#[derive(Debug, Clone, PartialEq, Props)]
pub struct EditorKeysProps {
    /// 要接管的按键,nsh 的写法(`"mod+enter"`、`"mod+shift+f"`)。
    pub keys: Prop<Vec<SharedString>>,
    /// 为 true 时绑定(编辑器有焦点时);变为 false 或组件卸载时解绑。
    #[prop(default = false)]
    pub active: Prop<bool>,
}

/// The events of an [`EditorKeys`].
#[derive(Debug, Clone, PartialEq, Event)]
pub enum EditorKeysEvent {
    /// 按下了 `keys` 里的某个键:按写法原样给出(`"mod+enter"`)。
    #[args(keystroke)]
    Key(SharedString),
}

/// 不可见的组件:`active` 期间接管 `keys`,按下时报告 `onKey({ keystroke })`。
/// 放在编辑器旁边,`active` 跟随编辑器的焦点。
#[component]
pub fn EditorKeys(props: EditorKeysProps, cx: Scope<EditorKeysEvent>) -> impl IntoElement {
    effect(move || {
        if !props.active.get() {
            return;
        }
        for key in props.keys.get() {
            on_key_with(
                &key,
                KeyOptions {
                    when: When::Always,
                    repeat: false,
                    pass_through: false,
                },
                move |k| cx.emit(EditorKeysEvent::Key(SharedString::from(k.to_string()))),
            );
        }
    });
    view().absolute().w(px(0.0)).h(px(0.0))
}
