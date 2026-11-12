//! Google Gemini:`POST {base}/v1beta/models/{model}:streamGenerateContent?alt=sse`,
//! `x-goog-api-key` 鉴权,assistant 角色叫 model,system 走 systemInstruction。

use crate::ai::agent::{Item, ToolCall, ToolSpec, TurnOut};
use crate::ai::chat::{get_json, read_sse, send, split_system, ChatEvent, ChatMessage, Mode};
use crate::ai::provider::Endpoint;
use rust_i18n::t;

fn auth(req: reqwest::RequestBuilder, ep: &Endpoint) -> reqwest::RequestBuilder {
    req.header("x-goog-api-key", &ep.api_key)
}

pub(crate) async fn stream(
    client: &reqwest::Client,
    ep: &Endpoint,
    messages: &[ChatMessage],
    mode: Mode,
    on_event: &(dyn Fn(ChatEvent) + Send + Sync),
) -> Result<String, String> {
    let (system, turns) = split_system(messages);
    let contents: Vec<_> = turns
        .into_iter()
        .map(|(is_user, text)| {
            serde_json::json!({
                "role": if is_user { "user" } else { "model" },
                "parts": [{ "text": text }],
            })
        })
        .collect();
    let mut body = serde_json::json!({ "contents": contents });
    if !system.is_empty() {
        body["systemInstruction"] = serde_json::json!({ "parts": [{ "text": system }] });
    }
    let model = ep.model.trim_start_matches("models/");
    let url = format!(
        "{}/v1beta/models/{model}:streamGenerateContent?alt=sse",
        ep.base_url
    );
    let resp = send(auth(client.post(url), ep).json(&body)).await?;

    let mut full = String::new();
    read_sse(resp, |data| {
        let v: serde_json::Value = serde_json::from_str(data).unwrap_or_default();
        if let Some(e) = v.pointer("/error/message").and_then(|m| m.as_str()) {
            return Err(t!("ai.err.service_error", message = e).to_string());
        }
        let t = chunk_text(&v);
        if !t.is_empty() {
            full.push_str(&t);
            on_event(ChatEvent::Delta { text: t });
            return Ok(mode == Mode::Probe);
        }
        Ok(false)
    })
    .await?;
    Ok(full)
}

/// 带工具的一轮:functionCall 整块到达(不分片);下一轮把模型的原始 part 原样带回
/// (新模型的 thoughtSignature 必须回传),结果以 functionResponse 交回。
pub(crate) async fn turn(
    client: &reqwest::Client,
    ep: &Endpoint,
    system: &str,
    items: &[Item],
    tools: &[ToolSpec],
    allow_tools: bool,
    on_delta: &(dyn Fn(String) + Send + Sync),
) -> Result<TurnOut, String> {
    let body = turn_body(system, items, tools, allow_tools);
    let model = ep.model.trim_start_matches("models/");
    let url = format!("{}/v1beta/models/{model}:streamGenerateContent?alt=sse", ep.base_url);
    let resp = send(auth(client.post(url), ep).json(&body)).await?;
    let mut out = TurnOut::default();
    read_sse(resp, |data| {
        let v: serde_json::Value = serde_json::from_str(data).unwrap_or_default();
        if let Some(e) = v.pointer("/error/message").and_then(|m| m.as_str()) {
            return Err(t!("ai.err.service_error", message = e).to_string());
        }
        let t = chunk_text(&v);
        if !t.is_empty() {
            out.text.push_str(&t);
            on_delta(t);
        }
        for part in v.pointer("/candidates/0/content/parts").and_then(|p| p.as_array()).into_iter().flatten() {
            let Some(fc) = part.get("functionCall") else {
                continue;
            };
            let n = out.calls.len();
            out.calls.push(ToolCall {
                id: fc.get("id").and_then(|x| x.as_str()).map(str::to_string).unwrap_or_else(|| format!("call_{n}")),
                name: fc.get("name").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                args: fc.get("args").cloned().unwrap_or_else(|| serde_json::json!({})),
                raw: Some(part.clone()),
            });
        }
        Ok(false)
    })
    .await?;
    out.calls.retain(|c| !c.name.is_empty());
    Ok(out)
}

fn turn_body(system: &str, items: &[Item], tools: &[ToolSpec], allow_tools: bool) -> serde_json::Value {
    let contents: Vec<serde_json::Value> = items
        .iter()
        .map(|item| match item {
            Item::User(text) => serde_json::json!({ "role": "user", "parts": [{ "text": text }] }),
            Item::Assistant { text, calls } => {
                let mut parts = Vec::new();
                if !text.trim().is_empty() {
                    parts.push(serde_json::json!({ "text": text }));
                }
                for c in calls {
                    parts.push(c.raw.clone().unwrap_or_else(|| {
                        serde_json::json!({ "functionCall": { "name": c.name, "args": c.args } })
                    }));
                }
                serde_json::json!({ "role": "model", "parts": parts })
            }
            Item::Results(results) => serde_json::json!({
                "role": "user",
                "parts": results
                    .iter()
                    .map(|(c, content)| {
                        let mut fr = serde_json::json!({ "name": c.name, "response": { "result": content } });
                        if let Some(id) = c.raw.as_ref().and_then(|r| r.pointer("/functionCall/id")) {
                            fr["id"] = id.clone();
                        }
                        serde_json::json!({ "functionResponse": fr })
                    })
                    .collect::<Vec<_>>(),
            }),
        })
        .collect();
    let mut body = serde_json::json!({ "contents": contents });
    if !system.is_empty() {
        body["systemInstruction"] = serde_json::json!({ "parts": [{ "text": system }] });
    }
    if !tools.is_empty() {
        body["tools"] = serde_json::json!([{
            "functionDeclarations": tools
                .iter()
                .map(|t| serde_json::json!({ "name": t.name, "description": t.description, "parameters": t.parameters }))
                .collect::<Vec<_>>(),
        }]);
        body["toolConfig"] = serde_json::json!({
            "functionCallingConfig": { "mode": if allow_tools { "AUTO" } else { "NONE" } }
        });
    }
    body
}

pub(crate) async fn list_models(
    client: &reqwest::Client,
    ep: &Endpoint,
) -> Result<Vec<String>, String> {
    let v = get_json(auth(
        client.get(format!("{}/v1beta/models?pageSize=1000", ep.base_url)),
        ep,
    ))
    .await?;
    parse_models(&v).ok_or_else(|| t!("ai.err.bad_model_list").to_string())
}

/// `{"models":[{"name":"models/gemini-2.5-pro","supportedGenerationMethods":[..]}]}`,
/// 只留支持 generateContent 的,去掉 `models/` 前缀。
fn parse_models(v: &serde_json::Value) -> Option<Vec<String>> {
    Some(
        v.get("models")?
            .as_array()?
            .iter()
            .filter(|m| {
                m.get("supportedGenerationMethods")
                    .and_then(|s| s.as_array())
                    .is_some_and(|s| s.iter().any(|x| x == "generateContent"))
            })
            .filter_map(|m| {
                let name = m.get("name")?.as_str()?;
                Some(name.trim_start_matches("models/").to_string())
            })
            .collect(),
    )
}

/// candidates[0].content.parts[*].text 拼接;思考摘要(thought: true)不算回复正文。
fn chunk_text(v: &serde_json::Value) -> String {
    v.pointer("/candidates/0/content/parts")
        .and_then(|p| p.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter(|p| !p.get("thought").and_then(|t| t.as_bool()).unwrap_or(false))
                .filter_map(|p| p.get("text")?.as_str())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_and_skips_thoughts() {
        let v = serde_json::json!({"candidates":[{"content":{"role":"model","parts":[
            {"text":"思考","thought":true},{"text":"SELECT "},{"text":"1"}]}}]});
        assert_eq!(chunk_text(&v), "SELECT 1");
        assert_eq!(chunk_text(&serde_json::json!({"usageMetadata":{}})), "");
    }

    #[test]
    fn model_list_keeps_generate_content_only() {
        let v = serde_json::json!({"models":[
            {"name":"models/gemini-2.5-pro","supportedGenerationMethods":["generateContent","countTokens"]},
            {"name":"models/text-embedding-004","supportedGenerationMethods":["embedContent"]}]});
        assert_eq!(parse_models(&v).unwrap(), vec!["gemini-2.5-pro"]);
    }
}
