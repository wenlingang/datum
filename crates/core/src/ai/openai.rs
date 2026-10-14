//! OpenAI Chat Completions(及兼容端点):`POST {base}/chat/completions`,Bearer 鉴权。

use crate::ai::chat::{
    debug_log, get_json, read_sse, send, truncate, ChatEvent, ChatMessage, Mode,
};
use crate::ai::provider::Endpoint;
use rust_i18n::t;
use serde::Deserialize;

pub(crate) async fn stream(
    client: &reqwest::Client,
    ep: &Endpoint,
    messages: &[ChatMessage],
    mode: Mode,
    on_event: &(dyn Fn(ChatEvent) + Send + Sync),
) -> Result<String, String> {
    let resp = send(
        client
            .post(format!("{}/chat/completions", ep.base_url))
            .bearer_auth(&ep.api_key)
            .json(&serde_json::json!({
                "model": ep.model,
                "messages": messages,
                "stream": true,
            })),
    )
    .await?;

    let mut full = String::new();
    let mut done = false;
    let raw = read_sse(resp, |data| {
        match parse_frame(data) {
            Some(Frame::Done) => done = true,
            Some(Frame::Delta(t)) => {
                full.push_str(&t);
                on_event(ChatEvent::Delta { text: t });
            }
            None => {}
        }
        Ok(done || (mode == Mode::Probe && !full.is_empty()))
    })
    .await?;
    if !full.is_empty() || done {
        return Ok(full);
    }
    // SSE 一帧都没解析出来:网关可能不支持 stream:true,把整个响应体按非流式 JSON 兜底。
    let body = String::from_utf8_lossy(&raw);
    debug_log!(
        "[ai_chat] stream ended empty, head={}",
        truncate(&body, 200)
    );
    match parse_non_stream_body(&body) {
        Some(text) => {
            on_event(ChatEvent::Delta { text: text.clone() });
            Ok(text)
        }
        None => Err(t!("ai.err.unparsable_response", body = truncate(&body, 300)).to_string()),
    }
}

pub(crate) async fn list_models(
    client: &reqwest::Client,
    ep: &Endpoint,
) -> Result<Vec<String>, String> {
    let v = get_json(
        client
            .get(format!("{}/models", ep.base_url))
            .bearer_auth(&ep.api_key),
    )
    .await?;
    parse_models(&v).ok_or_else(|| t!("ai.err.bad_model_list").to_string())
}

/// `{"data":[{"id":..}]}`
fn parse_models(v: &serde_json::Value) -> Option<Vec<String>> {
    Some(
        v.get("data")?
            .as_array()?
            .iter()
            .filter_map(|m| m.get("id")?.as_str().map(str::to_string))
            .collect(),
    )
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
}

#[derive(Deserialize, Default)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
}

enum Frame {
    Delta(String),
    Done,
}

/// data 负载 → 增量文本;解析不出 delta.content 的帧(usage、role 帧)返回 None。
fn parse_frame(data: &str) -> Option<Frame> {
    if data == "[DONE]" {
        return Some(Frame::Done);
    }
    let chunk: StreamChunk = serde_json::from_str(data).ok()?;
    let content = chunk.choices.into_iter().next()?.delta.content?;
    (!content.is_empty()).then_some(Frame::Delta(content))
}

/// 非流式响应兜底:choices[0].message.content。
fn parse_non_stream_body(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    let content = v
        .get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()?;
    (!content.is_empty()).then(|| content.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::sse_data;

    fn delta(line: &str) -> Option<String> {
        match parse_frame(sse_data(line)?) {
            Some(Frame::Delta(t)) => Some(t),
            _ => None,
        }
    }

    #[test]
    fn parses_delta_frame() {
        let line = r#"data: {"id":"x","choices":[{"index":0,"delta":{"content":"SELECT"}}]}"#;
        assert_eq!(delta(line).as_deref(), Some("SELECT"));
    }

    #[test]
    fn done_marker_detected() {
        assert!(matches!(parse_frame("[DONE]"), Some(Frame::Done)));
    }

    /// role 帧、usage 帧都不产出增量。
    #[test]
    fn non_content_frames_ignored() {
        assert!(delta(r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#).is_none());
        assert!(delta(r#"data: {"choices":[],"usage":{"total_tokens":10}}"#).is_none());
    }

    #[test]
    fn non_stream_fallback() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"hi"}}]}"#;
        assert_eq!(parse_non_stream_body(body).as_deref(), Some("hi"));
        assert!(parse_non_stream_body("<html>").is_none());
    }

    #[test]
    fn parses_model_list() {
        let v = serde_json::json!({"data":[{"id":"gpt-4o"},{"id":"DeepSeek-V3"}]});
        assert_eq!(parse_models(&v).unwrap(), vec!["gpt-4o", "DeepSeek-V3"]);
        assert!(parse_models(&serde_json::json!({"models":[]})).is_none());
    }
}
