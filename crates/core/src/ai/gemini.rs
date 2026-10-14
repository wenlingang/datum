//! Google Gemini:`POST {base}/v1beta/models/{model}:streamGenerateContent?alt=sse`,
//! `x-goog-api-key` 鉴权,assistant 角色叫 model,system 走 systemInstruction。

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
