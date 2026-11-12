//! Redis 分析的真实执行。需要一个可随意写入的 Redis(淘汰策略设为 allkeys-lfu 才测热 key),默认忽略:
//!   DATUM_REDIS_ANALYSIS=端口   例如 docker run --rm -p 56379:6379 redis:6-alpine --maxmemory-policy allkeys-lfu
//!   cargo test -p datum-core --test redis_analysis -- --ignored --nocapture
//! 用库 15,开始前清空。

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;
use std::time::Duration;

async fn setup() -> Option<(AppState, String)> {
    let port: u16 = std::env::var("DATUM_REDIS_ANALYSIS").ok()?.parse().ok()?;
    let dir = std::env::temp_dir().join(format!("datum-rds-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let st = AppState::new(dir);
    let c = Connection {
        id: "rds-analysis".into(),
        name: "rds".into(),
        kind: DbKind::Redis,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        user: String::new(),
        database: "15".into(),
        plaintext_password: None,
        file_path: None,
        ssl_mode: SslMode::Disable,
        uri: None,
        auth_source: None,
        ssh: None,
    };
    commands::save_connection(&st, c, Some(String::new())).unwrap();
    service::connect(&st, &DbKind::Redis, "rds-analysis").await.unwrap();
    service::redis_exec(&st, "rds-analysis", "FLUSHDB").await.unwrap();
    Some((st, "rds-analysis".into()))
}

async fn wait_done(st: &AppState, id: &str) -> datum_core::rds::analysis::Report {
    for _ in 0..200 {
        let r = service::redis_analysis_report(st, id).await.unwrap();
        if r.done {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("分析没有结束");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn scans_and_summarizes() {
    let Some((st, id)) = setup().await else {
        return;
    };
    let exec = |cmd: String| {
        let st = &st;
        let id = id.clone();
        async move { service::redis_exec(st, &id, &cmd).await.unwrap() }
    };
    for i in 0..300 {
        exec(format!("SET user:{i} {}", "x".repeat(20))).await;
    }
    for i in 0..50 {
        exec(format!("SET session:{i} v EX 600")).await;
    }
    exec(format!("SET blob {}", "y".repeat(50_000))).await;
    exec("RPUSH queue a b c".into()).await;
    // 让 hot:key 的访问频度明显更高
    exec("SET hot:key 1".into()).await;
    for _ in 0..200 {
        exec("GET hot:key".into()).await;
    }

    let o = service::redis_overview(&st, &id).await.unwrap();
    assert_eq!(o.keys, Some(353));
    assert_eq!(o.expires, Some(50));
    assert!(o.used_memory.is_some());

    service::redis_analysis_start(&st, &id).await.unwrap();
    let r = wait_done(&st, &id).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.scanned, 353);
    assert!(!r.truncated && !r.cancelled);
    assert!(r.memory_supported);
    assert_eq!(r.biggest[0].key, "blob");
    assert!(r.biggest[0].bytes.unwrap() > 50_000);
    let user = r.prefixes.iter().find(|p| p.name == "user").unwrap();
    assert_eq!(user.keys, 300);
    assert_eq!(r.ttl.persistent, 303);
    assert_eq!(r.ttl.within_hour, 50);
    assert!(r.types.iter().any(|t| t.name == "list" && t.keys == 1));
    if r.lfu {
        assert_eq!(r.hottest[0].key, "hot:key");
    } else {
        assert!(r.hottest.is_empty());
    }

    let slow = service::redis_slowlog(&st, &id).await.unwrap();
    // 慢日志的内容取决于实例,这里只验证能取回并解析
    let _ = slow;
    service::redis_exec(&st, &id, "FLUSHDB").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn cancel_stops_early() {
    let Some((st, id)) = setup().await else {
        return;
    };
    // 2 万个 key:够扫一阵,来得及取消
    let mut batch = String::from("MSET");
    for i in 0..20_000 {
        batch.push_str(&format!(" k:{i} v"));
        if i % 2_000 == 1_999 {
            service::redis_exec(&st, &id, &batch).await.unwrap();
            batch = String::from("MSET");
        }
    }
    service::redis_analysis_start(&st, &id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    service::redis_analysis_cancel(&st, &id).await;
    let r = wait_done(&st, &id).await;
    assert!(r.cancelled);
    assert!(r.scanned < 20_000, "scanned {}", r.scanned);
    service::redis_exec(&st, &id, "FLUSHDB").await.unwrap();
}
