//! AI 助手:配置读写、连通性测试、流式对话,以及面板用到的纯计算
//! (system prompt 拼装、命中表名、回复分段、服务商预设)。

use crate::runtime::{from, json, spawn, state};
use datum_core::ai::chat::{ChatEvent, ChatMessage};
use datum_core::ai::commands as ai_cmd;
use datum_core::ai::markdown::{Segment, parse_segments};
use datum_core::ai::presets::PRESETS;
use datum_core::ai::prompt::{PromptContext, build_system_prompt_in, mentioned_tables};
use datum_core::ai::settings::{AiSettings, ProviderConfig};
use datum_core::core::connection::DbKind;
use datum_core::pg::browse::TableDetail;
use datum_core::schema::ColumnsByTable;
use datum_core::sql::Dialect;
use nsh_core::JsonValue;
use nsh_core::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// `Ai.systemPrompt` 的入参(TSX 侧收集的上下文)。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptInput {
    kind: DbKind,
    /// 对话绑定的连接标签(「连接名 / 库名」)
    database: String,
    tables: Vec<String>,
    #[serde(default)]
    details: BTreeMap<String, TableDetail>,
    #[serde(default)]
    columns: Option<ColumnsByTable>,
}

/// 按给定 locale 与知识库拼 system prompt(纯计算,便于测试)。
fn prompt_of(input: JsonValue, knowledge: (&str, &str), locale: &str) -> Result<String, String> {
    let p: PromptInput = from(input, "context")?;
    Ok(build_system_prompt_in(
        &PromptContext {
            dialect: Dialect::from_kind(&p.kind).unwrap_or(Dialect::Pg),
            database: &p.database,
            tables: &p.tables,
            details: &p.details,
            columns_brief: p.columns.as_ref(),
            knowledge: Some(knowledge),
        },
        locale,
    ))
}

/// 回复分段 → `[{ kind: "text" | "sql", text }]`。
fn segments_json(content: &str) -> JsonValue {
    JsonValue::Array(
        parse_segments(content)
            .into_iter()
            .map(|s| match s {
                Segment::Text(text) => serde_json::json!({ "kind": "text", "text": text }),
                Segment::Sql(text) => serde_json::json!({ "kind": "sql", "text": text }),
            })
            .collect(),
    )
}

/// AI 配置与对话。
#[module]
pub struct Ai;

#[module]
impl Ai {
    /// 当前配置状态(是否已配置、当前模型等)。
    pub fn status() -> Result<JsonValue, String> {
        json(ai_cmd::ai_status(&state()))
    }

    /// 整份 AI 配置(服务商、模型、当前选择)。
    pub fn settings() -> Result<JsonValue, String> {
        json(ai_cmd::ai_get_settings(&state()))
    }

    /// 保存整份配置;当前模型失效时自动改选第一个可用模型。返回规范化后的配置。
    pub fn save_settings(settings: JsonValue) -> Result<JsonValue, String> {
        let mut s: AiSettings = from(settings, "settings")?;
        s.normalize_current();
        ai_cmd::ai_save_settings(&state(), &s)?;
        json(s)
    }

    /// 用给定服务商与模型发一次最小请求,验证配置可用。
    pub async fn test_config(provider: JsonValue, model: String) -> Result<(), String> {
        let p: ProviderConfig = from(provider, "provider")?;
        spawn(ai_cmd::ai_test_config(p, model)).await?
    }

    /// 拉取服务商的模型列表。
    pub async fn list_models(provider: JsonValue) -> Result<JsonValue, String> {
        let p: ProviderConfig = from(provider, "provider")?;
        spawn(ai_cmd::ai_list_models(p)).await?.and_then(json)
    }

    /// 知识库文档(项目约定,拼进 system prompt)。
    pub fn knowledge() -> Result<JsonValue, String> {
        json(ai_cmd::ai_read_knowledge(&state()))
    }

    /// 拼 AI 对话的 system prompt:方言、库名、表清单、已加载的表结构、字段速览、知识库与输出规则。
    /// `context` 为 `{ kind, database, tables, details?: { 表名: TableDetail }, columns?: ColumnsByTable }`;
    /// 知识库在这里读取,提示词语言跟随界面语言。
    pub fn system_prompt(context: JsonValue) -> Result<String, String> {
        let doc = ai_cmd::ai_read_knowledge(&state());
        prompt_of(context, (&doc.name, &doc.content), &rust_i18n::locale())
    }

    /// 用户消息里提到的已存在表名(按需加载表结构),最多 10 张。
    pub fn mentioned_tables(text: String, tables: Vec<String>) -> Vec<String> {
        mentioned_tables(&text, &tables)
    }

    /// 把 assistant 回复切成文本段与 SQL 段:`[{ kind: "text" | "sql", text }]`。
    /// 流式中未闭合的结尾代码块同样识别。
    pub fn segments(content: String) -> JsonValue {
        segments_json(&content)
    }

    /// 常用服务商预设:`[{ name, displayName, protocol, baseUrl }]`(展示名按当前语言)。
    pub fn presets() -> JsonValue {
        JsonValue::Array(
            PRESETS
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "name": p.name,
                        "displayName": p.display_name(),
                        "protocol": p.protocol,
                        "baseUrl": p.base_url,
                    })
                })
                .collect(),
        )
    }

    /// 流式对话。`messages` 为 `[{ role, content }]`(含 system);
    /// `on_event` 依次收到 `{ type: "delta", text }`、`{ type: "done", full }` 或 `{ type: "error", message }`。
    /// 对话结束(成功或失败)后 Promise 才完成。
    pub async fn chat(
        messages: JsonValue,
        #[args(event)] on_event: Callback<JsonValue>,
    ) -> Result<(), String> {
        let wire: Vec<ChatMessage> = from(messages, "messages")?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
        let done = background(async move {
            let tx2 = tx.clone();
            let result = ai_cmd::ai_chat(&state(), wire, move |e| {
                let _ = tx2.send(e);
            })
            .await;
            if let Err(e) = result {
                let _ = tx.send(ChatEvent::Error { message: e });
            }
        });
        while let Some(event) = rx.recv().await {
            let payload = match event {
                ChatEvent::Delta { text } => serde_json::json!({ "type": "delta", "text": text }),
                ChatEvent::Done { full } => serde_json::json!({ "type": "done", "full": full }),
                ChatEvent::Error { message } => {
                    serde_json::json!({ "type": "error", "message": message })
                }
            };
            on_event.post(payload);
        }
        done.await.map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_split_text_and_sql() {
        let v = segments_json("建表:\n```sql\nCREATE TABLE t (id int);\n```\n好了");
        assert_eq!(
            v,
            serde_json::json!([
                { "kind": "text", "text": "建表:" },
                { "kind": "sql", "text": "CREATE TABLE t (id int);" },
                { "kind": "text", "text": "好了" },
            ])
        );
    }

    #[test]
    fn segments_keep_unclosed_sql_while_streaming() {
        let v = segments_json("```sql\nSELECT 1");
        assert_eq!(v, serde_json::json!([{ "kind": "sql", "text": "SELECT 1" }]));
    }

    #[test]
    fn prompt_includes_tables_details_and_knowledge() {
        let ctx = serde_json::json!({
            "kind": "mysql",
            "database": "local / shop",
            "tables": ["users", "orders"],
            "details": {
                "users": {
                    "columns": [{ "name": "id", "dataType": "int", "notNull": true, "isPk": true }],
                    "indexes": []
                }
            },
            "columns": { "orders": [{ "name": "total", "dataType": "decimal" }] }
        });
        let p = prompt_of(ctx, ("规范", "表名用复数"), "en").unwrap();
        assert!(p.contains("MySQL"), "{p}");
        assert!(p.contains("local / shop"));
        assert!(p.contains("users, orders"));
        assert!(p.contains("- id int"));
        assert!(p.contains("orders(total)"));
        assert!(p.contains("表名用复数"));
    }

    #[test]
    fn prompt_rejects_bad_context() {
        assert!(prompt_of(serde_json::json!({ "kind": "pg" }), ("", ""), "en").is_err());
    }

    #[test]
    fn mentioned_tables_matches_case_insensitively() {
        let tables = vec!["users".to_string(), "orders".to_string()];
        assert_eq!(mentioned_tables("count USERS", &tables), vec!["users".to_string()]);
    }
}
