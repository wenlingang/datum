//! SQL 文本工具:格式化、拆语句、风险检查、拼分页 SQL 等(纯计算,同步)。

use crate::runtime::from;
use datum_core::core::connection::DbKind;
use datum_core::sql::risk::{self, RiskFinding, RiskLevel};
use datum_core::sql::{Dialect, build, format, stmt};
use nsh_core::JsonValue;
use nsh_core::prelude::*;

fn dialect(kind: &str) -> Result<Dialect, String> {
    let k: DbKind = from(JsonValue::String(kind.to_string()), "kind")?;
    Dialect::from_kind(&k).ok_or_else(|| format!("{kind} 没有 SQL 方言"))
}

/// 风险检查结果 → `[{ sql, level: "danger" | "warning", kind, object }]`。
pub(crate) fn findings_json(findings: Vec<RiskFinding>) -> JsonValue {
    JsonValue::Array(
        findings
            .into_iter()
            .map(|f| {
                serde_json::json!({
                    "sql": f.sql,
                    "level": match f.level {
                        RiskLevel::Danger => "danger",
                        RiskLevel::Warning => "warning",
                    },
                    "kind": f.kind,
                    "object": f.object,
                })
            })
            .collect(),
    )
}

/// SQL 文本工具。
#[module]
pub struct Sql;

#[module]
impl Sql {
    /// 格式化 SQL。
    pub fn format(sql: String) -> String {
        format::format_sql(&sql)
    }

    /// 把文档拆成语句(去掉空语句)。
    pub fn statements(doc: String) -> Vec<String> {
        stmt::statements(&doc)
    }

    /// 光标(字符偏移)所在的语句。
    pub fn current_statement(doc: String, caret: u32) -> String {
        // TSX 给的是 UTF-16 偏移,core 要字节偏移
        let byte = utf16_to_byte(&doc, caret as usize);
        stmt::current_statement(&doc, byte)
    }

    /// 是否为返回结果集的查询语句。
    pub fn is_query(sql: String) -> bool {
        stmt::is_query(&sql)
    }

    /// 是否整段只读(免确认执行的前提)。
    pub fn is_read_only(doc: String) -> bool {
        risk::is_read_only(&doc)
    }

    /// SQL 风险检查:破坏性语句清单。
    pub fn analyze(doc: String) -> JsonValue {
        findings_json(risk::analyze_sql(&doc))
    }

    /// Redis 命令风险检查。
    pub fn analyze_redis(cmd: String) -> JsonValue {
        findings_json(risk::analyze_redis(&cmd))
    }

    /// 带方言转义的标识符。
    pub fn ident(name: String, kind: String) -> Result<String, String> {
        Ok(build::ident(&name, dialect(&kind)?))
    }

    /// 带方言转义的字符串字面量。
    pub fn literal(value: String, kind: String) -> Result<String, String> {
        Ok(build::literal(&value, dialect(&kind)?))
    }

    /// 分页浏览一张表的 SQL;`sort_col` 给定时按它排序。
    pub fn page_sql(
        table: String,
        page: u32,
        page_size: u32,
        kind: String,
        sort_col: Option<String>,
        descending: Option<bool>,
    ) -> Result<String, String> {
        let d = dialect(&kind)?;
        let sort = sort_col
            .as_deref()
            .map(|c| (c, descending.unwrap_or(false)));
        Ok(build::page_sql(&table, page as usize, page_size as usize, d, sort))
    }

    /// 从 SQL 里猜主表名(结果集可编辑、导出 INSERT 用)。
    pub fn guess_table_name(sql: String) -> Option<String> {
        build::guess_table_name(&sql)
    }
}

/// UTF-16 偏移 → 字节偏移(越界时取末尾)。
pub(crate) fn utf16_to_byte(s: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (byte, ch) in s.char_indices() {
        if units >= utf16 {
            return byte;
        }
        units += ch.len_utf16();
    }
    s.len()
}
