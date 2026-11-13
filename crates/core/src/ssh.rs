//! SSH 隧道:经跳板机把数据库端口转发到本机 `127.0.0.1` 的随机端口,驱动改连本地端口。
//!
//! - 一个连接一条 SSH 会话;本地端口每来一个 TCP 连接就开一条 direct-tcpip 通道
//!   (MySQL 连接池会陆续开多条)。会话断了,下一条连接进来时自动重连。
//! - 主机指纹:查 `~/.ssh/known_hosts` 与 Datum 自己的 known_hosts(配置目录下),规则见
//!   [`known_hosts`]:有记录就必须对上(不论密钥类型)、吊销的拒绝;两处都没有记录才首次信任,
//!   记到 Datum 自己的文件里,不改动用户的 `~/.ssh`。
//! - 证书校验仍按数据库的原主机名进行([`Target::tls_host`]),驱动只是改连本地端口。
//! - 跳板主机可以填 `~/.ssh/config` 里的别名:HostName / Port / User / IdentityFile / ProxyJump 照用
//!   (表单里填了的优先,端口填默认的 22 时听 config 的)。
//! - 多级跳板(ProxyJump):依次连上每一跳,下一跳经上一跳的 direct-tcpip 通道握手;中间的跳板用
//!   ssh-agent 或私钥(config 里的 IdentityFile、`~/.ssh` 下的默认私钥)认证,不发送密码。

pub mod config;
mod known_hosts;

use self::known_hosts::Status;
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

/// 驱动该怎么连数据库:直连时就是数据库本身;走隧道时见 [`Via`],
/// `tls_host` 仍是数据库的原主机名(证书校验、SNI 用)。
#[derive(Clone)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub tls_host: String,
    /// 经 SSH 隧道时的走法;直连为 None
    pub via: Option<Via>,
}

/// 经隧道连数据库的方式。
#[derive(Clone)]
pub enum Via {
    /// 本机 127.0.0.1 上的转发端口(`host` / `port`)
    Tcp,
    /// 只有本用户能连的 Unix 套接字(MySQL 不加密时)
    Socket(PathBuf),
    /// 不开任何本地端口:每条连接直接用一条 SSH 通道(PG、Redis)
    Stream(Arc<Shared>),
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let via = match &self.via {
            None => "direct".to_string(),
            Some(Via::Tcp) => "tcp".into(),
            Some(Via::Socket(p)) => format!("socket {}", p.display()),
            Some(Via::Stream(_)) => "ssh channel".into(),
        };
        write!(f, "Target({}:{} tls={} via {via})", self.host, self.port, self.tls_host)
    }
}

/// 经 SSH 通道的连接流(实现 AsyncRead / AsyncWrite)。
pub type SshStream = russh::ChannelStream<client::Msg>;

impl Target {
    pub fn direct(host: &str, port: u16) -> Self {
        Self { host: host.to_string(), port, tls_host: host.to_string(), via: None }
    }

    /// 经本地转发端口(127.0.0.1:`port`)。
    pub fn forwarded(port: u16, tls_host: &str) -> Self {
        Self { host: "127.0.0.1".into(), port, tls_host: tls_host.to_string(), via: Some(Via::Tcp) }
    }

    /// 是否经由 SSH 隧道(连接地址与 TLS 主机名分开)。
    pub fn tunneled(&self) -> bool {
        self.via.is_some()
    }

    /// 经 SSH 通道直连时,开一条新的连接流;其余方式为 None。
    pub async fn open_stream(&self) -> Option<AppResult<SshStream>> {
        match &self.via {
            Some(Via::Stream(shared)) => Some(shared.route.open_channel(&shared.session, 0).await.map(|c| c.into_stream())),
            _ => None,
        }
    }
}

/// 建隧道需要的上下文:SSH 密钥与 Datum 自己的 known_hosts 文件。
#[derive(Debug, Clone, Default)]
pub struct SshContext {
    pub secret: String,
    pub known_hosts: PathBuf,
}

/// 按库类型选经隧道的走法:PG / Redis 的驱动能直接用 SSH 通道,不开本地端口;MySQL 驱动只认
/// TCP 或 Unix 套接字——明确不加密时(Unix)用只有本用户能连的套接字;「优先」「必须」「校验证书」
/// 都要尝试端到端 TLS(SSH 只加密到跳板机这一段,跳板机到数据库还要靠 TLS),而 mysql_async 在
/// 套接字上不做 TLS,所以只能用本地端口。
pub fn listen_for(conn: &Connection) -> Listen {
    use crate::core::connection::{DbKind, SslMode};
    match conn.kind {
        DbKind::Pg | DbKind::Redis => Listen::Stream,
        #[cfg(unix)]
        DbKind::Mysql if conn.ssl_mode == SslMode::Disable => Listen::Socket,
        _ => Listen::Tcp,
    }
}

/// 按连接配置得到驱动要连的地址;走 SSH 时先建好隧道,隧道随返回值存活。
pub async fn resolve(conn: &Connection, ctx: &SshContext) -> AppResult<(Target, Option<Tunnel>)> {
    match conn.tunnel() {
        None => Ok((Target::direct(&conn.host, conn.port), None)),
        Some(ssh) => {
            let tunnel = Tunnel::open(ssh, &ctx.secret, &conn.host, conn.port, &ctx.known_hosts, listen_for(conn)).await?;
            let target = tunnel.target(&conn.host);
            Ok((target, Some(tunnel)))
        }
    }
}

fn ssh_err(key: &str) -> AppError {
    AppError::new(ErrorKind::Connection, t!(key))
}

// ---------- 主机指纹 ----------

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
        let reject = |e: AppError| -> Result<bool, Self::Error> {
            *self.rejected.lock().unwrap() = Some(e);
            Ok(false)
        };
        let at = |file: &Path, line: usize| format!("{}:{} ({}:{})", self.host, self.port, file.display(), line);
        match known_hosts::status(&self.host, self.port, key, &files) {
            Status::Known => Ok(true),
            Status::Unknown => {
                // 首次连接:信任并记下,之后指纹变了就会被拦住。记不下来就不连:
                // 否则每次都像首次连接,指纹被换了也发现不了
                match keys::known_hosts::learn_known_hosts_path(&self.host, self.port, key, &self.app_known_hosts) {
                    Ok(()) => Ok(true),
                    Err(e) => reject(
                        ssh_err("core.err.ssh_known_hosts_unreadable")
                            .with_detail(format!("{}: {e}", self.app_known_hosts.display())),
                    ),
                }
            }
            Status::Unreadable(file, e) => {
                reject(ssh_err("core.err.ssh_known_hosts_unreadable").with_detail(format!("{}: {e}", file.display())))
            }
            Status::Changed(file, line) => reject(ssh_err("core.err.ssh_host_key_changed").with_detail(at(&file, line))),
            Status::Revoked(file, line) => reject(ssh_err("core.err.ssh_host_key_revoked").with_detail(at(&file, line))),
            Status::OtherType(recorded, offered) => reject(
                AppError::new(
                    ErrorKind::Connection,
                    t!("core.err.ssh_host_key_other_type", recorded = recorded.join(", "), offered = offered),
                )
                .with_detail(format!("{}:{}", self.host, self.port)),
            ),
        }
    }
}

/// 握手时优先协商 known_hosts 里已记录的主机密钥类型(OpenSSH 同样如此),
/// 免得服务器先出示另一种类型、被当成指纹不符拒绝。RSA 暂不支持(见 ssh_rsa_unsupported),不前置。
fn preferred_key_algorithms(host: &str, port: u16, app_known_hosts: &Path) -> Vec<keys::Algorithm> {
    let defaults: Vec<keys::Algorithm> = russh::Preferred::DEFAULT.key.to_vec();
    let recorded: Vec<keys::Algorithm> = known_hosts::records(host, port, &known_hosts_files(app_known_hosts))
        .unwrap_or_default()
        .into_iter()
        .filter(|r| !r.revoked)
        .filter_map(|r| keys::Algorithm::new(&r.key_type).ok())
        .filter(|a| !a.clone().is_rsa())
        .collect();
    let mut order: Vec<keys::Algorithm> = Vec::new();
    for a in recorded.into_iter().chain(defaults) {
        if !order.contains(&a) {
            order.push(a);
        }
    }
    order
}

// ---------- 会话 ----------

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

/// `~/.ssh` 下的默认私钥(存在的)。
fn default_keys() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return vec![] };
    ["id_ed25519", "id_ecdsa", "id_rsa"].iter().map(|n| home.join(".ssh").join(n)).filter(|p| p.exists()).collect()
}

/// 私钥路径:填了就用填的;没填就用 ssh config 里的 IdentityFile,再找 ~/.ssh 下的默认私钥。
fn key_path(cfg: &SshTunnel, ep: &Endpoint) -> AppResult<PathBuf> {
    let given = cfg.key_path.trim();
    if !given.is_empty() {
        return Ok(expand_home(given));
    }
    ep.identity
        .iter()
        .find(|p| p.exists())
        .cloned()
        .or_else(|| default_keys().into_iter().next())
        .ok_or_else(|| ssh_err("core.err.ssh_key_not_found"))
}

fn load_key_file(path: &Path, passphrase: &str) -> AppResult<keys::PrivateKey> {
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

// ---------- 主机解析(ssh config、ProxyJump) ----------

/// 要连的一台 SSH 主机(跳板或最终的跳板机),已按 ssh config 解析好。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// config 里的 IdentityFile(已展开 `~`)
    pub identity: Vec<PathBuf>,
}

fn local_user() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default()
}

/// 按 ssh config 解析一台主机;`port` / `user` 是明确指定的(优先)。
fn endpoint(alias: &str, port: Option<u16>, user: Option<&str>, c: &config::HostConfig) -> Endpoint {
    let home = dirs::home_dir();
    Endpoint {
        host: c.hostname.clone().unwrap_or_else(|| alias.to_string()),
        port: port.or(c.port).unwrap_or(22),
        user: user
            .filter(|u| !u.trim().is_empty())
            .map(str::to_string)
            .or_else(|| c.user.clone())
            .unwrap_or_else(local_user),
        identity: c.identity_files.iter().map(|f| config::expand_home(f, home.as_deref())).collect(),
    }
}

/// 隧道要经过的主机:先依次经过 `jumps`,最后到 `bastion`(在它上面开 direct-tcpip 到数据库)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub jumps: Vec<Endpoint>,
    pub bastion: Endpoint,
}

/// 由表单配置与 ssh config 得出路线。`lookup` 查某个主机的 ssh config(测试时可替换)。
pub fn plan_with(cfg: &SshTunnel, lookup: impl Fn(&str) -> config::HostConfig) -> AppResult<Plan> {
    let alias = cfg.host.trim();
    let c = lookup(alias);
    // 表单里端口是默认的 22(或没填)时,听 ssh config 的
    let port = (cfg.port != 0 && cfg.port != 22).then_some(cfg.port).or(c.port).or(Some(cfg.port).filter(|p| *p != 0));
    let bastion = endpoint(alias, port, Some(&cfg.user), &c);
    let jump_spec = if cfg.proxy_jump.trim().is_empty() { c.proxy_jump.clone().unwrap_or_default() } else { cfg.proxy_jump.clone() };
    let hops = config::parse_proxy_jump(&jump_spec)
        .ok_or_else(|| ssh_err("core.err.ssh_proxy_jump_invalid").with_detail(jump_spec.clone()))?;
    let jumps = hops
        .iter()
        .map(|h| {
            let hc = lookup(&h.host);
            endpoint(&h.host, h.port, h.user.as_deref(), &hc)
        })
        .collect();
    Ok(Plan { jumps, bastion })
}

pub fn plan(cfg: &SshTunnel) -> AppResult<Plan> {
    plan_with(cfg, config::lookup)
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

async fn authenticate(handle: &mut Session, cfg: &SshTunnel, ep: &Endpoint, secret: &str) -> AppResult<()> {
    let user = ep.user.as_str();
    let failed = |e: russh::Error| ssh_err("core.err.ssh_auth_failed").with_detail(e.to_string());
    let ok = match cfg.auth {
        SshAuth::Password => handle.authenticate_password(user, secret).await.map_err(failed)?.success(),
        SshAuth::Key => {
            let key = load_key_file(&key_path(cfg, ep)?, secret)?;
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
        Err(ssh_err("core.err.ssh_auth_failed").with_detail(format!("{user}@{}:{}", ep.host, ep.port)))
    }
}

/// 中间的跳板:不发密码。先试 ssh config 里为它写的 IdentityFile,再试 ssh-agent,最后试 `~/.ssh` 下的默认私钥
/// (agent 里钥匙多时先试一圈容易用光服务器的认证次数)。私钥先不带密码短语试,主认证是私钥时再用同一个密码短语试。
async fn authenticate_jump(handle: &mut Session, ep: &Endpoint, passphrase: Option<&str>) -> AppResult<()> {
    let user = ep.user.as_str();
    let hash = rsa_hash(handle).await;
    async fn try_files(handle: &mut Session, user: &str, files: &[PathBuf], passphrase: Option<&str>, hash: Option<HashAlg>) -> bool {
        for path in files {
            for pass in [None, passphrase.filter(|p| !p.is_empty())] {
                let Ok(key) = keys::load_secret_key(path, pass) else { continue };
                if key.algorithm().is_rsa() {
                    break;
                }
                if handle
                    .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                    .await
                    .is_ok_and(|r| r.success())
                {
                    return true;
                }
                break;
            }
        }
        false
    }
    let configured: Vec<PathBuf> = ep.identity.iter().filter(|p| p.exists()).cloned().collect();
    if try_files(handle, user, &configured, passphrase, hash).await {
        return Ok(());
    }
    if agent_auth(handle, user).await.unwrap_or(false) {
        return Ok(());
    }
    let defaults: Vec<PathBuf> = default_keys().into_iter().filter(|p| !configured.contains(p)).collect();
    if try_files(handle, user, &defaults, passphrase, hash).await {
        return Ok(());
    }
    Err(ssh_err("core.err.ssh_jump_auth_failed").with_detail(format!("{user}@{}:{}", ep.host, ep.port)))
}

fn session_config(ep: &Endpoint, app_known_hosts: &Path) -> Arc<client::Config> {
    Arc::new(client::Config {
        preferred: russh::Preferred {
            key: preferred_key_algorithms(&ep.host, ep.port, app_known_hosts).into(),
            ..russh::Preferred::DEFAULT
        },
        keepalive_interval: Some(KEEPALIVE),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    })
}

/// 一条 SSH 会话链:依次经过的跳板会话 + 最后到跳板机的会话。前面的会话要一直活着,
/// 后一跳的连接就是走在前一跳的通道上的。
pub(crate) struct Chain {
    jumps: Vec<Session>,
    session: Session,
}

impl Chain {
    fn is_closed(&self) -> bool {
        self.session.is_closed() || self.jumps.iter().any(|s| s.is_closed())
    }
}

/// 握手一跳:第一跳直接 TCP,之后经上一跳的 direct-tcpip 通道。
async fn handshake(ep: &Endpoint, via: Option<&Session>, app_known_hosts: &Path) -> AppResult<Session> {
    let rejected = Arc::new(std::sync::Mutex::new(None));
    let handler = HostKeyCheck {
        host: ep.host.clone(),
        port: ep.port,
        app_known_hosts: app_known_hosts.to_path_buf(),
        rejected: rejected.clone(),
    };
    let config = session_config(ep, app_known_hosts);
    let fail = |e: russh::Error| {
        rejected
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| ssh_err("core.err.ssh_connect_failed").with_detail(format!("{}:{} ({e})", ep.host, ep.port)))
    };
    match via {
        None => client::connect(config, (ep.host.clone(), ep.port), handler).await.map_err(fail),
        Some(prev) => {
            let ch = prev
                .channel_open_direct_tcpip(ep.host.clone(), ep.port as u32, "127.0.0.1", 0)
                .await
                .map_err(|e| ssh_err("core.err.ssh_connect_failed").with_detail(format!("{}:{} ({e})", ep.host, ep.port)))?;
            client::connect_stream(config, ch.into_stream(), handler).await.map_err(fail)
        }
    }
}

/// 按路线连上跳板机并完成认证(经过 ProxyJump 时逐跳握手)。
async fn open_session(cfg: &SshTunnel, secret: &str, app_known_hosts: &Path) -> AppResult<Chain> {
    let plan = plan(cfg)?;
    let jump_pass = (cfg.auth == SshAuth::Key).then_some(secret);
    let connect = async {
        let mut jumps: Vec<Session> = Vec::new();
        for ep in &plan.jumps {
            let mut s = handshake(ep, jumps.last(), app_known_hosts).await?;
            authenticate_jump(&mut s, ep, jump_pass).await?;
            jumps.push(s);
        }
        let mut session = handshake(&plan.bastion, jumps.last(), app_known_hosts).await?;
        authenticate(&mut session, cfg, &plan.bastion, secret).await?;
        Ok(Chain { jumps, session })
    };
    // 每多一跳多给一段时间
    let limit = CONNECT_TIMEOUT * (1 + plan.jumps.len() as u32);
    match tokio::time::timeout(limit, connect).await {
        Ok(r) => r,
        Err(_) => Err(ssh_err("core.err.ssh_timeout").with_detail(format!("{}:{}", plan.bastion.host, plan.bastion.port))),
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
    async fn open_channel(&self, session: &Mutex<Chain>, from_port: u16) -> AppResult<russh::Channel<client::Msg>> {
        let mut guard = session.lock().await;
        if guard.is_closed() {
            *guard = open_session(&self.ssh, &self.secret, &self.known_hosts).await?;
        }
        match self.direct_tcpip(&guard.session, from_port).await {
            Ok(ch) => Ok(ch),
            // 会话在检查之后才断开:重连一次再试
            Err(_) if guard.is_closed() => {
                *guard = open_session(&self.ssh, &self.secret, &self.known_hosts).await?;
                self.direct_tcpip(&guard.session, from_port).await.map_err(|e| self.unreachable(e))
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

/// 隧道在本机怎么接驱动的连接。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listen {
    /// 不开本地端口,驱动直接用 SSH 通道
    Stream,
    /// 127.0.0.1 上的随机端口(驱动只认 TCP 时)
    Tcp,
    /// 私有目录(0700)里的 Unix 套接字,只接受本用户的连接
    #[cfg(unix)]
    Socket,
}

/// 隧道的路线与会话链,供开新通道用。
pub struct Shared {
    route: Route,
    session: Mutex<Chain>,
}

/// 一条在用的隧道:丢弃即关闭(本地端口 / 套接字、SSH 会话与其上所有转发一并结束)。
///
/// 暴露面:PG 与 Redis 不开任何本地端口;MySQL 不加密时用只有本用户能连的 Unix 套接字;
/// MySQL 要(或优先)TLS 时只能开 127.0.0.1 端口(与 `ssh -L` 相同),Linux 上只接受本用户进程的连接,
/// macOS / Windows 上同一台机器的其他用户在隧道存活期间也能连到这个端口(数据库自身的认证仍然生效)。
pub struct Tunnel {
    /// 本地转发端口(Listen::Tcp);其余为 0
    pub local_port: u16,
    /// Unix 套接字路径(Listen::Socket)
    pub socket: Option<PathBuf>,
    shared: Arc<Shared>,
    task: Option<JoinHandle<()>>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        if let Some(t) = &self.task {
            t.abort();
        }
        if let Some(path) = &self.socket {
            let _ = std::fs::remove_file(path);
            if let Some(dir) = path.parent() {
                let _ = std::fs::remove_dir(dir);
            }
        }
    }
}

/// Linux:127.0.0.1 上连进来的这个 TCP 连接属于哪个用户(查 /proc/net/tcp);查不到为 None。
#[cfg(target_os = "linux")]
fn tcp_peer_uid(peer: std::net::SocketAddr, local_port: u16) -> Option<u32> {
    let hex_port = |p: u16| format!("{p:04X}");
    for file in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(file) else { continue };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            // local_address rem_address … uid(第 8 列)
            let (Some(local), Some(remote), Some(uid)) = (f.get(1), f.get(2), f.get(7)) else { continue };
            let port_of = |a: &str| a.rsplit(':').next().map(str::to_string);
            if port_of(local) == Some(hex_port(peer.port())) && port_of(remote) == Some(hex_port(local_port)) {
                return uid.parse().ok();
            }
        }
    }
    None
}

/// 这个连接是不是本用户的(只在 Linux 上查得到;其余平台放行)。
fn tcp_peer_allowed(peer: std::net::SocketAddr, local_port: u16) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let me = std::fs::metadata("/proc/self").map(|m| m.uid()).ok();
        match (me, tcp_peer_uid(peer, local_port)) {
            (Some(me), Some(uid)) => me == uid,
            // /proc 读不了:没法判断,放行(与没有这项检查时一样)
            _ => true,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (peer, local_port);
        true
    }
}

impl Tunnel {
    /// 连上跳板机,确认它能连到目标,再按 `listen` 在本机接驱动的连接。
    pub async fn open(
        ssh: &SshTunnel,
        secret: &str,
        target_host: &str,
        target_port: u16,
        known_hosts: &Path,
        listen: Listen,
    ) -> AppResult<Self> {
        let route = Route {
            ssh: ssh.clone(),
            secret: secret.to_string(),
            known_hosts: known_hosts.to_path_buf(),
            target_host: target_host.to_string(),
            target_port,
        };
        let session = Mutex::new(open_session(ssh, secret, known_hosts).await?);
        // 先试开一条通道:目标不可达时在这里给出明确报错,而不是让驱动报一个「连接被重置」
        drop(route.open_channel(&session, 0).await?);
        let shared = Arc::new(Shared { route, session });
        let failed = |e: std::io::Error| ssh_err("core.err.ssh_forward_failed").with_detail(e.to_string());
        let mut tunnel = Self { local_port: 0, socket: None, shared: shared.clone(), task: None };
        match listen {
            Listen::Stream => {}
            Listen::Tcp => {
                let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(failed)?;
                let port = listener.local_addr().map_err(failed)?.port();
                tunnel.local_port = port;
                tunnel.task = Some(tokio::spawn(async move {
                    // 转发任务都挂在这里:隧道关闭(本任务被 abort)时一起结束
                    let mut forwards = JoinSet::new();
                    loop {
                        tokio::select! {
                            accepted = listener.accept() => {
                                let Ok((mut socket, peer)) = accepted else { continue };
                                if !tcp_peer_allowed(peer, port) {
                                    continue; // 别的用户的进程:直接断开
                                }
                                let shared = shared.clone();
                                forwards.spawn(async move {
                                    let Ok(channel) = shared.route.open_channel(&shared.session, peer.port()).await else { return };
                                    let mut stream = channel.into_stream();
                                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                                });
                            }
                            Some(_) = forwards.join_next(), if !forwards.is_empty() => {}
                        }
                    }
                }));
            }
            #[cfg(unix)]
            Listen::Socket => {
                use std::os::unix::fs::{DirBuilderExt, MetadataExt};
                // 目录 0700:别的用户连目录都进不去;套接字路径要短(macOS 上限 104 字节)
                let dir = std::env::temp_dir().join(format!("datum-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]));
                std::fs::DirBuilder::new().mode(0o700).create(&dir).map_err(failed)?;
                let me = std::fs::metadata(&dir).map_err(failed)?.uid();
                let path = dir.join("db.sock");
                let listener = tokio::net::UnixListener::bind(&path).map_err(failed)?;
                tunnel.socket = Some(path);
                tunnel.task = Some(tokio::spawn(async move {
                    let mut forwards = JoinSet::new();
                    loop {
                        tokio::select! {
                            accepted = listener.accept() => {
                                let Ok((mut socket, _)) = accepted else { continue };
                                // 再核对一次对端用户(目录权限之外的第二道)
                                if socket.peer_cred().map(|c| c.uid()).ok() != Some(me) {
                                    continue;
                                }
                                let shared = shared.clone();
                                forwards.spawn(async move {
                                    let Ok(channel) = shared.route.open_channel(&shared.session, 0).await else { return };
                                    let mut stream = channel.into_stream();
                                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                                });
                            }
                            Some(_) = forwards.join_next(), if !forwards.is_empty() => {}
                        }
                    }
                }));
            }
        }
        Ok(tunnel)
    }

    /// 驱动该怎么连(`tls_host` 为数据库原主机名)。
    pub fn target(&self, tls_host: &str) -> Target {
        let via = match (&self.socket, self.local_port) {
            (Some(p), _) => Via::Socket(p.clone()),
            (None, 0) => Via::Stream(self.shared.clone()),
            (None, _) => Via::Tcp,
        };
        Target { host: "127.0.0.1".into(), port: self.local_port, tls_host: tls_host.to_string(), via: Some(via) }
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
    fn recorded_host_key_type_is_negotiated_first() {
        let dir = std::env::temp_dir().join(format!("datum-ssh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("known_hosts");
        let host = "datum-test-bastion.invalid";
        let ecdsa = keys::Algorithm::Ecdsa { curve: keys::EcdsaCurve::NistP256 };
        // 没有记录:默认顺序
        assert_eq!(preferred_key_algorithms(host, 2222, &file), russh::Preferred::DEFAULT.key.to_vec());
        // 记录了 ecdsa:ecdsa 排第一,其余不重复
        std::fs::write(&file, format!("[{host}]:2222 ecdsa-sha2-nistp256 AAAA\n")).unwrap();
        let order = preferred_key_algorithms(host, 2222, &file);
        assert_eq!(order[0], ecdsa);
        assert_eq!(order.len(), russh::Preferred::DEFAULT.key.len());
        // 只记录了 RSA:不前置(暂不支持),保持默认
        std::fs::write(&file, format!("[{host}]:2222 ssh-rsa AAAA\n")).unwrap();
        assert_eq!(preferred_key_algorithms(host, 2222, &file), russh::Preferred::DEFAULT.key.to_vec());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn target_direct_is_not_tunneled() {
        let t = Target::direct("db.example.com", 5432);
        assert!(!t.tunneled());
        let t = Target::forwarded(50000, "db.example.com");
        assert!(t.tunneled());
    }

    #[test]
    fn plan_follows_ssh_config_and_proxy_jump() {
        let lookup = |h: &str| match h {
            "prod-db" => config::HostConfig {
                hostname: Some("10.1.2.3".into()),
                port: Some(2200),
                user: Some("dba".into()),
                identity_files: vec!["/k/prod".into()],
                proxy_jump: Some("gw,ops@edge:2222".into()),
            },
            "gw" => config::HostConfig { hostname: Some("gw.example.com".into()), user: Some("me".into()), ..Default::default() },
            _ => config::HostConfig::default(),
        };
        let cfg = SshTunnel { host: "prod-db".into(), port: 22, user: String::new(), ..Default::default() };
        let p = plan_with(&cfg, lookup).unwrap();
        assert_eq!((p.bastion.host.as_str(), p.bastion.port, p.bastion.user.as_str()), ("10.1.2.3", 2200, "dba"));
        assert_eq!(p.bastion.identity, vec![PathBuf::from("/k/prod")]);
        let hops: Vec<(&str, u16, &str)> = p.jumps.iter().map(|e| (e.host.as_str(), e.port, e.user.as_str())).collect();
        assert_eq!(hops, vec![("gw.example.com", 22, "me"), ("edge", 2222, "ops")]);

        // 表单里填的优先:非默认端口、用户、跳板;跳板写 none 则不经过
        let cfg = SshTunnel { host: "prod-db".into(), port: 2201, user: "root".into(), proxy_jump: "none".into(), ..Default::default() };
        let p = plan_with(&cfg, lookup).unwrap();
        assert_eq!((p.bastion.port, p.bastion.user.as_str()), (2201, "root"));
        assert!(p.jumps.is_empty());
        // 不在 config 里的主机原样使用
        let cfg = SshTunnel { host: "1.2.3.4".into(), port: 22, user: "u".into(), ..Default::default() };
        let p = plan_with(&cfg, lookup).unwrap();
        assert_eq!((p.bastion.host.as_str(), p.bastion.port), ("1.2.3.4", 22));
        let bad = SshTunnel { host: "x".into(), proxy_jump: "h:port".into(), ..Default::default() };
        assert!(plan_with(&bad, lookup).is_err());
    }

    #[test]
    fn explicit_key_path_expands_home() {
        let cfg = SshTunnel {
            key_path: "~/.ssh/work_key".into(),
            auth: SshAuth::Key,
            ..Default::default()
        };
        let p = key_path(&cfg, &endpoint("h", None, None, &config::HostConfig::default())).unwrap();
        assert!(p.ends_with(".ssh/work_key"));
        assert!(!p.starts_with("~"));
    }
}
