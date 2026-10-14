//! SQL 编辑器:gpui-kit 的代码编辑器 + Datum 的 schema 感知补全。
//!
//! 补全的计算在 `datum_core::completion`(纯函数);这里只负责把结果转成编辑器的
//! LSP 补全项,以及在 schema 到达后更新补全上下文。

use datum_core::completion::{self, ItemKind};
use datum_core::core::connection::DbKind;
use datum_core::schema::ColumnsByTable;
use gpui_kit::component::input::{CompletionProvider, EditorState, Rope, RopeExt as _};
use gpui_kit::{App, AppContext as _, Entity, Result, Task, Window};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    TextEdit,
};
use rust_i18n::t;
use std::cell::RefCell;
use std::rc::Rc;

/// 补全用的 schema 快照:表名(Redis 为 key 名)与表→列。
#[derive(Default)]
pub struct SchemaSnapshot {
    pub tables: Vec<String>,
    pub columns: ColumnsByTable,
}

pub type SchemaHandle = Rc<RefCell<SchemaSnapshot>>;

struct SqlCompletions {
    kind: DbKind,
    schema: SchemaHandle,
}

fn lsp_kind(kind: ItemKind) -> CompletionItemKind {
    match kind {
        ItemKind::Keyword => CompletionItemKind::KEYWORD,
        ItemKind::Table => CompletionItemKind::CLASS,
        ItemKind::Column => CompletionItemKind::FIELD,
        ItemKind::Snippet => CompletionItemKind::SNIPPET,
        ItemKind::Command => CompletionItemKind::FUNCTION,
        ItemKind::Key => CompletionItemKind::CONSTANT,
    }
}

impl CompletionProvider for SqlCompletions {
    fn completions(
        &self,
        rope: &Rope,
        offset: usize,
        trigger: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let text = rope.to_string();
        let explicit = trigger.trigger_kind == lsp_types::CompletionTriggerKind::INVOKED;
        let schema = self.schema.borrow();
        let result = if self.kind == DbKind::Redis {
            completion::redis_completions(&text, offset, &schema.tables, explicit)
        } else {
            completion::sql_completions(&text, offset, &schema.tables, &schema.columns, explicit)
        };
        let Some(result) = result else {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        };
        let range = lsp_types::Range::new(
            rope.offset_to_position(result.replace.start),
            rope.offset_to_position(result.replace.end),
        );
        let items = result
            .items
            .into_iter()
            .enumerate()
            .map(|(i, item)| CompletionItem {
                label: item.label,
                kind: Some(lsp_kind(item.kind)),
                detail: (!item.detail.is_empty()).then_some(item.detail),
                // 保持计算出的顺序(模板 → 列 → 表 → 关键字)
                sort_text: Some(format!("{i:05}")),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range,
                    new_text: item.insert,
                })),
                ..Default::default()
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _: &mut App) -> bool {
        // 词字符、点号、空白(FROM 后立即弹表名)都可能触发;无候选时 completions 返回空
        new_text
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == ' ' || c == ':')
    }
}

/// 新建一个 SQL(或 Redis 命令)编辑器。
pub fn new_editor(
    kind: DbKind,
    window: &mut Window,
    cx: &mut App,
) -> (Entity<EditorState>, SchemaHandle) {
    let schema: SchemaHandle = Rc::new(RefCell::new(SchemaSnapshot::default()));
    let provider = Rc::new(SqlCompletions {
        kind,
        schema: schema.clone(),
    });
    let editor = cx.new(|cx| {
        let mut state = EditorState::new(window, cx)
            .language(if kind == DbKind::Redis { "text" } else { "sql" })
            .line_number(true)
            .soft_wrap(false)
            .placeholder(if kind == DbKind::Redis {
                t!("tab.editor_placeholder_redis")
            } else {
                t!("tab.editor_placeholder_sql")
            });
        state.lsp_mut().completion_provider = Some(provider);
        state
    });
    (editor, schema)
}
