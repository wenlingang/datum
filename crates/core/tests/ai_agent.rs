//! 带工具的对话,三种协议端到端:本地 mock 依次回「调用工具」与「最终回答」的流,
//! 工具在真实的 SQLite 库上执行;检查事件序列,以及第二轮请求里工具调用 / 结果的格式。

use datum_core::ai::agent::{self, AgentEvent, ToolFuture, ToolOutput, ToolRunner};
use datum_core::ai::chat::ChatMessage;
use datum_core::ai::db_tools::{self, DbTools};
use datum_core::ai::provider;
use datum_core::ai::settings::{Protocol, ProviderConfig};
use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 依次回应多个请求的 mock;返回 (base_url, 收到的请求体)。
async fn mock(responses: Vec<String>) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    tokio::spawn(async move {
        for resp in responses {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 8192];
            let head_end = loop {
                let n = sock.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break p + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
            let len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                .unwrap_or(0);
            while buf.len() < head_end + len {
                let n = sock.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            seen.lock()
                .unwrap()
                .push(serde_json::from_slice(&buf[head_end..head_end + len]).unwrap_or_default());
            let out = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{resp}",
                resp.len()
            );
            sock.write_all(out.as_bytes()).await.unwrap();
            sock.shutdown().await.ok();
        }
    });
    (format!("http://{addr}"), bodies)
}

fn sse(events: &[serde_json::Value]) -> String {
    events.iter().map(|e| format!("data: {e}\n\n")).collect()
}

async fn sqlite_state() -> (AppState, String) {
    let dir = std::env::temp_dir().join(format!("datum-agent-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let st = AppState::new(dir.clone());
    let file = dir.join("shop.db");
    let db = rusqlite::Connection::open(&file).unwrap();
    db.execute_batch(
        "CREATE TABLE orders(id INTEGER PRIMARY KEY, status TEXT, total REAL);
         INSERT INTO orders(status,total) VALUES ('paid',10),('paid',20),('cancelled',5);",
    )
    .unwrap();
    let c = Connection {
        id: "shop".into(),
        name: "shop".into(),
        kind: DbKind::Sqlite,
        env: Env::Local,
        host: String::new(),
        port: 0,
        user: String::new(),
        database: String::new(),
        plaintext_password: None,
        file_path: Some(file.display().to_string()),
        ssl_mode: SslMode::Disable,
        uri: None,
        auth_source: None,
        ssh: None,
    };
    commands::save_connection(&st, c, Some(String::new())).unwrap();
    service::connect(&st, &DbKind::Sqlite, "shop").await.unwrap();
    (st, "shop".into())
}

fn endpoint(protocol: Protocol, base: &str) -> provider::Endpoint {
    provider::build(
        &ProviderConfig {
            id: "p".into(),
            name: "p".into(),
            protocol,
            base_url: base.into(),
            api_key: "k".into(),
            enabled: true,
            models: vec![],
        },
        "m",
    )
    .unwrap()
}

const QUESTION: &str = "how many paid orders?";
const SQL: &str = "SELECT COUNT(*) AS n FROM orders WHERE status = 'paid'";

async fn run(ep: provider::Endpoint, st: &AppState, id: &str) -> (Vec<AgentEvent>, String) {
    let events = Mutex::new(Vec::new());
    let messages = vec![
        ChatMessage { role: "system".into(), content: "You are a SQL assistant".into() },
        ChatMessage { role: "user".into(), content: QUESTION.into() },
    ];
    let runner = DbTools { state: st, kind: DbKind::Sqlite, id: id.into() };
    let full = agent::run(&ep, &messages, &db_tools::specs(), &runner, &agent::Cancel::default(), &|e| events.lock().unwrap().push(e))
        .await
        .unwrap();
    (events.into_inner().unwrap(), full)
}

/// 事件序列:先说一句 → 工具开始 / 结束(真查出 2)→ 最终回答 → Done。
fn check_events(events: &[AgentEvent], full: &str) {
    let tool_end = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolEnd { ok, detail, .. } => Some((ok, detail.clone())),
            _ => None,
        })
        .expect("工具没有执行");
    assert!(*tool_end.0, "{:?}", tool_end.1);
    assert_eq!(tool_end.1["rows"][0][0], "2");
    assert!(matches!(events.last(), Some(AgentEvent::Done { .. })));
    assert!(full.starts_with("Let me check.\n"), "{full}");
    assert!(full.ends_with("There are 2 paid orders."), "{full}");
}

#[tokio::test]
async fn openai_tool_round_trip() {
    let (st, id) = sqlite_state().await;
    let args = serde_json::json!({ "sql": SQL }).to_string();
    let (half1, half2) = args.split_at(10);
    let first = sse(&[
        serde_json::json!({"choices":[{"delta":{"content":"Let me check."}}]}),
        serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"run_query","arguments":half1}}]}}]}),
        serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":half2}}]}}]}),
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
    ]) + "data: [DONE]\n\n";
    let second = sse(&[serde_json::json!({"choices":[{"delta":{"content":"There are 2 paid orders."}}]})]) + "data: [DONE]\n\n";
    let (base, bodies) = mock(vec![first, second]).await;
    let (events, full) = run(endpoint(Protocol::OpenAi, &base), &st, &id).await;
    check_events(&events, &full);
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies[0]["tools"][2]["function"]["name"], "run_query");
    assert_eq!(bodies[0]["tool_choice"], "auto");
    let msgs = bodies[1]["messages"].as_array().unwrap();
    let assistant = &msgs[msgs.len() - 2];
    assert_eq!(assistant["tool_calls"][0]["id"], "call_a");
    assert_eq!(assistant["tool_calls"][0]["function"]["arguments"], args);
    let tool = msgs.last().unwrap();
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["tool_call_id"], "call_a");
    assert!(tool["content"].as_str().unwrap().contains("n\n2"));
}

#[tokio::test]
async fn anthropic_tool_round_trip() {
    let (st, id) = sqlite_state().await;
    let args = serde_json::json!({ "sql": SQL }).to_string();
    let (half1, half2) = args.split_at(12);
    let first = sse(&[
        serde_json::json!({"type":"message_start","message":{}}),
        serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Let me check."}}),
        serde_json::json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"run_query","input":{}}}),
        serde_json::json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":half1}}),
        serde_json::json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":half2}}),
        serde_json::json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
        serde_json::json!({"type":"message_stop"}),
    ]);
    let second = sse(&[
        serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"There are 2 paid orders."}}),
        serde_json::json!({"type":"message_stop"}),
    ]);
    let (base, bodies) = mock(vec![first, second]).await;
    let (events, full) = run(endpoint(Protocol::Anthropic, &base), &st, &id).await;
    check_events(&events, &full);
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies[0]["tools"][2]["input_schema"]["required"][0], "sql");
    assert_eq!(bodies[0]["system"], "You are a SQL assistant");
    let msgs = bodies[1]["messages"].as_array().unwrap();
    let assistant = &msgs[msgs.len() - 2];
    assert_eq!(assistant["content"][1]["type"], "tool_use");
    assert_eq!(assistant["content"][1]["input"]["sql"], SQL);
    let result = &msgs.last().unwrap()["content"][0];
    assert_eq!(result["type"], "tool_result");
    assert_eq!(result["tool_use_id"], "toolu_1");
}

#[tokio::test]
async fn gemini_tool_round_trip_keeps_thought_signature() {
    let (st, id) = sqlite_state().await;
    let first = sse(&[serde_json::json!({"candidates":[{"content":{"role":"model","parts":[
        {"text":"Let me check."},
        {"functionCall":{"name":"run_query","args":{"sql":SQL}},"thoughtSignature":"sig-123"}
    ]}}]})]);
    let second = sse(&[serde_json::json!({"candidates":[{"content":{"role":"model","parts":[{"text":"There are 2 paid orders."}]}}]})]);
    let (base, bodies) = mock(vec![first, second]).await;
    let (events, full) = run(endpoint(Protocol::Gemini, &base), &st, &id).await;
    check_events(&events, &full);
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies[0]["tools"][0]["functionDeclarations"][2]["name"], "run_query");
    let contents = bodies[1]["contents"].as_array().unwrap();
    let model = &contents[contents.len() - 2];
    assert_eq!(model["parts"][1]["thoughtSignature"], "sig-123", "thoughtSignature 必须原样带回");
    let fr = &contents.last().unwrap()["parts"][0]["functionResponse"];
    assert_eq!(fr["name"], "run_query");
    assert!(fr["response"]["result"].as_str().unwrap().contains("2"));
}

/// 写语句不会被执行:工具拒绝并提示模型改用代码块;数据不变。
#[tokio::test]
async fn writes_are_refused_by_the_query_tool() {
    let (st, id) = sqlite_state().await;
    let runner = DbTools { state: &st, kind: DbKind::Sqlite, id: id.clone() };
    let args = serde_json::json!({ "sql": "DELETE FROM orders" });
    let o: ToolOutput = runner.run("run_query", &args).await;
    assert!(!o.ok);
    assert!(o.for_model.contains("```sql"));
    // 只读判定绕过时,数据库层的只读会话也会拒绝
    let r = service::query(&st, &DbKind::Sqlite, &id, "SELECT COUNT(*) FROM orders", None).await.unwrap();
    assert_eq!(r.rows[0][0].as_deref(), Some("3"));
}

/// 步数用完:最后一轮要求不再调用工具(tool_choice none),模型回答后结束。
#[tokio::test]
async fn step_limit_forces_an_answer() {
    struct Echo;
    impl ToolRunner for Echo {
        fn run<'a>(&'a self, _: &'a str, _: &'a serde_json::Value) -> ToolFuture<'a> {
            Box::pin(async {
                ToolOutput { for_model: "[]".into(), ok: true, summary: "ok".into(), detail: serde_json::json!({}) }
            })
        }
    }
    let call = sse(&[serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"list_tables","arguments":"{}"}}]}}]})])
        + "data: [DONE]\n\n";
    let mut responses = vec![call; agent::MAX_STEPS];
    responses.push(sse(&[serde_json::json!({"choices":[{"delta":{"content":"done"}}]})]) + "data: [DONE]\n\n");
    let (base, bodies) = mock(responses).await;
    let ep = endpoint(Protocol::OpenAi, &base);
    let messages = vec![ChatMessage { role: "user".into(), content: "loop".into() }];
    let full = agent::run(&ep, &messages, &db_tools::specs(), &Echo, &agent::Cancel::default(), &|_| {}).await.unwrap();
    assert_eq!(full, "done");
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), agent::MAX_STEPS + 1);
    assert_eq!(bodies.last().unwrap()["tool_choice"], "none");
    assert_eq!(bodies[0]["tool_choice"], "auto");
}

/// 停止:服务商一直不回应时,取消后立即以已有文本结束(仍发 Done),不等到超时。
#[tokio::test]
async fn cancel_interrupts_a_hanging_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (_sock, _) = listener.accept().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    });
    let (st, id) = sqlite_state().await;
    let runner = DbTools { state: &st, kind: DbKind::Sqlite, id };
    let cancel = agent::Cancel::default();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        c.cancel();
    });
    let done = Mutex::new(false);
    let started = std::time::Instant::now();
    let messages = vec![ChatMessage { role: "user".into(), content: "hi".into() }];
    let full = agent::run(&endpoint(Protocol::OpenAi, &base), &messages, &db_tools::specs(), &runner, &cancel, &|e| {
        if matches!(e, AgentEvent::Done { .. }) {
            *done.lock().unwrap() = true;
        }
    })
    .await
    .unwrap();
    assert!(started.elapsed().as_secs() < 5);
    assert_eq!(full, "");
    assert!(*done.lock().unwrap());
}
