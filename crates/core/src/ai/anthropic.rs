//! Anthropic Messages API:`POST {base}/v1/messages`,`x-api-key` 鉴权,
//! system 单独传,流式事件为 content_block_delta / message_stop / error。

use crate::ai::agent::{CallAcc, Item, ToolSpec, TurnOut};
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

/// 带工具的一轮:assistant 的工具调用是 tool_use 内容块,结果是 user 消息里的 tool_result 块;
/// 流式里工具参数以 input_json_delta 分片到达。
pub(crate) async fn turn(
    client: &reqwest::Client,
    ep: &Endpoint,
    system: &str,
    items: &[Item],
    tools: &[ToolSpec],
    allow_tools: bool,
    on_delta: &(dyn Fn(String) + Send + Sync),
) -> Result<TurnOut, String> {
    let body = turn_body(ep, system, items, tools, allow_tools);
    let resp =
        send(auth(client.post(format!("{}/v1/messages", ep.base_url)), ep).json(&body)).await?;
    let mut out = TurnOut::default();
    let mut acc = CallAcc::default();
    // 内容块序号 → 工具调用序号(文本块不占位)
    let mut tool_index: std::collections::HashMap<u64, usize> = Default::default();
    read_sse(resp, |data| {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
            return Ok(false);
        };
        let block = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
        match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "content_block_start" => {
                let cb = v.get("content_block").cloned().unwrap_or_default();
                if cb.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    let n = tool_index.len();
                    tool_index.insert(block, n);
                    acc.push(n, cb.get("id").and_then(|x| x.as_str()), cb.get("name").and_then(|x| x.as_str()), None);
                }
                Ok(false)
            }
            "content_block_delta" => {
                let d = v.get("delta").cloned().unwrap_or_default();
                match d.get("type").and_then(|t| t.as_str()) {
                    Some("text_delta") => {
                        if let Some(t) = d.get("text").and_then(|t| t.as_str()).filter(|t| !t.is_empty()) {
                            out.text.push_str(t);
                            on_delta(t.to_string());
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(&n) = tool_index.get(&block) {
                            acc.push(n, None, None, d.get("partial_json").and_then(|x| x.as_str()));
                        }
                    }
                    _ => {}
                }
                Ok(false)
            }
            "message_stop" => Ok(true),
            "error" => match parse_event(data) {
                Some(Event::Error(e)) => Err(e),
                _ => Ok(true),
            },
            _ => Ok(false),
        }
    })
    .await?;
    out.calls = acc.finish();
    Ok(out)
}

fn turn_body(ep: &Endpoint, system: &str, items: &[Item], tools: &[ToolSpec], allow_tools: bool) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = items
        .iter()
        .map(|item| match item {
            Item::User(text) => serde_json::json!({ "role": "user", "content": text }),
            Item::Assistant { text, calls } if calls.is_empty() => {
                serde_json::json!({ "role": "assistant", "content": text })
            }
            Item::Assistant { text, calls } => {
                let mut blocks = Vec::new();
                if !text.trim().is_empty() {
                    blocks.push(serde_json::json!({ "type": "text", "text": text }));
                }
                for c in calls {
                    blocks.push(serde_json::json!({ "type": "tool_use", "id": c.id, "name": c.name, "input": c.args }));
                }
                serde_json::json!({ "role": "assistant", "content": blocks })
            }
            Item::Results(results) => serde_json::json!({
                "role": "user",
                "content": results
                    .iter()
                    .map(|(c, content)| serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": c.id,
                        "content": content,
                    }))
                    .collect::<Vec<_>>(),
            }),
        })
        .collect();
    let mut body = serde_json::json!({
        "model": ep.model,
        "max_tokens": MAX_TOKENS,
        "messages": messages,
        "stream": true,
    });
    if !system.is_empty() {
        body["system"] = system.into();
    }
    if !tools.is_empty() {
        body["tools"] = tools
            .iter()
            .map(|t| serde_json::json!({ "name": t.name, "description": t.description, "input_schema": t.parameters }))
            .collect();
        body["tool_choice"] = serde_json::json!({ "type": if allow_tools { "auto" } else { "none" } });
    }
    body
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
