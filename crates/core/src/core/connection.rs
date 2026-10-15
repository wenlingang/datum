use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum DbKind {
    Pg,
    Mysql,
    Redis,
    Sqlite,
    Mongo,
}

impl DbKind {
    pub const ALL: [DbKind; 5] = [
        DbKind::Pg,
        DbKind::Mysql,
        DbKind::Sqlite,
        DbKind::Redis,
        DbKind::Mongo,
    ];

    /// 展示名(状态栏、类型选择)。
    pub fn label(&self) -> &'static str {
        match self {
            DbKind::Pg => "PostgreSQL",
            DbKind::Mysql => "MySQL",
            DbKind::Sqlite => "SQLite",
            DbKind::Redis => "Redis",
            DbKind::Mongo => "MongoDB",
        }
    }

    /// 连接串预览的 scheme。
    pub fn scheme(&self) -> &'static str {
        match self {
            DbKind::Pg => "postgresql",
            DbKind::Mysql => "mysql",
            DbKind::Sqlite => "sqlite",
            DbKind::Redis => "redis",
            DbKind::Mongo => "mongodb",
        }
    }

    /// SQL 编辑器能力:关键字大写、SQL 补全、AI 对话、导入 / 导出 SQL、ERD。
    pub fn supports_sql(&self) -> bool {
        matches!(self, DbKind::Pg | DbKind::Mysql | DbKind::Sqlite)
    }

    /// 是否加载列 schema(补全与 AI 上下文)。
    pub fn has_columns(&self) -> bool {
        self.supports_sql()
    }

    /// 是否提供文档级的编辑 / 新增 / 删除。
    pub fn supports_doc_edit(&self) -> bool {
        matches!(self, DbKind::Mongo)
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

/// 网络连接的加密方式(PG / MySQL / Redis;SQLite 是本地文件,不涉及)。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum SslMode {
    /// 不加密
    Disable,
    /// 服务器支持就加密,不支持退回明文;不校验证书(与 libpq 的 prefer 一致)。
    /// Redis 无法在同一端口协商加密,此档等同不加密。
    #[default]
    Prefer,
    /// 必须加密,不校验证书(防窃听,不防中间人)
    Require,
    /// 必须加密,并用系统证书库校验证书与主机名
    VerifyFull,
}

impl SslMode {
    pub const ALL: [SslMode; 4] = [Self::Disable, Self::Prefer, Self::Require, Self::VerifyFull];
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
    /// 加密方式;旧配置没有这一项时取默认的「优先」
    #[serde(default)]
    pub ssl_mode: SslMode,
    /// 仅 Mongo:连接串模式下的 URI(已去除密码与库名)。Some 即为连接串模式。
    #[serde(default)]
    pub uri: Option<String>,
    /// 仅 Mongo 表单模式:认证库,None 等同 "admin"。
    #[serde(default)]
    pub auth_source: Option<String>,
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
            ssl_mode: Default::default(),
            uri: None,
            auth_source: None,
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

    #[test]
    fn capability_truth_table() {
        use DbKind::*;
        let rows = [
            (Pg, true, true, false),
            (Mysql, true, true, false),
            (Sqlite, true, true, false),
            (Redis, false, false, false),
            (Mongo, false, false, true),
        ];
        for (k, sql, cols, doc) in rows {
            assert_eq!(k.supports_sql(), sql, "{k:?} supports_sql");
            assert_eq!(k.has_columns(), cols, "{k:?} has_columns");
            assert_eq!(k.supports_doc_edit(), doc, "{k:?} supports_doc_edit");
        }
        assert_eq!(DbKind::ALL.len(), 5);
        assert_eq!(Mongo.label(), "MongoDB");
        assert_eq!(Mongo.scheme(), "mongodb");
    }

    #[test]
    fn mongo_kind_serializes_camel_case() {
        assert_eq!(serde_json::to_string(&DbKind::Mongo).unwrap(), "\"mongo\"");
    }

    #[test]
    fn old_config_without_mongo_fields_deserializes() {
        let json = r#"{"id":"c1","name":"n","kind":"pg","env":"local","host":"h","port":5432,"user":"u","database":"d"}"#;
        let c: Connection = serde_json::from_str(json).unwrap();
        assert_eq!(c.uri, None);
        assert_eq!(c.auth_source, None);
    }
}
