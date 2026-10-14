//! 各 SSL 模式下的真实连接行为。需要本机数据库,默认忽略:
//!   DATUM_TLS_PG_PLAIN=端口   未开 SSL 的 PG(用户 postgres、库 postgres、空密码)
//!   DATUM_TLS_PG_SSL=端口     开了 SSL(自签名证书)的 PG
//!   DATUM_TLS_MYSQL=端口      开了 SSL(自签名证书)的 MySQL(用户 root、空密码)
//!   DATUM_TLS_REDIS=端口      未开 TLS 的 Redis
//!   cargo test -p datum-core --test tls -- --ignored --test-threads=1 --nocapture

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;

fn port(var: &str) -> Option<u16> {
    std::env::var(var).ok().and_then(|p| p.parse().ok())
}

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-tls-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

fn conn(kind: DbKind, port: u16, user: &str, db: &str, ssl: SslMode) -> Connection {
    Connection {
        id: format!("tls-{kind:?}-{ssl:?}"),
        name: "tls".into(),
        kind,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        user: user.into(),
        database: db.into(),
        plaintext_password: None,
        file_path: None,
        ssl_mode: ssl,
    }
}

/// 建立连接并执行一条 SQL,返回第一行第一列。
async fn connect_and(st: &AppState, c: Connection, sql: &str) -> Result<Option<String>, String> {
    let (kind, id) = (c.kind.clone(), c.id.clone());
    commands::save_connection(st, c, Some(String::new())).map_err(|e| e.display())?;
    service::connect(st, &kind, &id)
        .await
        .map_err(|e| e.display())?;
    if sql.is_empty() {
        return Ok(None);
    }
    let r = service::query(st, &kind, &id, sql, None)
        .await
        .map_err(|e| e.display())?;
    Ok(r.rows.first().and_then(|row| row[0].clone()))
}

const PG_SSL_IN_USE: &str = "SELECT ssl::text FROM pg_stat_ssl WHERE pid = pg_backend_pid()";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_without_server_ssl() {
    let Some(p) = port("DATUM_TLS_PG_PLAIN") else {
        return;
    };
    let st = state();
    let c = |m| conn(DbKind::Pg, p, "postgres", "postgres", m);
    // 优先:服务器不支持就退回明文
    assert_eq!(
        connect_and(&st, c(SslMode::Prefer), PG_SSL_IN_USE)
            .await
            .unwrap()
            .as_deref(),
        Some("false")
    );
    assert_eq!(
        connect_and(&st, c(SslMode::Disable), PG_SSL_IN_USE)
            .await
            .unwrap()
            .as_deref(),
        Some("false")
    );
    // 必须加密:不能悄悄降级成明文
    let e = connect_and(&st, c(SslMode::Require), "").await.unwrap_err();
    println!("PG 未开 SSL + 必须加密 → {e}");
    assert!(connect_and(&st, c(SslMode::VerifyFull), "").await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_with_server_ssl() {
    let Some(p) = port("DATUM_TLS_PG_SSL") else {
        return;
    };
    let st = state();
    let c = |m| conn(DbKind::Pg, p, "postgres", "postgres", m);
    assert_eq!(
        connect_and(&st, c(SslMode::Prefer), PG_SSL_IN_USE)
            .await
            .unwrap()
            .as_deref(),
        Some("true")
    );
    assert_eq!(
        connect_and(&st, c(SslMode::Require), PG_SSL_IN_USE)
            .await
            .unwrap()
            .as_deref(),
        Some("true")
    );
    assert_eq!(
        connect_and(&st, c(SslMode::Disable), PG_SSL_IN_USE)
            .await
            .unwrap()
            .as_deref(),
        Some("false")
    );
    // 自签名证书过不了校验
    let e = connect_and(&st, c(SslMode::VerifyFull), "")
        .await
        .unwrap_err();
    println!("PG 自签名 + 校验证书 → {e}");
}

/// MySQL 当前会话的加密套件(空字符串 = 未加密)。
async fn mysql_cipher(st: &AppState, c: Connection) -> Result<String, String> {
    let (kind, id) = (c.kind.clone(), c.id.clone());
    commands::save_connection(st, c, Some(String::new())).map_err(|e| e.display())?;
    service::connect(st, &kind, &id)
        .await
        .map_err(|e| e.display())?;
    let r = service::query(
        st,
        &kind,
        &id,
        "SHOW SESSION STATUS LIKE 'Ssl_cipher'",
        None,
    )
    .await
    .map_err(|e| e.display())?;
    Ok(r.rows[0][1].clone().unwrap_or_default())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_with_server_ssl() {
    let Some(p) = port("DATUM_TLS_MYSQL") else {
        return;
    };
    let st = state();
    let c = |m| conn(DbKind::Mysql, p, "root", "mysql", m);
    let prefer = mysql_cipher(&st, c(SslMode::Prefer)).await.unwrap();
    let require = mysql_cipher(&st, c(SslMode::Require)).await.unwrap();
    let disable = mysql_cipher(&st, c(SslMode::Disable)).await.unwrap();
    println!("MySQL 加密套件:优先={prefer:?} 必须={require:?} 关闭={disable:?}");
    assert!(!prefer.is_empty() && !require.is_empty());
    assert!(disable.is_empty());
    let e = mysql_cipher(&st, c(SslMode::VerifyFull)).await.unwrap_err();
    println!("MySQL 自签名 + 校验证书 → {e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn redis_without_tls() {
    let Some(p) = port("DATUM_TLS_REDIS") else {
        return;
    };
    let st = state();
    let c = |m| conn(DbKind::Redis, p, "", "0", m);
    connect_and(&st, c(SslMode::Prefer), "").await.unwrap();
    connect_and(&st, c(SslMode::Disable), "").await.unwrap();
    let e = connect_and(&st, c(SslMode::Require), "").await.unwrap_err();
    println!("Redis 未开 TLS + 必须加密 → {e}");
}
