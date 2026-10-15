use crate::core::connection::Connection;
use crate::error::{AppError, AppResult, ErrorKind};
use rust_i18n::t;

/// 后端无关的密钥串抽象,便于测试替换。
pub trait SecretBackend {
    fn set(&self, service: &str, account: &str, secret: &str) -> AppResult<()>;
    fn get(&self, service: &str, account: &str) -> AppResult<Option<String>>;
    fn delete(&self, service: &str, account: &str) -> AppResult<()>;
}

const SERVICE: &str = "com.dbstudio.app";

fn account_key(conn: &Connection) -> String {
    format!("{}:{}", conn.env_label(), conn.id)
}

/// 保存密码:Local 写进 connection 的明文字段;其余环境写进密钥串后端。
/// 返回应持久化到 json 的 connection(明文字段已按规则设置)。
pub fn save_password<B: SecretBackend>(
    backend: &B,
    mut conn: Connection,
    password: &str,
) -> AppResult<Connection> {
    if conn.env.stores_password_in_plaintext() {
        conn.plaintext_password = Some(password.to_string());
    } else {
        conn.plaintext_password = None;
        backend.set(SERVICE, &account_key(&conn), password)?;
    }
    Ok(conn)
}

/// 取密码:Local 从明文字段(缺失则视为空密码,支持无密码/trust 认证);其余从密钥串。
pub fn load_password<B: SecretBackend>(backend: &B, conn: &Connection) -> AppResult<String> {
    if conn.env.stores_password_in_plaintext() {
        Ok(conn.plaintext_password.clone().unwrap_or_default())
    } else {
        backend.get(SERVICE, &account_key(conn))?.ok_or_else(|| {
            AppError::new(
                ErrorKind::Credential,
                t!("core.err.keychain_password_not_found"),
            )
        })
    }
}

/// 删除密码:Local 无需操作(随 json 删除);其余从密钥串删除。
pub fn delete_password<B: SecretBackend>(backend: &B, conn: &Connection) -> AppResult<()> {
    if conn.env.stores_password_in_plaintext() {
        Ok(())
    } else {
        backend.delete(SERVICE, &account_key(conn))
    }
}

pub struct KeyringBackend;
impl SecretBackend for KeyringBackend {
    fn set(&self, service: &str, account: &str, secret: &str) -> AppResult<()> {
        keyring::Entry::new(service, account)
            .and_then(|e| e.set_password(secret))
            .map_err(|e| {
                AppError::new(ErrorKind::Credential, t!("core.err.keychain_write_failed"))
                    .with_detail(e.to_string())
            })
    }
    fn get(&self, service: &str, account: &str) -> AppResult<Option<String>> {
        match keyring::Entry::new(service, account).and_then(|e| e.get_password()) {
            Ok(p) => Ok(Some(p)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AppError::new(
                ErrorKind::Credential,
                t!("core.err.keychain_read_failed"),
            )
            .with_detail(e.to_string())),
        }
    }
    fn delete(&self, service: &str, account: &str) -> AppResult<()> {
        match keyring::Entry::new(service, account).and_then(|e| e.delete_password()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(AppError::new(
                ErrorKind::Credential,
                t!("core.err.keychain_delete_failed"),
            )
            .with_detail(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connection::{DbKind, Env};
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeBackend {
        store: RefCell<HashMap<String, String>>,
    }
    impl SecretBackend for FakeBackend {
        fn set(&self, _s: &str, a: &str, secret: &str) -> AppResult<()> {
            self.store.borrow_mut().insert(a.into(), secret.into());
            Ok(())
        }
        fn get(&self, _s: &str, a: &str) -> AppResult<Option<String>> {
            Ok(self.store.borrow().get(a).cloned())
        }
        fn delete(&self, _s: &str, a: &str) -> AppResult<()> {
            self.store.borrow_mut().remove(a);
            Ok(())
        }
    }

    fn conn(env: Env) -> Connection {
        Connection {
            id: "c1".into(),
            name: "n".into(),
            kind: DbKind::Pg,
            env,
            host: "h".into(),
            port: 5432,
            user: "u".into(),
            database: "d".into(),
            plaintext_password: None,
            file_path: None,
            ssl_mode: Default::default(),
            uri: None,
            auth_source: None,
        }
    }

    #[test]
    fn local_keeps_plaintext_and_skips_keychain() {
        let b = FakeBackend::default();
        let saved = save_password(&b, conn(Env::Local), "pw").unwrap();
        assert_eq!(saved.plaintext_password.as_deref(), Some("pw"));
        assert!(b.store.borrow().is_empty());
        assert_eq!(load_password(&b, &saved).unwrap(), "pw");
    }

    #[test]
    fn local_missing_plaintext_is_empty_password() {
        // 旧连接或无密码的 local 连接:缺明文不再报错,视为空密码
        let b = FakeBackend::default();
        let c = conn(Env::Local); // plaintext_password: None
        assert_eq!(load_password(&b, &c).unwrap(), "");
    }

    #[test]
    fn prod_uses_keychain_and_clears_plaintext() {
        let b = FakeBackend::default();
        let saved = save_password(&b, conn(Env::Prod), "pw").unwrap();
        assert_eq!(saved.plaintext_password, None);
        assert_eq!(b.store.borrow().len(), 1);
        assert_eq!(load_password(&b, &saved).unwrap(), "pw");
    }

    #[test]
    fn delete_removes_keychain_entry_for_prod_only() {
        let b = FakeBackend::default();
        let saved = save_password(&b, conn(Env::Prod), "pw").unwrap();
        assert_eq!(b.store.borrow().len(), 1);
        delete_password(&b, &saved).unwrap();
        assert!(b.store.borrow().is_empty());
        // local: no-op, must not error
        let local = save_password(&b, conn(Env::Local), "pw").unwrap();
        assert!(delete_password(&b, &local).is_ok());
    }
}
