//! AI 对话请求:按服务商协议分派到 openai / anthropic / gemini,统一成同一套流式事件。
//!
//! 流式增量经回调推给 UI;prompt 拼装(system + schema 上下文)在上层完成,这里只做转发。

use crate::ai::provider::Endpoint;
use crate::ai::settings::Protocol;
use crate::ai::{anthropic, gemini, openai};
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 整个流式响应的上限:长回复也应在几分钟内完成。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// 「测试」与拉取模型列表用的短超时:只是验证连通性,不该让用户等一分钟。
const SHORT_TIMEOUT: Duration = Duration::from_secs(15);

/// 仅 debug 构建输出的诊断日志:release 里完全静默,避免把请求状态/响应体预览
/// (可能带 system prompt 回显片段,如表结构或知识库文本)持续写到用户的 stderr。
macro_rules! debug_log {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        eprintln!($($arg)*);
    };
}
pub(crate) use debug_log;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// 推给前端的流式事件。Done 携带全文兜底:即使个别 Delta 丢失,
/// 前端也能用 full 校正最终文本。
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChatEvent {
    Delta { text: String },
    Done { full: String },
    Error { message: String },
}

/// 一次请求的模式:正常对话,或「测试」——收到第一段内容就算通过、立即断开。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Chat,
    Probe,
}

pub async fn stream_chat(
    ep: &Endpoint,
    messages: &[ChatMessage],
    on_event: &(dyn Fn(ChatEvent) + Send + Sync),
) -> Result<(), String> {
    let client = http(REQUEST_TIMEOUT)?;
    debug_log!(
        "[ai_chat] {:?} {} model={} msgs={}",
        ep.protocol,
        ep.base_url,
        ep.model,
        messages.len()
    );
    let full = dispatch(&client, ep, messages, Mode::Chat, on_event).await?;
    on_event(ChatEvent::Done { full });
    Ok(())
}

/// 设置页「测试」:用当前模型发一句 hi,收到首段回复即判定可用。
/// 不用 max_tokens=1 之类的限制——推理类模型(o 系列、gpt-5)不接受该参数。
pub async fn test_connection(ep: &Endpoint) -> Result<(), String> {
    let client = http(SHORT_TIMEOUT)?;
    let hi = [ChatMessage {
        role: "user".into(),
        content: "hi".into(),
    }];
    dispatch(&client, ep, &hi, Mode::Probe, &|_| {})
        .await
        .map(|_| ())
}

/// 列出服务商可用的对话模型(已过滤向量/重排/语音等,按名称排序)。
pub async fn list_models(ep: &Endpoint) -> Result<Vec<String>, String> {
    let client = http(SHORT_TIMEOUT)?;
    let mut ids = match ep.protocol {
        Protocol::OpenAi => openai::list_models(&client, ep).await?,
        Protocol::Anthropic => anthropic::list_models(&client, ep).await?,
        Protocol::Gemini => gemini::list_models(&client, ep).await?,
    };
    ids.retain(|id| is_chat_model(id));
    ids.sort_by_key(|id| id.to_ascii_lowercase());
    ids.dedup();
    if ids.is_empty() {
        return Err(t!("ai.err.no_chat_models").to_string());
    }
    Ok(ids)
}

async fn dispatch(
    client: &reqwest::Client,
    ep: &Endpoint,
    messages: &[ChatMessage],
    mode: Mode,
    on_event: &(dyn Fn(ChatEvent) + Send + Sync),
) -> Result<String, String> {
    match ep.protocol {
        Protocol::OpenAi => openai::stream(client, ep, messages, mode, on_event).await,
        Protocol::Anthropic => anthropic::stream(client, ep, messages, mode, on_event).await,
        Protocol::Gemini => gemini::stream(client, ep, messages, mode, on_event).await,
    }
}

fn is_chat_model(id: &str) -> bool {
    let l = id.to_ascii_lowercase();
    ![
        "embedding",
        "embed",
        "rerank",
        "whisper",
        "realtime",
        "tts",
        "dall-e",
        "moderation",
        "imagen",
        "aqa",
    ]
    .iter()
    .any(|k| l.contains(k))
}

fn http(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// 发请求并把非 2xx 映射成可读错误。
pub(crate) async fn send(req: reqwest::RequestBuilder) -> Result<reqwest::Response, String> {
    let resp = req.send().await.map_err(|e| {
        debug_log!("[ai] request error: {e}");
        t!("ai.err.request_failed", error = e).to_string()
    })?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(map_error_status(status, &body));
    }
    Ok(resp)
}

/// GET 并解析 JSON(模型列表用)。
pub(crate) async fn get_json(req: reqwest::RequestBuilder) -> Result<serde_json::Value, String> {
    let body = send(req)
        .await?
        .text()
        .await
        .map_err(|e| t!("ai.err.read_response", error = e).to_string())?;
    serde_json::from_str(body.trim())
        .map_err(|_| t!("ai.err.not_json", body = truncate(&body, 200)).to_string())
}

/// HTTP 状态 → 用户可读错误。三家的错误体都是 `{"error":{"message":..}}`,能取到就只显示 message。
fn map_error_status(status: reqwest::StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body.trim())
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| truncate(body.trim(), 300).to_string());
    match status.as_u16() {
        401 | 403 => t!("ai.err.unauthorized", detail = truncate(&detail, 200)).to_string(),
        404 => t!("ai.err.not_found", detail = truncate(&detail, 200)).to_string(),
        429 => t!("ai.err.rate_limited").to_string(),
        code => t!("ai.err.status", code = code, detail = detail).to_string(),
    }
}

/// 逐行读 SSE,把每个 `data:` 负载交给 `on_data`;它返回 true 表示可以结束。
/// 返回原始响应体:有的网关无视 stream:true 直接回整段 JSON,调用方据此做非流式兜底。
pub(crate) async fn read_sse(
    mut resp: reqwest::Response,
    mut on_data: impl FnMut(&str) -> Result<bool, String>,
) -> Result<Vec<u8>, String> {
    let mut buf = Vec::<u8>::new();
    let mut raw = Vec::<u8>::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| t!("ai.err.stream_interrupted", error = e).to_string())?
    {
        raw.extend_from_slice(&chunk);
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            if let Some(data) = sse_data(line.trim()) {
                if on_data(data)? {
                    return Ok(raw);
                }
            }
        }
    }
    Ok(raw)
}

/// `data: xxx` / `data:xxx` → `xxx`;其它行(空行、`: keep-alive`、`event:`)→ None。
pub(crate) fn sse_data(line: &str) -> Option<&str> {
    line.strip_prefix("data:").map(str::trim)
}

/// Anthropic / Gemini 要求 system 单独传、user/assistant 严格交替:
/// 抽出 system,合并相邻同角色消息,丢掉空消息和开头的 assistant。
pub(crate) fn split_system(messages: &[ChatMessage]) -> (String, Vec<(bool, String)>) {
    let mut system = Vec::new();
    // (is_user, content)
    let mut turns: Vec<(bool, String)> = Vec::new();
    for m in messages {
        if m.content.trim().is_empty() {
            continue;
        }
        if m.role == "system" {
            system.push(m.content.clone());
            continue;
        }
        let is_user = m.role != "assistant";
        match turns.last_mut() {
            Some((u, c)) if *u == is_user => {
                c.push_str("\n\n");
                c.push_str(&m.content);
            }
            None if !is_user => {}
            _ => turns.push((is_user, m.content.clone())),
        }
    }
    (system.join("\n\n"), turns)
}

pub(crate) fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: content.into(),
        }
    }

    #[test]
    fn sse_data_handles_prefix_variants() {
        assert_eq!(sse_data("data: {\"a\":1}"), Some("{\"a\":1}"));
        assert_eq!(sse_data("data:[DONE]"), Some("[DONE]"));
        assert_eq!(sse_data(": keep-alive"), None);
        assert_eq!(sse_data("event: message_start"), None);
    }

    #[test]
    fn split_system_merges_and_alternates() {
        let (sys, turns) = split_system(&[
            msg("system", "S"),
            msg("assistant", "orphan"),
            msg("user", "a"),
            msg("user", "b"),
            msg("assistant", ""),
            msg("assistant", "c"),
        ]);
        assert_eq!(sys, "S");
        assert_eq!(turns, vec![(true, "a\n\nb".into()), (false, "c".into())]);
    }

    #[test]
    fn chat_model_filter() {
        assert!(is_chat_model("gpt-4o"));
        assert!(is_chat_model("claude-sonnet-4"));
        assert!(!is_chat_model("text-embedding-3-small"));
        assert!(!is_chat_model("qwen3-rerank"));
        assert!(!is_chat_model("gpt-realtime-whisper"));
    }

    #[test]
    fn error_status_extracts_message() {
        let body = r#"{"error":{"message":"Bad request for dependent service.","type":"x"}}"#;
        // 文案随全局 locale 变化,只断言与语言无关的部分
        let msg = map_error_status(reqwest::StatusCode::BAD_REQUEST, body);
        assert!(msg.contains("400"), "{msg}");
        assert!(msg.ends_with("Bad request for dependent service."), "{msg}");
        assert!(map_error_status(reqwest::StatusCode::BAD_GATEWAY, "<html>").ends_with("<html>"));
    }

    #[test]
    fn truncate_respects_char_boundary() {
        assert_eq!(truncate("中文字符串", 2), "中文");
        assert_eq!(truncate("ab", 300), "ab");
    }
}
