use crate::core::connection::Connection;
use crate::error::{AppError, AppResult, ErrorKind};
use rust_i18n::t;
use std::path::{Path, PathBuf};

/// 负责把连接列表读写到 connections.json(一个目录下的固定文件名)。
pub struct ConnectionRepo {
    path: PathBuf,
}

impl ConnectionRepo {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            path: config_dir.join("connections.json"),
        }
    }

    pub fn load(&self) -> AppResult<Vec<Connection>> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let bytes = std::fs::read(&self.path).map_err(|e| {
            AppError::new(ErrorKind::Config, t!("core.err.read_config_failed"))
                .with_detail(e.to_string())
        })?;
        // 旧版本写出的文件可能是 0644:读取时顺手收紧(失败不影响读取)
        let _ = self.restrict_permissions();
        serde_json::from_slice(&bytes).map_err(|e| {
            AppError::new(ErrorKind::Config, t!("core.err.parse_config_failed"))
                .with_detail(e.to_string())
        })
    }

    pub fn save(&self, conns: &[Connection]) -> AppResult<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AppError::new(ErrorKind::Config, t!("core.err.create_config_dir_failed"))
                    .with_detail(e.to_string())
            })?;
        }
        let json = serde_json::to_vec_pretty(conns).map_err(|e| {
            AppError::new(ErrorKind::Config, t!("core.err.serialize_config_failed"))
                .with_detail(e.to_string())
        })?;
        std::fs::write(&self.path, json).map_err(|e| {
            AppError::new(ErrorKind::Config, t!("core.err.write_config_failed"))
                .with_detail(e.to_string())
        })?;
        self.restrict_permissions()
    }

    /// local 环境的连接密码明文存在这个文件里,收紧到仅本用户可读写。
    fn restrict_permissions(&self) -> AppResult<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600)).map_err(
                |e| {
                    AppError::new(ErrorKind::Config, t!("core.err.set_permissions_failed"))
                        .with_detail(e.to_string())
                },
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connection::{DbKind, Env};

    fn sample() -> Connection {
        Connection {
            id: "c1".into(),
            name: "local-pg".into(),
            kind: DbKind::Pg,
            env: Env::Local,
            host: "127.0.0.1".into(),
            port: 5432,
            user: "postgres".into(),
            database: "postgres".into(),
            plaintext_password: Some("pw".into()),
            file_path: None,
        }
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = std::env::temp_dir().join(format!("dbstudio-test-{}", std::process::id()));
        let repo = ConnectionRepo::new(&dir);
        assert_eq!(repo.load().unwrap().len(), 0);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = std::env::temp_dir().join(format!("dbstudio-rt-{}", std::process::id()));
        let repo = ConnectionRepo::new(&dir);
        repo.save(&[sample()]).unwrap();
        let loaded = repo.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "local-pg");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 含明文密码的配置文件只有本用户可读;旧版留下的宽松权限在读取时也会被收紧。
    #[cfg(unix)]
    #[test]
    fn config_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("dbstudio-perm-{}", std::process::id()));
        let repo = ConnectionRepo::new(&dir);
        repo.save(&[sample()]).unwrap();
        let file = dir.join("connections.json");
        let mode = |f: &std::path::Path| std::fs::metadata(f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        repo.load().unwrap();
        assert_eq!(mode(&file), 0o600);
        std::fs::remove_dir_all(&dir).ok();
    }
}
