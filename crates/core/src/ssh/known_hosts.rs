//! known_hosts 的解析与判定,按 OpenSSH 的语义(sshd(8) 的 SSH_KNOWN_HOSTS FILE FORMAT):
//!
//! - 行格式:`[标记] 主机模式 密钥类型 base64 [注释]`;`#` 开头为注释
//! - 标记:`@revoked`(该密钥已吊销,出示即拒绝);`@cert-authority`(CA 公钥,Datum 不校验主机证书,忽略)
//! - 主机模式:逗号分隔;支持 `*`、`?` 通配与 `!` 否定(命中否定则整行不算);
//!   `|1|盐|摘要` 为哈希过的主机名(HMAC-SHA1);非 22 端口写作 `[主机]:端口`;主机名不区分大小写
//!
//! 判定从严:这台主机只要在任何文件里有记录,出示的密钥就必须和其中一条一致,
//! 否则拒绝(不论密钥类型是否相同),只有完全没有记录时才首次信任。

use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use russh::keys::{self, PublicKey};
use sha1::Sha1;
use std::path::{Path, PathBuf};

/// 某主机在 known_hosts 里的一条记录。
#[derive(Debug, Clone)]
pub struct Record {
    pub file: PathBuf,
    pub line: usize,
    pub revoked: bool,
    /// 记录里写的密钥类型(`ssh-ed25519`、`ecdsa-sha2-nistp256`…)
    pub key_type: String,
    /// 解析不了的密钥(不支持的类型等)为 None:仍算作「有记录」
    pub key: Option<PublicKey>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    Known,
    /// 没有任何记录:可以首次信任
    Unknown,
    /// 出示的密钥已被 @revoked
    Revoked(PathBuf, usize),
    /// 记录了同类型的另一把密钥:(文件, 行号)
    Changed(PathBuf, usize),
    /// 只记录了别的类型的密钥:(记录里的类型, 出示的类型)
    OtherType(Vec<String>, String),
    /// 文件读不了:无法确认,按不可信处理
    Unreadable(PathBuf, String),
}

/// 主机在 known_hosts 里的写法:22 端口为 `主机`,其余为 `[主机]:端口`。
fn host_port(host: &str, port: u16) -> String {
    let host = host.to_ascii_lowercase();
    if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    }
}

/// `*` 匹配任意串,`?` 匹配单个字符(大小写已在调用前统一)。
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => glob(&pattern[1..], text) || (!text.is_empty() && glob(pattern, &text[1..])),
        (Some(b'?'), Some(_)) => glob(&pattern[1..], &text[1..]),
        (Some(p), Some(t)) if p == t => glob(&pattern[1..], &text[1..]),
        _ => false,
    }
}

fn hashed_matches(entry: &str, target: &str) -> bool {
    let mut parts = entry.trim_start_matches("|1|").split('|');
    let (Some(salt), Some(hash)) = (parts.next(), parts.next()) else {
        return false;
    };
    let b64 = base64::engine::general_purpose::STANDARD;
    let (Ok(salt), Ok(hash)) = (b64.decode(salt), b64.decode(hash)) else {
        return false;
    };
    let Ok(mut mac) = <Hmac<Sha1> as KeyInit>::new_from_slice(&salt) else {
        return false;
    };
    mac.update(target.as_bytes());
    mac.verify_slice(&hash).is_ok()
}

/// 主机模式列表是否命中:命中任一否定模式则整体不算,否则命中任一肯定模式即算。
fn patterns_match(patterns: &str, target: &str) -> bool {
    let mut positive = false;
    for p in patterns.split(',').filter(|p| !p.is_empty()) {
        if p.starts_with("|1|") {
            positive |= hashed_matches(p, target);
            continue;
        }
        let (negated, p) = match p.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, p),
        };
        if glob(p.to_ascii_lowercase().as_bytes(), target.as_bytes()) {
            if negated {
                return false;
            }
            positive = true;
        }
    }
    positive
}

/// 一个文件里关于这台主机的记录;文件不存在返回空。
fn records_in(file: &Path, target: &str) -> std::io::Result<Vec<Record>> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace().peekable();
        let revoked = match fields.peek() {
            Some(&"@revoked") => {
                fields.next();
                true
            }
            // @cert-authority 等其他标记:Datum 不校验主机证书,跳过
            Some(m) if m.starts_with('@') => continue,
            _ => false,
        };
        let (Some(hosts), Some(key_type), Some(b64)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        if !patterns_match(hosts, target) {
            continue;
        }
        out.push(Record {
            file: file.to_path_buf(),
            line: i + 1,
            revoked,
            key_type: key_type.to_string(),
            key: keys::parse_public_key_base64(b64).ok(),
        });
    }
    Ok(out)
}

/// 各文件里关于这台主机的全部记录。
pub fn records(host: &str, port: u16, files: &[PathBuf]) -> Result<Vec<Record>, (PathBuf, String)> {
    let target = host_port(host, port);
    let mut all = Vec::new();
    for file in files {
        all.extend(records_in(file, &target).map_err(|e| (file.clone(), e.to_string()))?);
    }
    Ok(all)
}

pub fn status(host: &str, port: u16, key: &PublicKey, files: &[PathBuf]) -> Status {
    let records = match records(host, port, files) {
        Ok(r) => r,
        Err((file, e)) => return Status::Unreadable(file, e),
    };
    let same = |r: &Record| r.key.as_ref().is_some_and(|k| k.key_data() == key.key_data());
    if let Some(r) = records.iter().find(|r| r.revoked && same(r)) {
        return Status::Revoked(r.file.clone(), r.line);
    }
    let live: Vec<&Record> = records.iter().filter(|r| !r.revoked).collect();
    if live.iter().any(|r| same(r)) {
        return Status::Known;
    }
    if live.is_empty() {
        return Status::Unknown;
    }
    let offered = key.algorithm().to_string();
    if let Some(r) = live.iter().find(|r| r.key_type == offered) {
        return Status::Changed(r.file.clone(), r.line);
    }
    let mut types: Vec<String> = live.iter().map(|r| r.key_type.clone()).collect();
    types.dedup();
    Status::OtherType(types, offered)
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    const K2: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G1sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";
    const ECDSA: &str = "AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBM9ozf7TmOl8hLnZIt786j4G0nFEVicr8Xj00guWViSdbFIXnMP1hckT6bwOUon4PaB/20xaezTYUBBX3EnFedw=";

    fn key(s: &str) -> PublicKey {
        keys::parse_public_key_base64(s).unwrap()
    }

    fn file(content: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("datum-kh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("known_hosts");
        std::fs::write(&f, content).unwrap();
        (dir, f)
    }

    fn check(content: &str, host: &str, port: u16, offered: &str) -> Status {
        let (dir, f) = file(content);
        let s = status(host, port, &key(offered), &[dir.join("missing"), f]);
        let _ = std::fs::remove_dir_all(dir);
        s
    }

    #[test]
    fn plain_bracketed_and_case_insensitive() {
        assert_eq!(check(&format!("bastion ssh-ed25519 {K1}\n"), "bastion", 22, K1), Status::Known);
        assert_eq!(check(&format!("BASTION ssh-ed25519 {K1}\n"), "Bastion", 22, K1), Status::Known);
        assert_eq!(check(&format!("[bastion]:2222 ssh-ed25519 {K1}\n"), "bastion", 2222, K1), Status::Known);
        // 非 22 端口:不带端口的记录不算
        assert_eq!(check(&format!("bastion ssh-ed25519 {K1}\n"), "bastion", 2222, K1), Status::Unknown);
        assert_eq!(check("# comment\n\n", "bastion", 22, K1), Status::Unknown);
    }

    #[test]
    fn same_type_different_key_is_changed() {
        assert!(matches!(
            check(&format!("bastion ssh-ed25519 {K1}\n"), "bastion", 22, K2),
            Status::Changed(_, 1)
        ));
    }

    #[test]
    fn other_type_on_record_is_not_trusted() {
        // 记录的是 ecdsa,出示 ed25519:不能当作没见过而首次信任
        assert_eq!(
            check(&format!("bastion ecdsa-sha2-nistp256 {ECDSA}\n"), "bastion", 22, K1),
            Status::OtherType(vec!["ecdsa-sha2-nistp256".into()], "ssh-ed25519".into())
        );
        // 解析不了的记录也算有记录
        assert!(matches!(
            check("bastion ssh-dss AAAAB3NzaC1kc3MAAACBAP\n", "bastion", 22, K1),
            Status::OtherType(_, _)
        ));
    }

    #[test]
    fn revoked_key_is_rejected_even_without_other_records() {
        assert!(matches!(
            check(&format!("@revoked bastion ssh-ed25519 {K1}\n"), "bastion", 22, K1),
            Status::Revoked(_, 1)
        ));
        assert!(matches!(
            check(&format!("@revoked * ssh-ed25519 {K1}\nbastion ssh-ed25519 {K1}\n"), "bastion", 22, K1),
            Status::Revoked(_, 1)
        ));
        // 吊销的是别的密钥:不影响
        assert_eq!(check(&format!("@revoked * ssh-ed25519 {K2}\n"), "bastion", 22, K1), Status::Unknown);
    }

    #[test]
    fn cert_authority_lines_are_ignored() {
        assert_eq!(
            check(&format!("@cert-authority *.example.com ssh-ed25519 {K2}\n"), "db.example.com", 22, K1),
            Status::Unknown
        );
    }

    #[test]
    fn wildcards_and_negation() {
        let content = format!("*.example.com,!secret.example.com ssh-ed25519 {K1}\n");
        assert_eq!(check(&content, "bastion.example.com", 22, K1), Status::Known);
        assert!(matches!(check(&content, "bastion.example.com", 22, K2), Status::Changed(_, _)));
        assert_eq!(check(&content, "secret.example.com", 22, K2), Status::Unknown);
        assert_eq!(check(&format!("10.0.0.? ssh-ed25519 {K1}\n"), "10.0.0.7", 22, K1), Status::Known);
        assert_eq!(check(&format!("10.0.0.? ssh-ed25519 {K1}\n"), "10.0.0.17", 22, K1), Status::Unknown);
    }

    #[test]
    fn hashed_hostnames() {
        // ssh-keygen -H 的格式:|1|base64(盐)|base64(HMAC-SHA1(盐, 主机))
        let salt = b"0123456789abcdefghij";
        let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(salt).unwrap();
        mac.update(b"[bastion]:2222");
        let b64 = base64::engine::general_purpose::STANDARD;
        let entry = format!("|1|{}|{}", b64.encode(salt), b64.encode(mac.finalize().into_bytes()));
        let content = format!("{entry} ssh-ed25519 {K1}\n");
        assert_eq!(check(&content, "bastion", 2222, K1), Status::Known);
        assert!(matches!(check(&content, "bastion", 2222, K2), Status::Changed(_, _)));
        assert_eq!(check(&content, "bastion", 22, K2), Status::Unknown);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_is_not_trusted() {
        let dir = std::env::temp_dir().join(format!("datum-kh-{}", uuid::Uuid::new_v4()));
        // 目录当文件读:读失败但不是「不存在」
        std::fs::create_dir_all(dir.join("known_hosts")).unwrap();
        assert!(matches!(
            status("bastion", 22, &key(K1), &[dir.join("known_hosts")]),
            Status::Unreadable(_, _)
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}
