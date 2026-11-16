//! 敏感数据遮罩在 AI 出口上的强制执行:工具查询结果、对话历史里的结果表、一句话筛选的样例行、
//! 补全的取值提示。都在真实的 SQLite 库上跑,遮罩开关来自配置目录的 mask_settings.json。

use datum_core::ai::agent::{self, AgentEvent, ToolRunner};
use datum_core::ai::chat::ChatMessage;
use datum_core::ai::commands::{nl_samples, render_history, WireMessage, WirePart};
use datum_core::ai::db_tools::{self, DbTools};
use datum_core::ai::nl_filter::ColumnInfo;
use datum_core::ai::provider;
use datum_core::ai::settings::{Protocol, ProviderConfig};
use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::mask::{ConnMask, MaskRepo, MaskSettings, Override};
use datum_core::service;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const EMAIL: &str = "alice@example.com";
const PHONE: &str = "13812345678";
const PASSWORD: &str = "hunter2-secret";

/// 本地 SQLite 连接 + customers 表;`masking` 写进 mask_settings.json 的开关。
async fn state(masking: Option<bool>) -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-mask-it-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let st = AppState::new(dir.clone());
    let file = dir.join("crm.db");
    let db = rusqlite::Connection::open(&file).unwrap();
    db.execute_batch(&format!(
        "CREATE TABLE customers(id INTEGER PRIMARY KEY, name TEXT, email TEXT, phone TEXT, password TEXT, status TEXT);
         INSERT INTO customers(name,email,phone,password,status) VALUES
           ('Alice','{EMAIL}','{PHONE}','{PASSWORD}','active'),
           ('Bob','bob@example.org','13987654321','pw2','vip');"
    ))
    .unwrap();
    let c = Connection {
        id: "crm".into(),
        name: "crm".into(),
        kind: DbKind::Sqlite,
        env: Env::Local,
        host: String::new(),
        port: 0,
        user: String::new(),
        database: String::new(),
        plaintext_password: None,
        file_path: Some(file.display().to_string()),
        ssl_mode: SslMode::Disable,
        ssh: None,
    };
    commands::save_connection(&st, c, Some(String::new())).unwrap();
    if let Some(on) = masking {
        let mut s = MaskSettings::default();
        s.connections.insert("crm".into(), ConnMask { enabled: Some(on), ..Default::default() });
        MaskRepo::new(&dir).save(&s).unwrap();
    }
    service::connect(&st, &DbKind::Sqlite, "crm").await.unwrap();
    st
}

fn runner(st: &AppState) -> DbTools<'_> {
    DbTools { state: st, kind: DbKind::Sqlite, id: "crm".into(), allow_data: true }
}

/// 工具查询:遮罩开着时,交给模型的文本和界面上的结果表都是遮罩后的值。
#[tokio::test]
async fn agent_tool_result_is_masked() {
    let st = state(Some(true)).await;
    let args = serde_json::json!({ "sql": "SELECT id, name, email, phone, password, status FROM customers ORDER BY id" });
    let out = runner(&st).run(db_tools::RUN_QUERY, &args).await;
    assert!(out.ok, "{}", out.for_model);
    for raw in [EMAIL, PHONE, PASSWORD] {
        assert!(!out.for_model.contains(raw), "模型看到了原值 {raw}:{}", out.for_model);
        assert!(!out.detail.to_string().contains(raw), "界面结果里有原值 {raw}");
    }
    assert!(out.for_model.contains("a***@example.com"), "{}", out.for_model);
    assert!(out.for_model.contains("138****5678"));
    assert!(out.for_model.contains("••••••"));
    assert!(out.for_model.contains("masked by the user's privacy settings"));
    // 不敏感的列原样
    assert!(out.for_model.contains("active"));
    assert_eq!(out.detail["masked"], true);
    assert_eq!(out.detail["rows"][0][2], "a***@example.com");
}

/// 遮罩关着(本地库默认):原值照常给模型。
#[tokio::test]
async fn agent_tool_result_unmasked_when_off() {
    let st = state(None).await;
    let out = runner(&st).run(db_tools::RUN_QUERY, &serde_json::json!({ "sql": "SELECT email FROM customers" })).await;
    assert!(out.for_model.contains(EMAIL), "{}", out.for_model);
    assert_eq!(out.detail["masked"], false);
}

/// 手动覆盖:永不遮罩的列放行(单表查询认得出来源表),强制遮罩的列按整体遮罩。
#[tokio::test]
async fn overrides_apply_to_tool_results() {
    let st = state(Some(true)).await;
    let mut s = MaskRepo::new(&st.config_dir).load();
    let c = s.connections.get_mut("crm").unwrap();
    c.overrides.insert("customers.email".into(), Override::Plain);
    c.overrides.insert("customers.status".into(), Override::Mask);
    MaskRepo::new(&st.config_dir).save(&s).unwrap();
    let out = runner(&st).run(db_tools::RUN_QUERY, &serde_json::json!({ "sql": "SELECT email, status FROM customers" })).await;
    assert!(out.for_model.contains(EMAIL), "{}", out.for_model);
    assert!(!out.for_model.contains("active"), "{}", out.for_model);
}

/// 演示模式:即使连接的遮罩关了也强制遮罩。
#[tokio::test]
async fn demo_mode_forces_masking() {
    let st = state(Some(false)).await;
    let mut s = MaskRepo::new(&st.config_dir).load();
    s.demo_mode = true;
    MaskRepo::new(&st.config_dir).save(&s).unwrap();
    let out = runner(&st).run(db_tools::RUN_QUERY, &serde_json::json!({ "sql": "SELECT email FROM customers" })).await;
    assert!(!out.for_model.contains(EMAIL), "{}", out.for_model);
}

/// 依次回应多个请求的 mock(OpenAI 流式格式);返回 (base_url, 收到的请求体)。
async fn mock(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
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
            seen.lock().unwrap().push(String::from_utf8_lossy(&buf[head_end..head_end + len]).into_owned());
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
    events.iter().map(|e| format!("data: {e}\n\n")).collect::<String>() + "data: [DONE]\n\n"
}

/// 端到端:模型调用 run_query,第二轮请求(发给服务商的报文)里只有遮罩后的值;界面事件同样遮罩。
#[tokio::test]
async fn provider_never_receives_raw_values() {
    let st = state(Some(true)).await;
    let args = serde_json::json!({ "sql": "SELECT name, email, phone FROM customers" }).to_string();
    let first = sse(&[
        serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"run_query","arguments":args}}]}}]}),
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
    ]);
    let second = sse(&[serde_json::json!({"choices":[{"delta":{"content":"done"}}]})]);
    let (base, bodies) = mock(vec![first, second]).await;
    let ep = provider::build(
        &ProviderConfig {
            id: "p".into(),
            name: "p".into(),
            protocol: Protocol::OpenAi,
            base_url: base,
            api_key: "k".into(),
            enabled: true,
            models: vec![],
        },
        "m",
    )
    .unwrap();
    let events = Mutex::new(Vec::new());
    let messages = vec![ChatMessage { role: "user".into(), content: "list customers".into() }];
    let r = runner(&st);
    agent::run(&ep, &messages, &db_tools::specs(), &r, &agent::Cancel::default(), &|e| events.lock().unwrap().push(e))
        .await
        .unwrap();
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(!bodies[1].contains(EMAIL) && !bodies[1].contains(PHONE), "{}", bodies[1]);
    assert!(bodies[1].contains("a***@example.com"), "{}", bodies[1]);
    let events = events.into_inner().unwrap();
    let detail = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolEnd { detail, .. } => Some(detail.to_string()),
            _ => None,
        })
        .unwrap();
    assert!(!detail.contains(EMAIL), "{detail}");
}

fn table_part(rows: Vec<Vec<Option<String>>>) -> WirePart {
    WirePart::Table {
        columns: vec!["email".into(), "status".into()],
        rows,
        more: 3,
        sql: Some("SELECT email, status FROM customers".into()),
    }
}

/// 对话历史里的结果表(用户执行的卡片、之前的工具结果)由 core 渲染,发出前遮罩。
#[tokio::test]
async fn history_tables_are_masked_in_core() {
    let st = state(Some(true)).await;
    let wire = vec![
        WireMessage { role: "user".into(), content: "who?".into(), parts: vec![] },
        WireMessage {
            role: "user".into(),
            content: String::new(),
            parts: vec![
                WirePart::Text { text: "Executed:".into() },
                table_part(vec![vec![Some(EMAIL.into()), Some("active".into())], vec![None, Some("vip".into())]]),
            ],
        },
    ];
    let out = render_history(&st, Some("crm"), wire.clone());
    assert_eq!(out[0].content, "who?");
    assert_eq!(out[1].content, "Executed:\nemail\tstatus\na***@example.com\tactive\nNULL\tvip\n… 3 more");
    // 遮罩关掉:原值
    let st2 = state(Some(false)).await;
    assert!(render_history(&st2, Some("crm"), wire.clone())[1].content.contains(EMAIL));
    // 没有绑定连接:结果表不发
    assert_eq!(render_history(&st, None, wire)[1].content, "Executed:");
}

/// 生产库未允许发数据:历史里的结果表整段不发(与界面侧的判断一致,core 再兜一层)。
#[tokio::test]
async fn history_tables_dropped_for_prod_without_permission() {
    let st = state(None).await;
    let mut conns = commands::list_connections(&st).unwrap();
    let mut c = conns.remove(0);
    c.env = Env::Prod;
    commands::save_connection(&st, c, None).unwrap();
    let wire = vec![WireMessage {
        role: "user".into(),
        content: String::new(),
        parts: vec![WirePart::Text { text: "r".into() }, table_part(vec![vec![Some(EMAIL.into()), None]])],
    }];
    assert_eq!(render_history(&st, Some("crm"), wire)[0].content, "r");
}

/// 一句话筛选的样例行:敏感列遮罩,其余照常(帮模型认枚举写法)。
#[tokio::test]
async fn nl_filter_samples_are_masked() {
    let st = state(Some(true)).await;
    let cols: Vec<ColumnInfo> = serde_json::from_value(serde_json::json!([{ "name": "email" }, { "name": "status" }])).unwrap();
    let samples = vec![vec![Some(EMAIL.to_string()), Some("active".to_string())]];
    let out = nl_samples(&st, "crm", Some("customers"), &cols, samples);
    assert_eq!(out, vec![vec![Some("a***@example.com".to_string()), Some("active".to_string())]]);
}

/// 补全的取值提示:敏感列不提示,普通低基数列照常。
#[tokio::test]
async fn distinct_values_skip_sensitive_columns() {
    let st = state(Some(true)).await;
    let email = service::distinct_values(&st, &DbKind::Sqlite, "crm", "customers", "email", "TEXT").await.unwrap();
    assert_eq!(email, None);
    let status = service::distinct_values(&st, &DbKind::Sqlite, "crm", "customers", "status", "TEXT").await.unwrap();
    assert_eq!(status, Some(vec!["active".to_string(), "vip".to_string()]));
    // 遮罩关着:邮箱列也照常提示(两个不同的值,低基数)
    let st2 = state(Some(false)).await;
    let email = service::distinct_values(&st2, &DbKind::Sqlite, "crm", "customers", "email", "TEXT").await.unwrap();
    assert_eq!(email.map(|v| v.len()), Some(2));
}
