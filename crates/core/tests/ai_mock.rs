//! 三种协议的端到端 mock:本地起一个 HTTP 服务,按各家真实格式回 SSE / JSON,
//! 同时检查我们发出的路径、鉴权头与请求体。

use datum_core::ai::chat::{self, ChatEvent, ChatMessage};
use datum_core::ai::provider;
use datum_core::ai::settings::{Protocol, ProviderConfig};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// 收到的一次请求
#[derive(Debug)]
struct Captured {
    /// 如 `POST /v1/messages`
    line: String,
    headers: Vec<(String, String)>,
    body: serde_json::Value,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// 起一个只服务一次请求的 mock,返回 (base_url, 收到的请求)。
async fn mock(
    status: u16,
    content_type: &'static str,
    body: String,
) -> (String, oneshot::Receiver<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        // 读到头部结束,再按 Content-Length 读完请求体
        let head_end = loop {
            let n = sock.read(&mut tmp).await.unwrap();
            buf.extend_from_slice(&tmp[..n]);
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break p + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.split("\r\n");
        let line = lines
            .next()
            .unwrap()
            .rsplit_once(' ')
            .unwrap()
            .0
            .to_string();
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        let len: usize = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .map_or(0, |(_, v)| v.parse().unwrap());
        while buf.len() < head_end + len {
            let n = sock.read(&mut tmp).await.unwrap();
            buf.extend_from_slice(&tmp[..n]);
        }
        let req_body = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap_or_default();
        let _ = tx.send(Captured {
            line,
            headers,
            body: req_body,
        });
        let resp = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
        sock.shutdown().await.ok();
    });
    (format!("http://{addr}"), rx)
}

fn sse(frames: &[&str]) -> String {
    frames.iter().map(|f| format!("data: {f}\n\n")).collect()
}

fn provider(protocol: Protocol, base: &str) -> ProviderConfig {
    ProviderConfig {
        id: "p".into(),
        name: "P".into(),
        protocol,
        base_url: base.into(),
        api_key: "sk-test".into(),
        enabled: true,
        models: vec![],
    }
}

fn conversation() -> Vec<ChatMessage> {
    let m = |role: &str, content: &str| ChatMessage {
        role: role.into(),
        content: content.into(),
    };
    vec![
        m("system", "你是 SQL 助手"),
        m("user", "建一张表"),
        m("assistant", "好的"),
        m("user", "执行结果: ok"),
        m("user", "再加一列"),
    ]
}

/// 跑一次流式对话,返回 (拼接的增量文本, Done 里的全文)。
async fn run_chat(p: &ProviderConfig, model: &str) -> Result<(String, String), String> {
    let ep = provider::build(p, model)?;
    let deltas = Arc::new(Mutex::new(String::new()));
    let done = Arc::new(Mutex::new(String::new()));
    let (d, f) = (deltas.clone(), done.clone());
    chat::stream_chat(&ep, &conversation(), &move |e| match e {
        ChatEvent::Delta { text } => d.lock().unwrap().push_str(&text),
        ChatEvent::Done { full } => *f.lock().unwrap() = full,
        ChatEvent::Error { .. } => {}
    })
    .await?;
    let (d, f) = (deltas.lock().unwrap().clone(), done.lock().unwrap().clone());
    Ok((d, f))
}

#[tokio::test]
async fn openai_stream_roundtrip() {
    let (base, req) = mock(
        200,
        "text/event-stream",
        sse(&[
            r#"{"choices":[{"delta":{"role":"assistant"}}]}"#,
            r#"{"choices":[{"delta":{"content":"SELECT "}}]}"#,
            r#"{"choices":[{"delta":{"content":"1"}}]}"#,
            "[DONE]",
        ]),
    )
    .await;
    // 站点根自动补 /v1
    let (text, full) = run_chat(&provider(Protocol::OpenAi, &base), "gpt-4o")
        .await
        .unwrap();
    assert_eq!((text.as_str(), full.as_str()), ("SELECT 1", "SELECT 1"));
    let req = req.await.unwrap();
    assert_eq!(req.line, "POST /v1/chat/completions");
    assert_eq!(req.header("authorization"), Some("Bearer sk-test"));
    assert_eq!(req.body["model"], "gpt-4o");
    assert_eq!(req.body["stream"], true);
    // OpenAI 原样转发全部消息(含 system)
    assert_eq!(req.body["messages"].as_array().unwrap().len(), 5);
}

/// 网关无视 stream:true 直接回整段 JSON:按非流式兜底。
#[tokio::test]
async fn openai_non_stream_fallback() {
    let (base, _req) = mock(
        200,
        "application/json",
        r#"{"choices":[{"message":{"role":"assistant","content":"SELECT 2"}}]}"#.into(),
    )
    .await;
    let (text, full) = run_chat(&provider(Protocol::OpenAi, &base), "m")
        .await
        .unwrap();
    assert_eq!((text.as_str(), full.as_str()), ("SELECT 2", "SELECT 2"));
}

#[tokio::test]
async fn anthropic_stream_roundtrip() {
    let (base, req) = mock(
        200,
        "text/event-stream",
        // Anthropic 的 SSE 带 event: 行,我们只看 data
        [
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: ping\ndata: {\"type\":\"ping\"}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ALTER \"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"TABLE\"}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ]
        .concat(),
    )
    .await;
    // 用户多填了 /v1 也能用
    let p = provider(Protocol::Anthropic, &format!("{base}/v1"));
    let (text, full) = run_chat(&p, "claude-sonnet-4-5").await.unwrap();
    assert_eq!(
        (text.as_str(), full.as_str()),
        ("ALTER TABLE", "ALTER TABLE")
    );
    let req = req.await.unwrap();
    assert_eq!(req.line, "POST /v1/messages");
    assert_eq!(req.header("x-api-key"), Some("sk-test"));
    assert_eq!(req.header("anthropic-version"), Some("2023-06-01"));
    assert!(req.header("authorization").is_none());
    assert_eq!(req.body["system"], "你是 SQL 助手");
    assert!(req.body["max_tokens"].as_u64().unwrap() > 0);
    // system 抽出;相邻两条 user 合并,保证严格交替
    let msgs = req.body["messages"].as_array().unwrap();
    let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["user", "assistant", "user"]);
    assert_eq!(msgs[2]["content"], "执行结果: ok\n\n再加一列");
}

#[tokio::test]
async fn anthropic_error_event_surfaces() {
    let (base, _req) = mock(
        200,
        "text/event-stream",
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n".into(),
    )
    .await;
    let err = run_chat(&provider(Protocol::Anthropic, &base), "claude")
        .await
        .unwrap_err();
    assert!(err.contains("Overloaded"), "{err}");
}

#[tokio::test]
async fn gemini_stream_roundtrip() {
    let (base, req) = mock(
        200,
        "text/event-stream",
        sse(&[
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"想一想","thought":true}]}}]}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"CREATE "}]}}]}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"INDEX"}]},"finishReason":"STOP"}],"usageMetadata":{}}"#,
        ]),
    )
    .await;
    let (text, full) = run_chat(&provider(Protocol::Gemini, &base), "gemini-2.5-pro")
        .await
        .unwrap();
    assert_eq!(
        (text.as_str(), full.as_str()),
        ("CREATE INDEX", "CREATE INDEX")
    );
    let req = req.await.unwrap();
    assert_eq!(
        req.line,
        "POST /v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
    );
    assert_eq!(req.header("x-goog-api-key"), Some("sk-test"));
    assert_eq!(
        req.body["systemInstruction"]["parts"][0]["text"],
        "你是 SQL 助手"
    );
    let roles: Vec<&str> = req.body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["user", "model", "user"]);
}

#[tokio::test]
async fn http_error_shows_server_message() {
    let (base, _req) = mock(
        401,
        "application/json",
        r#"{"error":{"type":"authentication_error","message":"invalid x-api-key"}}"#.into(),
    )
    .await;
    let err = run_chat(&provider(Protocol::Anthropic, &base), "claude")
        .await
        .unwrap_err();
    assert!(err.contains("invalid x-api-key"), "{err}");
}

/// 「测试」:收到首段内容即判定可用,不等整个回复。
#[tokio::test]
async fn probe_succeeds_on_first_delta() {
    let (base, req) = mock(
        200,
        "text/event-stream",
        sse(&[r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}"#]),
    )
    .await;
    let ep = provider::build(&provider(Protocol::Anthropic, &base), "claude").unwrap();
    chat::test_connection(&ep).await.unwrap();
    let req = req.await.unwrap();
    assert_eq!(req.body["messages"][0]["content"], "hi");
}

#[tokio::test]
async fn list_models_per_protocol() {
    let (base, req) = mock(
        200,
        "application/json",
        r#"{"data":[{"id":"gpt-4o"},{"id":"text-embedding-3-small"},{"id":"o3"}]}"#.into(),
    )
    .await;
    let ep = provider::build_for_listing(&provider(Protocol::OpenAi, &base)).unwrap();
    assert_eq!(chat::list_models(&ep).await.unwrap(), ["gpt-4o", "o3"]);
    assert_eq!(req.await.unwrap().line, "GET /v1/models");

    let (base, req) = mock(
        200,
        "application/json",
        r#"{"data":[{"id":"claude-sonnet-4-5","type":"model"}],"has_more":false}"#.into(),
    )
    .await;
    let ep = provider::build_for_listing(&provider(Protocol::Anthropic, &base)).unwrap();
    assert_eq!(chat::list_models(&ep).await.unwrap(), ["claude-sonnet-4-5"]);
    let req = req.await.unwrap();
    assert_eq!(req.line, "GET /v1/models?limit=1000");
    assert_eq!(req.header("x-api-key"), Some("sk-test"));

    let (base, req) = mock(
        200,
        "application/json",
        r#"{"models":[{"name":"models/gemini-2.5-flash","supportedGenerationMethods":["generateContent"]},
                      {"name":"models/text-embedding-004","supportedGenerationMethods":["embedContent"]}]}"#
            .into(),
    )
    .await;
    let ep = provider::build_for_listing(&provider(Protocol::Gemini, &base)).unwrap();
    assert_eq!(chat::list_models(&ep).await.unwrap(), ["gemini-2.5-flash"]);
    let req = req.await.unwrap();
    assert_eq!(req.line, "GET /v1beta/models?pageSize=1000");
    assert_eq!(req.header("x-goog-api-key"), Some("sk-test"));
}
