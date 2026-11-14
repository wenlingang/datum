//! 建连超时:服务器接受 TCP 却一直不回应握手(或主机不可达)时,不等系统的 TCP 超时,
//! 在建连时限内报「连接超时」。用本机一个只接受连接、从不回话的端口模拟,默认就跑。

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;
use std::time::{Duration, Instant};

/// 接受连接但从不回话;连接留着不关。
async fn silent_server() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = vec![];
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });
    port
}

async fn times_out(kind: DbKind, ssl: SslMode) {
    let dir = std::env::temp_dir().join(format!("datum-ctimeout-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let st = AppState::new(dir.clone());
    let port = silent_server().await;
    let id = format!("ct-{kind:?}");
    let c = Connection {
        id: id.clone(),
        name: "ct".into(),
        kind,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        user: "u".into(),
        database: "d".into(),
        plaintext_password: None,
        file_path: None,
        ssl_mode: ssl,
        uri: None,
        auth_source: None,
        ssh: None,
    };
    commands::save_connection(&st, c, Some(String::new())).unwrap();
    let t0 = Instant::now();
    let r = tokio::time::timeout(Duration::from_secs(40), service::connect(&st, &kind, &id)).await.expect("建连没有超时");
    assert!(r.is_err());
    assert!(t0.elapsed() < Duration::from_secs(25), "{kind:?} 超时太久 {:?}", t0.elapsed());
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_connect_times_out() {
    times_out(DbKind::Pg, SslMode::Disable).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mysql_connect_times_out_without_plaintext_retry() {
    // 「优先」:先试加密,超时后不再换明文重试(否则要等两倍时间)
    times_out(DbKind::Mysql, SslMode::Prefer).await;
}
