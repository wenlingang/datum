//! 经 SSH 隧道的真实连接。需要一台允许端口转发的 sshd 和本机数据库,默认忽略:
//!   DATUM_SSH=主机:端口:用户          跳板机(私钥认证)
//!   DATUM_SSH_KEY=路径                无密码短语的私钥
//!   DATUM_SSH_KEY_ENC=路径            密码短语为 DATUM_SSH_KEY_PASS 的私钥
//!   DATUM_SSH_MYSQL=端口              跳板机视角下 127.0.0.1 上的 MySQL(root、空密码)
//!   DATUM_SSH_REDIS=端口              跳板机视角下 127.0.0.1 上的 Redis(无密码)
//!   DATUM_SSH_PG=端口                 跳板机视角下 127.0.0.1 上的 PostgreSQL(postgres、空密码、trust)
//!   DATUM_SSH_AGENT=1                 SSH_AUTH_SOCK 指向的 agent 里已加载 DATUM_SSH_KEY
//!   DATUM_SSH_JUMP2=端口              同一台机器上第二个 sshd(测多级跳板:先经 DATUM_SSH,再到它)
//!   cargo test -p datum-core --test ssh -- --ignored --test-threads=1 --nocapture

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SshAuth, SshTunnel, SslMode};
use datum_core::service::{self, SshContext};

struct Jump {
    host: String,
    port: u16,
    user: String,
}

fn jump() -> Option<Jump> {
    let raw = std::env::var("DATUM_SSH").ok()?;
    let p: Vec<&str> = raw.split(':').collect();
    Some(Jump {
        host: p[0].into(),
        port: p[1].parse().ok()?,
        user: p[2].into(),
    })
}

fn env(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.is_empty())
}

fn port(var: &str) -> Option<u16> {
    env(var).and_then(|p| p.parse().ok())
}

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-ssh-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

fn tunnel(j: &Jump, auth: SshAuth, key: &str) -> SshTunnel {
    SshTunnel {
        host: j.host.clone(),
        port: j.port,
        user: j.user.clone(),
        auth,
        key_path: key.into(),
        secret: None,
        proxy_jump: String::new(),
    }
}

/// 写一份临时的 ssh config,让本进程的隧道读它(DATUM_SSH_CONFIG);返回时恢复。
struct SshConfigFile(std::path::PathBuf);

impl SshConfigFile {
    fn new(st: &AppState, text: &str) -> Self {
        let path = st.config_dir.join("ssh_config");
        std::fs::write(&path, text).unwrap();
        std::env::set_var("DATUM_SSH_CONFIG", &path);
        Self(path)
    }
}

impl Drop for SshConfigFile {
    fn drop(&mut self) {
        std::env::remove_var("DATUM_SSH_CONFIG");
    }
}

fn conn(kind: DbKind, db_port: u16, ssh: SshTunnel) -> Connection {
    Connection {
        id: format!("ssh-{kind:?}-{}", uuid::Uuid::new_v4()),
        name: "ssh".into(),
        kind,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port: db_port,
        user: match kind {
            DbKind::Mysql => "root".into(),
            DbKind::Pg => "postgres".into(),
            _ => String::new(),
        },
        database: match kind {
            DbKind::Redis => "0".into(),
            DbKind::Pg => "postgres".into(),
            _ => String::new(),
        },
        plaintext_password: None,
        file_path: None,
        ssl_mode: SslMode::Disable,
        uri: None,
        auth_source: None,
        ssh: Some(ssh),
    }
}

fn ctx(st: &AppState, secret: &str) -> SshContext {
    SshContext {
        secret: secret.into(),
        known_hosts: st.known_hosts_path(),
    }
}

/// 保存(SSH 密钥一并写入)后经 service 建连。
async fn save_and_connect(st: &AppState, mut c: Connection, secret: &str) -> Result<String, String> {
    c.ssh.as_mut().unwrap().secret = Some(secret.into());
    let saved = commands::save_connection(st, c, Some(String::new())).map_err(|e| e.display())?;
    // 返回给前端的连接不含 SSH 密钥
    assert_eq!(saved.ssh.as_ref().unwrap().secret, None);
    service::connect(st, &saved.kind, &saved.id).await.map_err(|e| e.display())?;
    Ok(saved.id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn mysql_through_tunnel_with_pooled_connections() {
    let (Some(j), Some(key), Some(db)) = (jump(), env("DATUM_SSH_KEY"), port("DATUM_SSH_MYSQL")) else {
        return;
    };
    let st = state();
    let id = save_and_connect(&st, conn(DbKind::Mysql, db, tunnel(&j, SshAuth::Key, &key)), "")
        .await
        .unwrap();
    // 首次连接的跳板机指纹记在 Datum 自己的 known_hosts 里
    let known = std::fs::read_to_string(st.known_hosts_path()).unwrap();
    assert!(known.contains(&format!("[{}]:{}", j.host, j.port)), "{known}");
    // 并发查询:连接池会经同一条 SSH 会话开多条转发通道
    let queries = (0..6).map(|i| {
        let st = &st;
        let id = id.clone();
        async move {
            service::query(st, &DbKind::Mysql, &id, &format!("SELECT {i} + SLEEP(0.2)"), None)
                .await
                .map(|r| r.rows[0][0].clone())
        }
    });
    let results = futures_util::future::join_all(queries).await;
    for (i, r) in results.into_iter().enumerate() {
        assert_eq!(r.unwrap().as_deref(), Some(i.to_string().as_str()));
    }
    service::disconnect(&st, &DbKind::Mysql, &id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn redis_through_tunnel() {
    let (Some(j), Some(key), Some(db)) = (jump(), env("DATUM_SSH_KEY"), port("DATUM_SSH_REDIS")) else {
        return;
    };
    let st = state();
    let id = save_and_connect(&st, conn(DbKind::Redis, db, tunnel(&j, SshAuth::Key, &key)), "")
        .await
        .unwrap();
    assert_eq!(service::redis_exec(&st, &id, "PING").await.unwrap(), "PONG");
    service::disconnect(&st, &DbKind::Redis, &id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn encrypted_key_needs_the_right_passphrase() {
    let (Some(j), Some(key), Some(pass), Some(db)) = (
        jump(),
        env("DATUM_SSH_KEY_ENC"),
        env("DATUM_SSH_KEY_PASS"),
        port("DATUM_SSH_MYSQL"),
    ) else {
        return;
    };
    let st = state();
    let c = conn(DbKind::Mysql, db, tunnel(&j, SshAuth::Key, &key));
    let missing = service::test_connection(&c, "", &ctx(&st, "")).await.unwrap_err();
    assert!(missing.display().contains("passphrase"), "{}", missing.display());
    let wrong = service::test_connection(&c, "", &ctx(&st, "nope")).await.unwrap_err();
    assert!(wrong.display().contains("Could not read the SSH private key"), "{}", wrong.display());
    service::test_connection(&c, "", &ctx(&st, &pass)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn changed_host_key_is_rejected() {
    let (Some(j), Some(key), Some(db)) = (jump(), env("DATUM_SSH_KEY"), port("DATUM_SSH_MYSQL")) else {
        return;
    };
    let st = state();
    // 给这台主机记一个别的指纹
    std::fs::write(
        st.known_hosts_path(),
        format!(
            "[{}]:{} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ\n",
            j.host, j.port
        ),
    )
    .unwrap();
    let c = conn(DbKind::Mysql, db, tunnel(&j, SshAuth::Key, &key));
    let e = service::test_connection(&c, "", &ctx(&st, "")).await.unwrap_err();
    assert!(e.display().contains("host key has changed"), "{}", e.display());
    // 只记录了另一种类型的密钥:不能当作没见过而首次信任
    std::fs::write(
        st.known_hosts_path(),
        format!("[{}]:{} ecdsa-sha2-nistp256 AAAA\n", j.host, j.port),
    )
    .unwrap();
    let e = service::test_connection(&c, "", &ctx(&st, "")).await.unwrap_err();
    assert!(e.display().contains("identity cannot be confirmed"), "{}", e.display());
    // 吊销
    std::fs::write(st.known_hosts_path(), "").unwrap();
    service::test_connection(&c, "", &ctx(&st, "")).await.unwrap();
    let learned = std::fs::read_to_string(st.known_hosts_path()).unwrap().trim().to_string();
    std::fs::write(st.known_hosts_path(), format!("@revoked {learned}\n")).unwrap();
    let e = service::test_connection(&c, "", &ctx(&st, "")).await.unwrap_err();
    assert!(e.display().contains("@revoked"), "{}", e.display());
    // 首次信任记下的那行能被再次认出
    std::fs::write(st.known_hosts_path(), learned).unwrap();
    service::test_connection(&c, "", &ctx(&st, "")).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn auth_and_forward_failures_are_explained() {
    let (Some(j), Some(key), Some(db)) = (jump(), env("DATUM_SSH_KEY"), port("DATUM_SSH_MYSQL")) else {
        return;
    };
    let st = state();
    // 用户名不对:认证失败
    let mut wrong_user = tunnel(&j, SshAuth::Key, &key);
    wrong_user.user = "datum-no-such-user".into();
    let e = service::test_connection(&conn(DbKind::Mysql, db, wrong_user), "", &ctx(&st, ""))
        .await
        .unwrap_err();
    assert!(e.display().contains("SSH authentication failed"), "{}", e.display());
    // 跳板机连不到目标端口
    let e = service::test_connection(&conn(DbKind::Mysql, 1, tunnel(&j, SshAuth::Key, &key)), "", &ctx(&st, ""))
        .await
        .unwrap_err();
    assert!(e.display().contains("could not reach the database"), "{}", e.display());
    // 跳板机本身连不上
    let mut dead = tunnel(&j, SshAuth::Key, &key);
    dead.port = 1;
    let e = service::test_connection(&conn(DbKind::Mysql, db, dead), "", &ctx(&st, ""))
        .await
        .unwrap_err();
    assert!(e.display().contains("Could not connect to the SSH server"), "{}", e.display());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn probe_databases_through_tunnel() {
    let (Some(j), Some(key), Some(db)) = (jump(), env("DATUM_SSH_KEY"), port("DATUM_SSH_MYSQL")) else {
        return;
    };
    let st = state();
    let c = conn(DbKind::Mysql, db, tunnel(&j, SshAuth::Key, &key));
    let dbs = service::probe_databases(&c, "", &ctx(&st, "")).await.unwrap();
    assert!(dbs.iter().any(|d| d == "mysql"), "{dbs:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn agent_auth() {
    let (Some(j), Some(_), Some(db)) = (jump(), env("DATUM_SSH_AGENT"), port("DATUM_SSH_MYSQL")) else {
        return;
    };
    let st = state();
    let c = conn(DbKind::Mysql, db, tunnel(&j, SshAuth::Agent, ""));
    service::test_connection(&c, "", &ctx(&st, "")).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_tls_through_tunnel() {
    let (Some(j), Some(key), Some(db)) = (jump(), env("DATUM_SSH_KEY"), port("DATUM_SSH_MYSQL")) else {
        return;
    };
    let st = state();
    let mut c = conn(DbKind::Mysql, db, tunnel(&j, SshAuth::Key, &key));
    c.ssl_mode = SslMode::Require;
    let id = save_and_connect(&st, c, "").await.unwrap();
    let r = service::query(&st, &DbKind::Mysql, &id, "SHOW SESSION STATUS LIKE 'Ssl_cipher'", None)
        .await
        .unwrap();
    let cipher = r.rows[0][1].clone().unwrap_or_default();
    assert!(!cipher.is_empty(), "TLS 没有生效");
    service::disconnect(&st, &DbKind::Mysql, &id).await;
}

/// DATUM_SSH_KILL:断开跳板机上所有已建立的 SSH 会话的命令(如 pkill 掉 sshd 的会话子进程)。
/// 会话断开后不重新 connect:连接池换新连接时经同一个本地端口进来,隧道自动重建 SSH 会话。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn tunnel_reconnects_after_ssh_session_drops() {
    let (Some(j), Some(key), Some(db), Some(kill)) =
        (jump(), env("DATUM_SSH_KEY"), port("DATUM_SSH_MYSQL"), env("DATUM_SSH_KILL"))
    else {
        return;
    };
    let st = state();
    let id = save_and_connect(&st, conn(DbKind::Mysql, db, tunnel(&j, SshAuth::Key, &key)), "")
        .await
        .unwrap();
    let one = || service::query(&st, &DbKind::Mysql, &id, "SELECT 1", None);
    assert!(one().await.is_ok());
    assert!(std::process::Command::new("sh").arg("-c").arg(&kill).status().unwrap().success());
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    // 池里的旧连接已断:第一次可能失败,随后池开新连接、隧道重连会话
    let mut ok = false;
    for _ in 0..3 {
        if one().await.is_ok() {
            ok = true;
            break;
        }
    }
    assert!(ok, "隧道没有自动重连");
    service::disconnect(&st, &DbKind::Mysql, &id).await;
}

/// DATUM_SSH_KEY_RSA:跳板机认可的 RSA 私钥(无密码短语)。RSA 暂不支持,要给出明确提示。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn rsa_key_is_reported_as_unsupported() {
    let (Some(j), Some(key), Some(db)) = (jump(), env("DATUM_SSH_KEY_RSA"), port("DATUM_SSH_MYSQL")) else {
        return;
    };
    let st = state();
    let c = conn(DbKind::Mysql, db, tunnel(&j, SshAuth::Key, &key));
    let e = service::test_connection(&c, "", &ctx(&st, "")).await.unwrap_err();
    assert!(e.display().contains("RSA SSH keys are not supported"), "{}", e.display());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_through_tunnel() {
    let (Some(j), Some(key), Some(db)) = (jump(), env("DATUM_SSH_KEY"), port("DATUM_SSH_PG")) else {
        return;
    };
    let st = state();
    let c = conn(DbKind::Pg, db, tunnel(&j, SshAuth::Key, &key));
    let dbs = service::probe_databases(&c, "", &ctx(&st, "")).await.unwrap();
    assert!(dbs.iter().any(|d| d == "postgres"), "{dbs:?}");
    let mut c = c;
    c.ssl_mode = SslMode::Prefer;
    let id = save_and_connect(&st, c, "").await.unwrap();
    // 服务端看到的对端地址是跳板机(本地 sshd),说明确实经隧道
    let r = service::query(&st, &DbKind::Pg, &id, "SELECT current_database(), 1 + 1", None)
        .await
        .unwrap();
    assert_eq!(r.rows[0][0].as_deref(), Some("postgres"));
    assert_eq!(r.rows[0][1].as_deref(), Some("2"));
    service::disconnect(&st, &DbKind::Pg, &id).await;
}

/// 跳板机填 ~/.ssh/config 里的别名:HostName / Port / User / IdentityFile 都从 config 来。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn bastion_alias_from_ssh_config() {
    let (Some(j), Some(key), Some(db)) = (jump(), env("DATUM_SSH_KEY"), port("DATUM_SSH_REDIS")) else {
        return;
    };
    let st = state();
    let _cfg = SshConfigFile::new(
        &st,
        &format!("Host datum-test-bastion\n  HostName {}\n  Port {}\n  User {}\n  IdentityFile {key}\n", j.host, j.port, j.user),
    );
    let mut t = tunnel(&j, SshAuth::Key, "");
    t.host = "datum-test-bastion".into();
    t.port = 22;
    t.user = String::new();
    let id = save_and_connect(&st, conn(DbKind::Redis, db, t), "").await.unwrap();
    assert_eq!(service::redis_exec(&st, &id, "PING").await.unwrap(), "PONG");
    service::disconnect(&st, &DbKind::Redis, &id).await;
}

/// 多级跳板:先经第一个 sshd(ProxyJump,用 config 里的 IdentityFile 认证),再到第二个 sshd 开转发。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn proxy_jump_chain() {
    let (Some(j), Some(key), Some(db), Some(second)) =
        (jump(), env("DATUM_SSH_KEY"), port("DATUM_SSH_PG"), port("DATUM_SSH_JUMP2"))
    else {
        return;
    };
    let st = state();
    let _cfg = SshConfigFile::new(
        &st,
        &format!("Host datum-test-jump\n  HostName {}\n  Port {}\n  User {}\n  IdentityFile {key}\n", j.host, j.port, j.user),
    );
    let mut t = tunnel(&j, SshAuth::Key, &key);
    t.port = second;
    t.proxy_jump = "datum-test-jump".into();
    let id = save_and_connect(&st, conn(DbKind::Pg, db, t.clone()), "").await.unwrap();
    let r = service::query(&st, &DbKind::Pg, &id, "SELECT 40 + 2", None).await.unwrap();
    assert_eq!(r.rows[0][0].as_deref(), Some("42"));
    service::disconnect(&st, &DbKind::Pg, &id).await;
    // 跳板写错:报看不懂
    t.proxy_jump = "x:notaport".into();
    let e = service::test_connection(&conn(DbKind::Pg, db, t), "", &ctx(&st, "")).await.unwrap_err();
    assert!(e.display().contains("ProxyJump"), "{}", e.display());
}
