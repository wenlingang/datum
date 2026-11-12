//! SSH 隧道:经跳板机把数据库端口转发到本机 `127.0.0.1` 的随机端口,驱动改连本地端口。
//!
//! - 一个连接一条 SSH 会话;本地端口每来一个 TCP 连接就开一条 direct-tcpip 通道
//!   (MySQL 连接池会陆续开多条)。会话断了,下一条连接进来时自动重连。
//! - 主机指纹:先查 `~/.ssh/known_hosts`,再查 Datum 自己的 known_hosts(配置目录下);
//!   指纹不一致直接拒绝,两处都没有记录的主机首次信任并记到 Datum 自己的文件里,
//!   不改动用户的 `~/.ssh`。
//! - 证书校验仍按数据库的原主机名进行([`Target::tls_host`]),驱动只是改连本地端口。

use crate::core::connection::{Connection, SshAuth, SshTunnel};
use crate::error::{AppError, AppResult, ErrorKind};
use russh::client::{self, Handle};
use russh::keys::{self, HashAlg, PrivateKeyWithHashAlg};
use rust_i18n::t;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::task::{JoinHandle, JoinSet};

/// 建立 SSH 会话(TCP + 握手 + 认证)的总时限。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 空闲时的心跳间隔:防止跳板机或中间的 NAT 把长连接掐掉。
const KEEPALIVE: Duration = Duration::from_secs(30);

/// 驱动该去连的地址:直连时就是数据库本身;走隧道时是本地转发端口,
/// `tls_host` 仍是数据库的原主机名(证书校验、SNI 用)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub tls_host: String,
}

impl Target {
    pub fn direct(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
            tls_host: host.to_string(),
        }
    }

    /// 是否经由本地转发端口(需要把 TLS 主机名与连接地址分开)。
    pub fn tunneled(&self) -> bool {
        self.host != self.tls_host
    }
}

/// 建隧道需要的上下文:SSH 密钥与 Datum 自己的 known_hosts 文件。
#[derive(Debug, Clone, Default)]
pub struct SshContext {
    pub secret: String,
    pub known_hosts: PathBuf,
}

/// 按连接配置得到驱动要连的地址;走 SSH 时先建好隧道,隧道随返回值存活。
pub async fn resolve(conn: &Connection, ctx: &SshContext) -> AppResult<(Target, Option<Tunnel>)> {
    match conn.tunnel() {
        None => Ok((Target::direct(&conn.host, conn.port), None)),
        Some(ssh) => {
            let tunnel = Tunnel::open(ssh, &ctx.secret, &conn.host, conn.port, &ctx.known_hosts).await?;
            let target = Target {
                host: "127.0.0.1".into(),
                port: tunnel.local_port,
                tls_host: conn.host.clone(),
            };
            Ok((target, Some(tunnel)))
        }
    }
}

fn ssh_err(key: &str) -> AppError {
    AppError::new(ErrorKind::Connection, t!(key))
}

// ---------- 主机指纹 ----------

#[derive(Debug, PartialEq, Eq)]
enum HostKeyStatus {
    Known,
    Unknown,
    /// 记录里有这台主机但指纹不同:(文件, 行号)
    Changed(PathBuf, usize),
    /// 文件读不了,或这台主机的记录解析不了:无法确认,按不可信处理
    Unreadable(PathBuf, String),
}

/// 依次在各 known_hosts 文件里查这台主机的指纹;文件不存在的跳过。
/// 出错时不当作「没见过」放行,否则一条读不了的记录就能让换了指纹的主机被重新信任。
fn host_key_status(host: &str, port: u16, key: &keys::PublicKey, files: &[PathBuf]) -> HostKeyStatus {
    for file in files {
        if !file.exists() {
            continue;
        }
        match keys::check_known_hosts_path(host, port, key, file) {
            Ok(true) => return HostKeyStatus::Known,
            Ok(false) => {}
            Err(keys::Error::KeyChanged { line }) => return HostKeyStatus::Changed(file.clone(), line),
            Err(e) => return HostKeyStatus::Unreadable(file.clone(), e.to_string()),
        }
    }
    HostKeyStatus::Unknown
}

fn known_hosts_files(app_file: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(home) = dirs::home_dir() {
        files.push(home.join(".ssh").join("known_hosts"));
    }
    files.push(app_file.to_path_buf());
    files
}

struct HostKeyCheck {
    host: String,
    port: u16,
    app_known_hosts: PathBuf,
    /// 拒绝时的原因(握手失败后据此给出可读的报错)
    rejected: Arc<std::sync::Mutex<Option<AppError>>>,
}

impl client::Handler for HostKeyCheck {
    type Error = russh::Error;

    async fn check_server_key(&mut self, server_key: &keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        // 主机证书(CA 签发)按其中的公钥比对 known_hosts
        let owned;
        let key = match server_key {
            keys::PublicKeyOrCertificate::PublicKey { key, .. } => key,
            keys::PublicKeyOrCertificate::Certificate(cert) => {
                owned = keys::PublicKey::new(cert.public_key().clone(), "");
                &owned
            }
        };
        let files = known_hosts_files(&self.app_known_hosts);
        match host_key_status(&self.host, self.port, key, &files) {
            HostKeyStatus::Known => Ok(true),
            HostKeyStatus::Unknown => {
                // 首次连接:信任并记下,之后指纹变了就会被拦住。记不下来就不连:
                // 否则每次都像首次连接,指纹被换了也发现不了
                match keys::known_hosts::learn_known_hosts_path(&self.host, self.port, key, &self.app_known_hosts) {
                    Ok(()) => Ok(true),
                    Err(e) => {
                        *self.rejected.lock().unwrap() = Some(
                            ssh_err("core.err.ssh_known_hosts_unreadable")
                                .with_detail(format!("{}: {e}", self.app_known_hosts.display())),
                        );
                        Ok(false)
                    }
                }
            }
            HostKeyStatus::Unreadable(file, e) => {
                *self.rejected.lock().unwrap() = Some(
                    ssh_err("core.err.ssh_known_hosts_unreadable").with_detail(format!("{}: {e}", file.display())),
                );
                Ok(false)
            }
            HostKeyStatus::Changed(file, line) => {
                *self.rejected.lock().unwrap() = Some(
                    ssh_err("core.err.ssh_host_key_changed").with_detail(format!(
                        "{}:{} ({}:{})",
                        self.host,
                        self.port,
                        file.display(),
                        line
                    )),
                );
                Ok(false)
            }
        }
    }
}

// ---------- 会话 ----------

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

/// 私钥路径:填了就用填的;没填就找 ~/.ssh 下的默认私钥。
fn key_path(cfg: &SshTunnel) -> AppResult<PathBuf> {
    let given = cfg.key_path.trim();
    if !given.is_empty() {
        return Ok(expand_home(given));
    }
    let home = dirs::home_dir().ok_or_else(|| ssh_err("core.err.ssh_key_not_found"))?;
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|n| home.join(".ssh").join(n))
        .find(|p| p.exists())
        .ok_or_else(|| ssh_err("core.err.ssh_key_not_found"))
}

fn load_key(cfg: &SshTunnel, passphrase: &str) -> AppResult<keys::PrivateKey> {
    let path = key_path(cfg)?;
    let pass = (!passphrase.is_empty()).then_some(passphrase);
    keys::load_secret_key(&path, pass).map_err(|e| {
        let key = match e {
            keys::Error::KeyIsEncrypted => "core.err.ssh_key_encrypted",
            keys::Error::IO(_) => "core.err.ssh_key_not_found",
            _ => "core.err.ssh_key_invalid",
        };
        ssh_err(key).with_detail(format!("{}: {e}", path.display()))
    })
}

type Session = Handle<HostKeyCheck>;

async fn rsa_hash(handle: &Session) -> Option<HashAlg> {
    handle.best_supported_rsa_hash().await.ok().flatten().flatten()
}

/// 用 agent 里的每把钥匙依次试,直到有一把通过。
async fn auth_with_agent<S>(handle: &mut Session, user: &str, mut agent: keys::agent::client::AgentClient<S>) -> AppResult<bool>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| ssh_err("core.err.ssh_agent_unavailable").with_detail(e.to_string()))?;
    let hash = rsa_hash(handle).await;
    // RSA 暂不支持(见 core.err.ssh_rsa_unsupported):只用其余的钥匙
    let usable: Vec<_> = identities.iter().filter(|id| !id.public_key().algorithm().is_rsa()).collect();
    if usable.is_empty() && !identities.is_empty() {
        return Err(ssh_err("core.err.ssh_rsa_unsupported"));
    }
    for id in usable {
        let key = id.public_key().into_owned();
        match handle.authenticate_publickey_with(user, key, hash, &mut agent).await {
            Ok(r) if r.success() => return Ok(true),
            _ => continue,
        }
    }
    Ok(false)
}

#[cfg(unix)]
async fn agent_auth(handle: &mut Session, user: &str) -> AppResult<bool> {
    let agent = keys::agent::client::AgentClient::connect_env()
        .await
        .map_err(|e| ssh_err("core.err.ssh_agent_unavailable").with_detail(e.to_string()))?;
    auth_with_agent(handle, user, agent).await
}

#[cfg(windows)]
async fn agent_auth(handle: &mut Session, user: &str) -> AppResult<bool> {
    use keys::agent::client::AgentClient;
    // 先试 Windows 自带的 OpenSSH agent,再试 Pageant
    match AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
        Ok(agent) => auth_with_agent(handle, user, agent).await,
        Err(_) => {
            let agent = AgentClient::connect_pageant()
                .await
                .map_err(|e| ssh_err("core.err.ssh_agent_unavailable").with_detail(e.to_string()))?;
            auth_with_agent(handle, user, agent).await
        }
    }
}

async fn authenticate(handle: &mut Session, cfg: &SshTunnel, secret: &str) -> AppResult<()> {
    let user = cfg.user.as_str();
    let failed = |e: russh::Error| ssh_err("core.err.ssh_auth_failed").with_detail(e.to_string());
    let ok = match cfg.auth {
        SshAuth::Password => handle.authenticate_password(user, secret).await.map_err(failed)?.success(),
        SshAuth::Key => {
            let key = load_key(cfg, secret)?;
            if key.algorithm().is_rsa() {
                return Err(ssh_err("core.err.ssh_rsa_unsupported"));
            }
            let hash = rsa_hash(handle).await;
            handle
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(failed)?
                .success()
        }
        SshAuth::Agent => agent_auth(handle, user).await?,
    };
    if ok {
        Ok(())
    } else {
        Err(ssh_err("core.err.ssh_auth_failed").with_detail(format!("{user}@{}:{}", cfg.host, cfg.port)))
    }
}

/// 连上跳板机并完成认证。
async fn open_session(cfg: &SshTunnel, secret: &str, app_known_hosts: &Path) -> AppResult<Session> {
    let config = Arc::new(client::Config {
        keepalive_interval: Some(KEEPALIVE),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    });
    let rejected = Arc::new(std::sync::Mutex::new(None));
    let handler = HostKeyCheck {
        host: cfg.host.clone(),
        port: cfg.port,
        app_known_hosts: app_known_hosts.to_path_buf(),
        rejected: rejected.clone(),
    };
    let addr = (cfg.host.clone(), cfg.port);
    let connect = async {
        let mut handle = client::connect(config, addr, handler).await.map_err(|e| {
            rejected
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| ssh_err("core.err.ssh_connect_failed").with_detail(e.to_string()))
        })?;
        authenticate(&mut handle, cfg, secret).await?;
        Ok(handle)
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(r) => r,
        Err(_) => Err(ssh_err("core.err.ssh_timeout").with_detail(format!("{}:{}", cfg.host, cfg.port))),
    }
}

// ---------- 隧道 ----------

/// 隧道两端的参数,会话断了用它重连。
struct Route {
    ssh: SshTunnel,
    secret: String,
    known_hosts: PathBuf,
    target_host: String,
    target_port: u16,
}

impl Route {
    async fn open_channel(&self, session: &Mutex<Session>, from_port: u16) -> AppResult<russh::Channel<client::Msg>> {
        let mut guard = session.lock().await;
        if guard.is_closed() {
            *guard = open_session(&self.ssh, &self.secret, &self.known_hosts).await?;
        }
        match self.direct_tcpip(&guard, from_port).await {
            Ok(ch) => Ok(ch),
            // 会话在检查之后才断开:重连一次再试
            Err(_) if guard.is_closed() => {
                *guard = open_session(&self.ssh, &self.secret, &self.known_hosts).await?;
                self.direct_tcpip(&guard, from_port).await.map_err(|e| self.unreachable(e))
            }
            Err(e) => Err(self.unreachable(e)),
        }
    }

    async fn direct_tcpip(&self, s: &Session, from_port: u16) -> Result<russh::Channel<client::Msg>, russh::Error> {
        s.channel_open_direct_tcpip(self.target_host.clone(), self.target_port as u32, "127.0.0.1", from_port as u32)
            .await
    }

    fn unreachable(&self, e: russh::Error) -> AppError {
        ssh_err("core.err.ssh_forward_failed").with_detail(format!("{}:{} ({e})", self.target_host, self.target_port))
    }
}

/// 一条在用的隧道:丢弃即关闭(本地端口、SSH 会话与其上所有转发一并结束)。
///
/// 已知暴露面:转发端口只监听 127.0.0.1,但不鉴别来访者,同一台机器上的其他进程
/// (含其他用户)在隧道存活期间也能经它连到数据库,与 `ssh -L` 的默认行为相同;
/// 数据库自身的认证仍然生效。PG / Redis 可改为把 SSH 通道直接交给驱动(不开端口),
/// MySQL 驱动只接受 TCP,见 TODO。
pub struct Tunnel {
    pub local_port: u16,
    task: JoinHandle<()>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Tunnel {
    /// 连上跳板机,确认它能连到目标,再在本机开转发端口。
    pub async fn open(
        ssh: &SshTunnel,
        secret: &str,
        target_host: &str,
        target_port: u16,
        known_hosts: &Path,
    ) -> AppResult<Self> {
        let route = Arc::new(Route {
            ssh: ssh.clone(),
            secret: secret.to_string(),
            known_hosts: known_hosts.to_path_buf(),
            target_host: target_host.to_string(),
            target_port,
        });
        let session = Mutex::new(open_session(ssh, secret, known_hosts).await?);
        // 先试开一条通道:目标不可达时在这里给出明确报错,而不是让驱动报一个「连接被重置」
        drop(route.open_channel(&session, 0).await?);
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| ssh_err("core.err.ssh_forward_failed").with_detail(e.to_string()))?;
        let local_port = listener
            .local_addr()
            .map_err(|e| ssh_err("core.err.ssh_forward_failed").with_detail(e.to_string()))?
            .port();
        let session = Arc::new(session);
        let task = tokio::spawn(async move {
            // 转发任务都挂在这里:隧道关闭(本任务被 abort)时一起结束
            let mut forwards = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut socket, peer)) = accepted else { continue };
                        let (route, session) = (route.clone(), session.clone());
                        forwards.spawn(async move {
                            let Ok(channel) = route.open_channel(&session, peer.port()).await else { return };
                            let mut stream = channel.into_stream();
                            let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                        });
                    }
                    Some(_) = forwards.join_next(), if !forwards.is_empty() => {}
                }
            }
        });
        Ok(Self { local_port, task })
    }
}

/// 已建立连接所用的隧道,按连接 id 存放;断开连接时移除即关闭。
#[derive(Default)]
pub struct TunnelPool {
    tunnels: Mutex<HashMap<String, Tunnel>>,
}

impl TunnelPool {
    /// 存放(或替换)某连接的隧道;传 None 则只移除旧的。
    pub async fn set(&self, id: &str, tunnel: Option<Tunnel>) {
        let mut map = self.tunnels.lock().await;
        match tunnel {
            Some(t) => {
                map.insert(id.to_string(), t);
            }
            None => {
                map.remove(id);
            }
        }
    }

    pub async fn close(&self, id: &str) {
        self.tunnels.lock().await.remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> keys::PublicKey {
        keys::parse_public_key_base64(s).unwrap()
    }

    const K1: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    const K2: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G1sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";

    #[test]
    fn host_key_unknown_then_learned_then_changed() {
        let dir = std::env::temp_dir().join(format!("datum-ssh-{}", uuid::Uuid::new_v4()));
        let file = dir.join("known_hosts");
        let files = vec![dir.join("missing"), file.clone()];
        assert_eq!(host_key_status("bastion", 22, &key(K1), &files), HostKeyStatus::Unknown);
        keys::known_hosts::learn_known_hosts_path("bastion", 22, &key(K1), &file).unwrap();
        assert_eq!(host_key_status("bastion", 22, &key(K1), &files), HostKeyStatus::Known);
        assert!(matches!(
            host_key_status("bastion", 22, &key(K2), &files),
            HostKeyStatus::Changed(_, _)
        ));
        // 不同端口视为不同主机
        assert_eq!(host_key_status("bastion", 2222, &key(K2), &files), HostKeyStatus::Unknown);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unparsable_record_for_the_host_is_not_trusted() {
        let dir = std::env::temp_dir().join(format!("datum-ssh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("known_hosts");
        std::fs::write(&file, "bastion ssh-ed25519 not-base64!!\n").unwrap();
        assert!(matches!(
            host_key_status("bastion", 22, &key(K1), &[file.clone()]),
            HostKeyStatus::Unreadable(_, _)
        ));
        // 别的主机的坏记录不影响
        assert_eq!(host_key_status("other", 22, &key(K1), &[file]), HostKeyStatus::Unknown);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn target_direct_is_not_tunneled() {
        let t = Target::direct("db.example.com", 5432);
        assert!(!t.tunneled());
        let t = Target {
            host: "127.0.0.1".into(),
            port: 50000,
            tls_host: "db.example.com".into(),
        };
        assert!(t.tunneled());
    }

    #[test]
    fn explicit_key_path_expands_home() {
        let cfg = SshTunnel {
            key_path: "~/.ssh/work_key".into(),
            auth: SshAuth::Key,
            ..Default::default()
        };
        let p = key_path(&cfg).unwrap();
        assert!(p.ends_with(".ssh/work_key"));
        assert!(!p.starts_with("~"));
    }
}
