use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum DbKind {
    Pg,
    Mysql,
    Redis,
    Sqlite,
}

impl DbKind {
    pub const ALL: [DbKind; 4] = [DbKind::Pg, DbKind::Mysql, DbKind::Sqlite, DbKind::Redis];

    /// 展示名(状态栏、类型选择)。
    pub fn label(&self) -> &'static str {
        match self {
            DbKind::Pg => "PostgreSQL",
            DbKind::Mysql => "MySQL",
            DbKind::Sqlite => "SQLite",
            DbKind::Redis => "Redis",
        }
    }

    /// 连接串预览的 scheme。
    pub fn scheme(&self) -> &'static str {
        match self {
            DbKind::Pg => "postgresql",
            DbKind::Mysql => "mysql",
            DbKind::Sqlite => "sqlite",
            DbKind::Redis => "redis",
        }
    }

    pub fn is_relational(&self) -> bool {
        !matches!(self, DbKind::Redis)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Env {
    Local,
    Staging,
    Prod,
}

impl Env {
    pub const ALL: [Env; 3] = [Env::Local, Env::Staging, Env::Prod];

    /// 仅 Local 把密码明文存本地;其余走钥匙串。
    pub fn stores_password_in_plaintext(&self) -> bool {
        matches!(self, Env::Local)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub kind: DbKind,
    pub env: Env,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    /// 仅当 env == Local 时这里才有明文密码;否则为 None(密码在钥匙串)。
    #[serde(default)]
    pub plaintext_password: Option<String>,
    /// 仅 SQLite 使用:数据库文件的绝对路径。其他类型为 None。
    #[serde(default)]
    pub file_path: Option<String>,
}

impl Connection {
    pub fn env_label(&self) -> &'static str {
        match self.env {
            Env::Local => "local",
            Env::Staging => "staging",
            Env::Prod => "prod",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_stores_plaintext_other_envs_do_not() {
        assert!(Env::Local.stores_password_in_plaintext());
        assert!(!Env::Staging.stores_password_in_plaintext());
        assert!(!Env::Prod.stores_password_in_plaintext());
    }
    #[test]
    fn roundtrips_json() {
        let c = Connection {
            id: "c1".into(),
            name: "prod-pg".into(),
            kind: DbKind::Pg,
            env: Env::Prod,
            host: "db.example.com".into(),
            port: 5432,
            user: "app".into(),
            database: "appdb".into(),
            plaintext_password: None,
            file_path: None,
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: Connection = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn deserializes_mysql_kind_from_frontend_json() {
        // 保存连接时前端传来的 JSON 形态;kind 缺少 Mysql 变体曾导致反序列化失败
        let json = r#"{"id":"c2","name":"local-mysql","kind":"mysql","env":"local",
            "host":"127.0.0.1","port":3306,"user":"root","database":"test"}"#;
        let c: Connection = serde_json::from_str(json).unwrap();
        assert_eq!(c.kind, DbKind::Mysql);
        assert!(serde_json::to_string(&c)
            .unwrap()
            .contains(r#""kind":"mysql""#));
    }
}
