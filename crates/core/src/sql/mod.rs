//! SQL 文本处理:语句切分、破坏性识别、生成与格式化。纯函数,UI 无关。

pub mod build;
pub mod format;
pub mod risk;
pub mod stmt;

use crate::core::connection::DbKind;

/// 关系型方言(Redis 没有 SQL 方言)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dialect {
    Pg,
    Mysql,
    Sqlite,
}

impl Dialect {
    pub fn from_kind(kind: &DbKind) -> Option<Self> {
        match kind {
            DbKind::Pg => Some(Self::Pg),
            DbKind::Mysql => Some(Self::Mysql),
            DbKind::Sqlite => Some(Self::Sqlite),
            DbKind::Redis => None,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Pg => "PostgreSQL",
            Self::Mysql => "MySQL",
            Self::Sqlite => "SQLite",
        }
    }
}
