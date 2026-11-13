//! 读 `~/.ssh/config`:按 OpenSSH 的规则给一个主机别名找出 HostName / Port / User / IdentityFile / ProxyJump。
//!
//! 支持的子集:`Host` 块(`*` `?` 通配、`!` 排除、多个模式)、`Include`(相对路径从 `~/.ssh` 起,支持通配)、
//! `Key Value` 与 `Key=Value` 两种写法、带引号的值。`Match` 块整段跳过(条件太多,不去猜)。
//! 同一项取第一次出现的值(OpenSSH 的规则),只有 IdentityFile 会累加。

use std::path::{Path, PathBuf};

/// 某个主机在 ssh config 里的设置(没写的为 None / 空)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostConfig {
    pub hostname: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub identity_files: Vec<String>,
    /// `none` 表示明确不用跳板
    pub proxy_jump: Option<String>,
}

/// Include 的嵌套层数上限(防止循环包含)。
const MAX_DEPTH: usize = 8;

/// `*`、`?` 通配匹配(不区分大小写,与 OpenSSH 一致)。
fn glob(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some(b'?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

/// `Host` 行的模式列表是否适用于 `host`:任一排除模式命中就不适用,否则任一普通模式命中即适用。
fn host_matches(patterns: &[String], host: &str) -> bool {
    let mut hit = false;
    for p in patterns {
        if let Some(neg) = p.strip_prefix('!') {
            if glob(neg, host) {
                return false;
            }
        } else if glob(p, host) {
            hit = true;
        }
    }
    hit
}

/// 一行拆成 (关键字小写, 参数列表);支持 `Key=Value` 与双引号。空行、注释为 None。
fn split_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, rest) = match line.find(|c: char| c.is_whitespace() || c == '=') {
        Some(i) => (&line[..i], line[i..].trim_start_matches(|c: char| c.is_whitespace() || c == '=')),
        None => (line, ""),
    };
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut has = false;
    for c in rest.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                has = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has {
                    args.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        args.push(cur);
    }
    Some((key.to_ascii_lowercase(), args))
}

/// `~/` 开头展开成主目录。
pub fn expand_home(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => PathBuf::from(path),
    }
}

/// Include 的路径(可带通配,只支持文件名部分的通配)→ 实际文件,按名字排序。
fn include_files(pattern: &str, ssh_dir: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let p = expand_home(pattern, home);
    let p = if p.is_absolute() { p } else { ssh_dir.join(p) };
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![p];
    }
    let Some(dir) = p.parent() else { return vec![] };
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| glob(&name, &e.file_name().to_string_lossy()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

struct Walker<'a> {
    host: &'a str,
    ssh_dir: &'a Path,
    home: Option<&'a Path>,
    out: HostConfig,
}

impl Walker<'_> {
    /// 处理一份配置的文本。`active`:当前是否在适用的块里(文件开头相当于 `Host *`)。
    fn walk(&mut self, text: &str, depth: usize) {
        let mut active = true;
        for line in text.lines() {
            let Some((key, args)) = split_line(line) else { continue };
            match key.as_str() {
                "host" => active = host_matches(&args, self.host),
                "match" => active = args.first().is_some_and(|a| a.eq_ignore_ascii_case("all")),
                "include" if active && depth < MAX_DEPTH => {
                    for pat in &args {
                        for f in include_files(pat, self.ssh_dir, self.home) {
                            if let Ok(t) = std::fs::read_to_string(&f) {
                                self.walk(&t, depth + 1);
                            }
                        }
                    }
                }
                _ if !active => {}
                "hostname" => {
                    if self.out.hostname.is_none() {
                        self.out.hostname = args.first().cloned();
                    }
                }
                "port" => {
                    if self.out.port.is_none() {
                        self.out.port = args.first().and_then(|p| p.parse().ok());
                    }
                }
                "user" => {
                    if self.out.user.is_none() {
                        self.out.user = args.first().cloned();
                    }
                }
                "identityfile" => {
                    if let Some(f) = args.first() {
                        self.out.identity_files.push(f.clone());
                    }
                }
                "proxyjump" => {
                    if self.out.proxy_jump.is_none() {
                        self.out.proxy_jump = Some(args.join(","));
                    }
                }
                _ => {}
            }
        }
    }
}

/// 解析一份 ssh config 的文本(`ssh_dir` 用于解析相对的 Include)。
pub fn lookup_in(text: &str, host: &str, ssh_dir: &Path, home: Option<&Path>) -> HostConfig {
    let mut w = Walker { host, ssh_dir, home, out: HostConfig::default() };
    w.walk(text, 0);
    let mut out = w.out;
    // %h:目标主机名;%d:主目录;%%:百分号。其余 token 不认识就原样留着(找不到文件时会报错)
    let hostname = out.hostname.clone().unwrap_or_else(|| host.to_string());
    let home_str = home.map(|h| h.display().to_string()).unwrap_or_default();
    let expand = |s: &str| s.replace("%h", &hostname).replace("%d", &home_str).replace("%%", "%");
    out.identity_files = out.identity_files.iter().map(|f| expand(f)).collect();
    if let Some(h) = &out.hostname {
        out.hostname = Some(h.replace("%h", host).replace("%%", "%"));
    }
    out
}

/// 读用户的 `~/.ssh/config`(环境变量 `DATUM_SSH_CONFIG` 可指定别的文件,测试用);没有这个文件就是全空。
pub fn lookup(host: &str) -> HostConfig {
    let Some(home) = dirs::home_dir() else { return HostConfig::default() };
    let ssh_dir = home.join(".ssh");
    let file = std::env::var_os("DATUM_SSH_CONFIG").map(PathBuf::from).unwrap_or_else(|| ssh_dir.join("config"));
    match std::fs::read_to_string(file) {
        Ok(text) => lookup_in(&text, host, &ssh_dir, Some(&home)),
        Err(_) => HostConfig::default(),
    }
}

/// 一跳跳板:`[user@]host[:port]`(IPv6 写成 `[::1]:22`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

/// 解析 ProxyJump 的值(逗号分隔,依次经过);`none` 或空为不用跳板。认不出的写法返回 None。
pub fn parse_proxy_jump(value: &str) -> Option<Vec<Hop>> {
    let v = value.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("none") {
        return Some(vec![]);
    }
    v.split(',')
        .map(|part| {
            let part = part.trim().strip_prefix("ssh://").unwrap_or(part.trim());
            let (user, rest) = match part.rsplit_once('@') {
                Some((u, r)) => (Some(u.to_string()), r),
                None => (None, part),
            };
            let (host, port) = if let Some(r) = rest.strip_prefix('[') {
                let (h, after) = r.split_once(']')?;
                let port = match after.strip_prefix(':') {
                    Some(p) => Some(p.parse::<u16>().ok()?),
                    None => None,
                };
                (h.to_string(), port)
            } else {
                match rest.rsplit_once(':') {
                    Some((h, p)) if !h.contains(':') => (h.to_string(), Some(p.parse().ok()?)),
                    _ => (rest.to_string(), None),
                }
            };
            (!host.is_empty()).then_some(Hop { user, host, port })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONF: &str = r#"
# 全局默认放在最后
Host bastion
    HostName 10.0.0.5
    Port 2200
    User ops
    IdentityFile ~/.ssh/ops_ed25519

Host db-*  !db-skip
    User=dba
    ProxyJump bastion
    IdentityFile "%d/.ssh/keys/%h.key"

Match host foo
    User wrong

Host *
    User fallback
    Port 22
    IdentityFile ~/.ssh/id_ed25519
"#;

    fn home() -> PathBuf {
        PathBuf::from("/home/me")
    }

    #[test]
    fn first_value_wins_and_identity_files_accumulate() {
        let h = home();
        let c = lookup_in(CONF, "bastion", &h.join(".ssh"), Some(&h));
        assert_eq!(c.hostname.as_deref(), Some("10.0.0.5"));
        assert_eq!(c.port, Some(2200));
        assert_eq!(c.user.as_deref(), Some("ops"));
        assert_eq!(c.identity_files, vec!["~/.ssh/ops_ed25519", "~/.ssh/id_ed25519"]);
        assert_eq!(c.proxy_jump, None);

        let c = lookup_in(CONF, "db-orders", &h.join(".ssh"), Some(&h));
        assert_eq!(c.user.as_deref(), Some("dba"), "Key=Value 写法");
        assert_eq!(c.port, Some(22));
        assert_eq!(c.proxy_jump.as_deref(), Some("bastion"));
        assert_eq!(c.identity_files[0], "/home/me/.ssh/keys/db-orders.key", "%d %h 展开、引号去掉");

        // 排除模式;Match 块跳过
        let c = lookup_in(CONF, "db-skip", &h.join(".ssh"), Some(&h));
        assert_eq!(c.user.as_deref(), Some("fallback"));
        assert_eq!(lookup_in(CONF, "foo", &h.join(".ssh"), Some(&h)).user.as_deref(), Some("fallback"));
    }

    #[test]
    fn include_relative_to_ssh_dir() {
        let dir = std::env::temp_dir().join(format!("datum-sshcfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("conf.d")).unwrap();
        std::fs::write(dir.join("conf.d/10-work"), "Host work\n  HostName work.example.com\n  User me\n").unwrap();
        std::fs::write(dir.join("conf.d/20-other"), "Host work\n  User someone-else\n").unwrap();
        let c = lookup_in("Include conf.d/*\nHost *\n  Port 2022\n", "work", &dir, None);
        assert_eq!(c.hostname.as_deref(), Some("work.example.com"));
        assert_eq!(c.user.as_deref(), Some("me"), "按文件名顺序,先出现的生效");
        assert_eq!(c.port, Some(2022));
    }

    #[test]
    fn proxy_jump_values() {
        assert_eq!(parse_proxy_jump("none"), Some(vec![]));
        assert_eq!(
            parse_proxy_jump("alice@jump1:2200, jump2,[::1]:2222"),
            Some(vec![
                Hop { user: Some("alice".into()), host: "jump1".into(), port: Some(2200) },
                Hop { user: None, host: "jump2".into(), port: None },
                Hop { user: None, host: "::1".into(), port: Some(2222) },
            ])
        );
        assert_eq!(parse_proxy_jump("ssh://bob@h:22"), Some(vec![Hop { user: Some("bob".into()), host: "h".into(), port: Some(22) }]));
        assert_eq!(parse_proxy_jump("h:notaport"), None);
        assert!(glob("db-*", "DB-1") && !glob("db-?", "db-12"));
    }
}
