//! Anthropic Messages API:`POST {base}/v1/messages`,`x-api-key` 鉴权,
//! system 单独传,流式事件为 content_block_delta / message_stop / error。

use crate::ai::chat::{get_json, read_sse, send, split_system, ChatEvent, ChatMessage, Mode};
use crate::ai::provider::Endpoint;
use rust_i18n::t;

const API_VERSION: &str = "2023-06-01";
/// Messages API 必填。SQL 助手的单次回复远用不到上限,取一个各代模型都支持的值。
const MAX_TOKENS: u32 = 8192;

fn auth(req: reqwest::RequestBuilder, ep: &Endpoint) -> reqwest::RequestBuilder {
    req.header("x-api-key", &ep.api_key)
        .header("anthropic-version", API_VERSION)
}

pub(crate) async fn stream(
    client: &reqwest::Client,
    ep: &Endpoint,
    messages: &[ChatMessage],
    mode: Mode,
    on_event: &(dyn Fn(ChatEvent) + Send + Sync),
) -> Result<String, String> {
    let (system, turns) = split_system(messages);
    let wire: Vec<_> = turns
        .into_iter()
        .map(|(is_user, content)| {
            serde_json::json!({
                "role": if is_user { "user" } else { "assistant" },
                "content": content,
            })
        })
        .collect();
    let mut body = serde_json::json!({
        "model": ep.model,
        "max_tokens": MAX_TOKENS,
        "messages": wire,
        "stream": true,
    });
    if !system.is_empty() {
        body["system"] = system.into();
    }
    let resp =
        send(auth(client.post(format!("{}/v1/messages", ep.base_url)), ep).json(&body)).await?;

    let mut full = String::new();
    read_sse(resp, |data| match parse_event(data) {
        Some(Event::Delta(t)) => {
            full.push_str(&t);
            on_event(ChatEvent::Delta { text: t });
            Ok(mode == Mode::Probe)
        }
        Some(Event::Stop) => Ok(true),
        Some(Event::Error(e)) => Err(e),
        None => Ok(false),
    })
    .await?;
    Ok(full)
}

pub(crate) async fn list_models(
    client: &reqwest::Client,
    ep: &Endpoint,
) -> Result<Vec<String>, String> {
    let v = get_json(auth(
        client.get(format!("{}/v1/models?limit=1000", ep.base_url)),
        ep,
    ))
    .await?;
    v.get("data")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("id")?.as_str().map(str::to_string))
                .collect()
        })
        .ok_or_else(|| t!("ai.err.bad_model_list").to_string())
}

#[derive(Debug, PartialEq)]
enum Event {
    Delta(String),
    Stop,
    Error(String),
}

/// 只关心文本增量、结束与错误;message_start、ping、思考块等其它事件忽略。
fn parse_event(data: &str) -> Option<Event> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    match v.get("type")?.as_str()? {
        "content_block_delta" => {
            let d = v.get("delta")?;
            (d.get("type")?.as_str()? == "text_delta")
                .then(|| d.get("text")?.as_str().map(str::to_string))
                .flatten()
                .filter(|t| !t.is_empty())
                .map(Event::Delta)
        }
        "message_stop" => Some(Event::Stop),
        "error" => {
            let unknown = t!("ai.err.unknown");
            let message = v
                .pointer("/error/message")
                .and_then(|m| m.as_str())
                .unwrap_or(&unknown);
            Some(Event::Error(
                t!("ai.err.service_error", message = message).to_string(),
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_delta() {
        let d = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"SELECT"}}"#;
        assert_eq!(parse_event(d), Some(Event::Delta("SELECT".into())));
    }

    #[test]
    fn ignores_thinking_and_bookkeeping_events() {
        assert_eq!(
            parse_event(
                r#"{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"…"}}"#
            ),
            None
        );
        assert_eq!(
            parse_event(r#"{"type":"message_start","message":{}}"#),
            None
        );
        assert_eq!(parse_event(r#"{"type":"ping"}"#), None);
    }

    #[test]
    fn stop_and_error_events() {
        assert_eq!(parse_event(r#"{"type":"message_stop"}"#), Some(Event::Stop));
        let e = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        // 前缀文案随全局 locale 变化,只断言透传的服务端 message
        assert!(matches!(parse_event(e), Some(Event::Error(m)) if m.ends_with("Overloaded")),);
    }
}
