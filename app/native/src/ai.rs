//! AI 助手:配置读写、连通性测试、流式对话。

use crate::runtime::{from, json, spawn, state};
use datum_core::ai::chat::{ChatEvent, ChatMessage};
use datum_core::ai::commands as ai_cmd;
use datum_core::ai::settings::{AiSettings, ProviderConfig};
use nsh_core::JsonValue;
use nsh_core::prelude::*;

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
