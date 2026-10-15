//! 连接串处理(纯函数):拆出密码与库名、按需拼回密码、表单字段拼连接串。
//!
//! 配置文件里只存不含密码、不含库名的连接串;密码走现有的 `credentials`(Local 明文、其余进钥匙串),
//! 库名放 `Connection.database`,这样侧栏切库只改 `database`,不用改连接串。

use crate::core::connection::{Connection, SslMode};

pub struct UriParts {
    /// 不含密码与库名的连接串
    pub sanitized: String,
    pub user: String,
    pub password: Option<String>,
    /// 主机列表原文(`h1:27017,h2` 或 SRV 域名),用于侧栏展示与切库匹配
    pub hosts: String,
    pub database: String,
}

/// RFC 3986 unreserved 之外的字节一律百分号编码。
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> Result<String, String> {
    let bad = || "bad percent-encoding".to_string();
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3).ok_or_else(bad)?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| bad())?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| bad())
}

/// 拆成 (scheme://, userinfo, hosts, path, query)。
fn pieces(input: &str) -> Result<(&str, Option<&str>, &str, &str, &str), String> {
    let scheme = ["mongodb+srv://", "mongodb://"]
        .into_iter()
        .find(|s| input.starts_with(s))
        .ok_or("connection string must start with mongodb:// or mongodb+srv://")?;
    let rest = &input[scheme.len()..];
    let (before_q, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, path) = before_q.split_once('/').unwrap_or((before_q, ""));
    let (userinfo, hosts) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    if hosts.is_empty() {
        return Err("connection string has no host".into());
    }
    Ok((scheme, userinfo, hosts, path, query))
}

/// 连接串里只有密码会被拆出去单独保存;其他机密参数(证书 / 代理密码、AWS 会话令牌)
/// 会随连接串明文写进配置文件,所以直接拒绝。报错只带参数名,不带值。
fn reject_secret_params(input: &str) -> Result<(), String> {
    let query = input.split_once('?').map_or("", |(_, q)| q);
    for param in query.split('&') {
        let (key, value) = param.split_once('=').unwrap_or((param, ""));
        let secret = key.to_ascii_lowercase().contains("password")
            || (key.eq_ignore_ascii_case("authMechanismProperties")
                && decode(value)
                    .unwrap_or_default()
                    .to_ascii_uppercase()
                    .contains("AWS_SESSION_TOKEN"));
        if secret {
            return Err(rust_i18n::t!("core.mongo.err.secret_param", key = key).to_string());
        }
    }
    Ok(())
}

pub fn split_uri(input: &str) -> Result<UriParts, String> {
    let input = input.trim();
    reject_secret_params(input)?;
    mongodb::options::ConnectionString::parse(input).map_err(|e| e.to_string())?;
    let (scheme, userinfo, hosts, path, query) = pieces(input)?;
    let (user, password) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((name, pw)) => (decode(name)?, Some(decode(pw)?)),
            None => (decode(u)?, None),
        },
        None => (String::new(), None),
    };
    let database = decode(path)?;
    let mut params: Vec<String> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    // 路径里的库名原本兼作认证库;去掉库名后显式写上,避免认证库悄悄变成 admin
    let has_auth_source = params.iter().any(|p| {
        p.split('=')
            .next()
            .is_some_and(|k| k.eq_ignore_ascii_case("authSource"))
    });
    // 只有 SCRAM(及未指定时的默认方式)以路径库名为认证库;
    // PLAIN / GSSAPI / X509 / AWS 默认用 $external,补上反而认证失败
    let mechanism = params.iter().find_map(|p| {
        p.split_once('=')
            .filter(|(k, _)| k.eq_ignore_ascii_case("authMechanism"))
            .map(|(_, v)| v.to_ascii_uppercase())
    });
    let db_is_auth_source = mechanism.as_deref().is_none_or(|m| m.starts_with("SCRAM"));
    if !database.is_empty() && !user.is_empty() && !has_auth_source && db_is_auth_source {
        params.push(format!("authSource={}", encode(&database)));
    }
    let userpart = if user.is_empty() {
        String::new()
    } else {
        format!("{}@", encode(&user))
    };
    let query = if params.is_empty() {
        String::new()
    } else {
        format!("?{}", params.join("&"))
    };
    Ok(UriParts {
        sanitized: format!("{scheme}{userpart}{hosts}/{query}"),
        user,
        password,
        hosts: hosts.to_string(),
        database,
    })
}

/// 把密码拼回已去密码的连接串;没有用户名或密码为空时原样返回。
pub fn with_password(sanitized: &str, password: &str) -> String {
    if password.is_empty() {
        return sanitized.to_string();
    }
    let Ok((scheme, Some(user), _, _, _)) = pieces(sanitized) else {
        return sanitized.to_string();
    };
    let rest = &sanitized[scheme.len() + user.len()..];
    format!("{scheme}{user}:{}{rest}", encode(password))
}

/// 把库名放回连接串的路径里(编辑表单回填用);存下来的连接串不含库名,
/// 不放回去的话,原样保存会把连接的默认库清空。
pub fn with_database(sanitized: &str, database: &str) -> String {
    if database.is_empty() {
        return sanitized.to_string();
    }
    match sanitized.split_once("/?") {
        Some((head, query)) => format!("{head}/{}?{query}", encode(database)),
        None => format!(
            "{}{}",
            sanitized.trim_end_matches('/').to_string() + "/",
            encode(database)
        ),
    }
}

/// 用 `••••` 遮住密码,供表单预览。输入不合法时也按 `user:password@` 的形状遮住,
/// 不能因为用户少打一个字符就把密码原样显示出来。
pub fn redact(uri: &str) -> String {
    let Some((before_at, after_at)) = uri.rsplit_once('@') else {
        return uri.to_string();
    };
    let user_start = before_at.find("//").map_or(0, |i| i + 2);
    match before_at[user_start..].find(':') {
        Some(i) => format!("{}:••••@{after_at}", &before_at[..user_start + i]),
        None => uri.to_string(),
    }
}

/// 表单模式拼连接串。`Prefer` 返回两个候选:先加密(不校验证书),失败再明文。
/// 单主机表单默认 directConnection,直连副本集成员也能用(与 mongosh 行为一致)。
pub fn form_uris(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    auth_source: Option<&str>,
    ssl: SslMode,
) -> Vec<String> {
    let auth = if user.is_empty() {
        String::new()
    } else if password.is_empty() {
        format!("{}@", encode(user))
    } else {
        format!("{}:{}@", encode(user), encode(password))
    };
    let mut params = vec!["directConnection=true".to_string()];
    if !user.is_empty() {
        let src = auth_source
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("admin");
        params.push(format!("authSource={}", encode(src)));
    }
    let base = format!("mongodb://{auth}{host}:{port}/?{}", params.join("&"));
    let tls = |flags: &str| format!("{base}&{flags}");
    match ssl {
        SslMode::Disable => vec![tls("tls=false")],
        SslMode::Prefer => vec![
            tls("tls=true&tlsAllowInvalidCertificates=true"),
            tls("tls=false"),
        ],
        SslMode::Require => vec![tls("tls=true&tlsAllowInvalidCertificates=true")],
        SslMode::VerifyFull => vec![tls("tls=true")],
    }
}

/// 已保存连接的候选连接串(按顺序尝试)。
pub fn connect_uris(conn: &Connection, password: &str) -> Vec<String> {
    match &conn.uri {
        Some(u) => vec![with_password(u, password)],
        None => form_uris(
            &conn.host,
            conn.port,
            &conn.user,
            password,
            conn.auth_source.as_deref(),
            conn.ssl_mode,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connection::SslMode;

    #[test]
    fn split_strips_password_and_db_and_keeps_auth_semantics() {
        let p = split_uri(
            "mongodb+srv://alice:p%40ss%3Aw%2Frd@cluster0.x.mongodb.net/shop?retryWrites=true&w=majority",
        )
        .unwrap();
        assert_eq!(p.user, "alice");
        assert_eq!(p.password.as_deref(), Some("p@ss:w/rd"));
        assert_eq!(p.database, "shop");
        assert_eq!(p.hosts, "cluster0.x.mongodb.net");
        assert_eq!(
            p.sanitized,
            "mongodb+srv://alice@cluster0.x.mongodb.net/?retryWrites=true&w=majority&authSource=shop"
        );
    }

    #[test]
    fn split_keeps_explicit_auth_source_and_multi_hosts() {
        let p = split_uri("mongodb://u:pw@h1:27017,h2:27018/app?replicaSet=rs0&authSource=admin")
            .unwrap();
        assert_eq!(p.hosts, "h1:27017,h2:27018");
        assert_eq!(
            p.sanitized,
            "mongodb://u@h1:27017,h2:27018/?replicaSet=rs0&authSource=admin"
        );
        let p = split_uri("mongodb://localhost").unwrap();
        assert_eq!(
            (p.user.as_str(), p.password.as_deref(), p.database.as_str()),
            ("", None, "")
        );
        assert_eq!(p.sanitized, "mongodb://localhost/");
    }

    #[test]
    fn split_rejects_garbage() {
        assert!(split_uri("postgres://x").is_err());
        assert!(split_uri("mongodb://").is_err());
    }

    #[test]
    fn with_password_reinserts_encoded() {
        assert_eq!(
            with_password("mongodb+srv://alice@c.net/?w=1", "p@ss:w/rd"),
            "mongodb+srv://alice:p%40ss%3Aw%2Frd@c.net/?w=1"
        );
        assert_eq!(with_password("mongodb://h/", "pw"), "mongodb://h/");
        assert_eq!(with_password("mongodb://u@h/", ""), "mongodb://u@h/");
        let p = split_uri("mongodb://u:%25%23x@h/").unwrap();
        let again =
            split_uri(&with_password(&p.sanitized, p.password.as_deref().unwrap())).unwrap();
        assert_eq!(again.password.as_deref(), Some("%#x"));
    }

    #[test]
    fn form_uris_map_ssl_modes() {
        let u = |ssl| form_uris("db.local", 27017, "root", "pw", None, ssl);
        assert_eq!(
            u(SslMode::Disable),
            vec!["mongodb://root:pw@db.local:27017/?directConnection=true&authSource=admin&tls=false"]
        );
        assert_eq!(
            u(SslMode::Require),
            vec!["mongodb://root:pw@db.local:27017/?directConnection=true&authSource=admin&tls=true&tlsAllowInvalidCertificates=true"]
        );
        assert_eq!(
            u(SslMode::VerifyFull),
            vec![
                "mongodb://root:pw@db.local:27017/?directConnection=true&authSource=admin&tls=true"
            ]
        );
        let prefer = u(SslMode::Prefer);
        assert_eq!(prefer.len(), 2);
        assert!(prefer[0].contains("tls=true"));
        assert!(prefer[1].contains("tls=false"));
        let anon = form_uris("h", 1, "", "", Some("x"), SslMode::Disable);
        assert_eq!(anon, vec!["mongodb://h:1/?directConnection=true&tls=false"]);
    }

    #[test]
    fn redact_masks_password() {
        assert_eq!(redact("mongodb://u:secret@h/"), "mongodb://u:••••@h/");
        assert_eq!(redact("mongodb://h/"), "mongodb://h/");
    }

    #[test]
    fn redact_masks_password_even_in_malformed_input() {
        for raw in [
            "mongodb//u:s3cret@h/",
            "mongo://u:s3cret@h",
            "u:s3cret@h",
            "mongodb://u:s3cret@",
        ] {
            let out = redact(raw);
            assert!(!out.contains("s3cret"), "{raw} -> {out}");
        }
    }

    #[test]
    fn edit_form_uri_round_trips_database() {
        let p = split_uri("mongodb+srv://alice:pw@c.net/shop?w=1").unwrap();
        // 表单回填:去掉库名的连接串加回库名,再解析一次,库名与连接串都不变
        let shown = with_database(&p.sanitized, &p.database);
        assert_eq!(shown, "mongodb+srv://alice@c.net/shop?w=1&authSource=shop");
        let again = split_uri(&shown).unwrap();
        assert_eq!(again.database, "shop");
        assert_eq!(again.sanitized, p.sanitized);
        assert_eq!(with_database("mongodb://h/", ""), "mongodb://h/");
        assert_eq!(with_database("mongodb://h/", "a b"), "mongodb://h/a%20b");
    }

    #[test]
    fn external_auth_mechanisms_get_no_auth_source() {
        // 这些认证方式的默认认证库是 $external(MONGODB-AWS / GSSAPI 需要驱动的额外 feature,未启用)
        for uri in [
            "mongodb://u:pw@h/app?authMechanism=PLAIN",
            "mongodb://CN%3Dclient@h/app?authMechanism=MONGODB-X509&tls=true",
        ] {
            let p = split_uri(uri).unwrap();
            assert!(
                !p.sanitized.contains("authSource"),
                "{uri}: {}",
                p.sanitized
            );
            assert_eq!(p.database, "app");
        }
        let p = split_uri("mongodb://u:pw@h/app?authMechanism=SCRAM-SHA-256").unwrap();
        assert!(p.sanitized.contains("authSource=app"));
    }

    #[test]
    fn secrets_in_query_params_are_rejected_not_stored() {
        for uri in [
            "mongodb://u:pw@h/?authMechanismProperties=AWS_SESSION_TOKEN:tok123",
            "mongodb://u:pw@h/?tls=true&tlsCertificateKeyFile=/k.pem&tlsCertificateKeyFilePassword=s3cret",
            "mongodb://u:pw@h/?proxyHost=p&proxyUsername=a&proxyPassword=s3cret",
        ] {
            match split_uri(uri) {
                Ok(p) => panic!("{uri} accepted, would persist: {}", p.sanitized),
                Err(e) => assert!(!e.contains("tok123") && !e.contains("s3cret"), "{e}"),
            }
        }
        // 非机密的参数照常接受
        assert!(split_uri("mongodb://u:pw@h/?tls=true&tlsCertificateKeyFile=/k.pem").is_ok());
    }
}
