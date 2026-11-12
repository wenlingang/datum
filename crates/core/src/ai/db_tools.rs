//! AI 自己查数据用的三个工具:列出表、查看表结构、执行只读查询。
//!
//! 只读查询两道关:静态判定必须是只读语句([`is_read_only`]),再在数据库强制只读的会话里执行
//! (PG / MySQL 服务端限时)。写操作不提供工具:模型只能在回复里以 SQL 代码块给出,
//! 由用户在卡片上确认后执行。

use crate::ai::agent::{ToolFuture, ToolOutput, ToolRunner, ToolSpec};
use crate::core::connection::DbKind;
use crate::pg::client::QueryResult;
use crate::service;
use crate::sql::risk::is_read_only;
use crate::AppState;
use rust_i18n::t;
use serde_json::{json, Value};

/// 查询结果最多带回界面的行数。
pub const UI_ROWS: u32 = 200;
/// 交给模型的行数上限。
const MODEL_ROWS: usize = 50;
/// 交给模型的文本总长上限(字符)。
const MODEL_CHARS: usize = 6000;
/// 单个单元格交给模型的长度上限。
const CELL_CHARS: usize = 200;
/// 单条查询的服务端限时。
const QUERY_TIMEOUT_MS: u32 = 30_000;
/// 表清单交给模型的上限。
const MAX_TABLES: usize = 500;

pub const LIST_TABLES: &str = "list_tables";
pub const DESCRIBE_TABLE: &str = "describe_table";
pub const RUN_QUERY: &str = "run_query";

/// 工具定义(说明用英文:各家模型对英文工具说明最稳)。
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: LIST_TABLES.into(),
            description: "List all tables (and views) in the current database.".into(),
            parameters: json!({ "type": "object", "properties": {} }),
        },
        ToolSpec {
            name: DESCRIBE_TABLE.into(),
            description: "Show the columns (name, type, nullable, primary key, default, comment) and indexes of one table.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "table": { "type": "string", "description": "Table name" } },
                "required": ["table"],
            }),
        },
        ToolSpec {
            name: RUN_QUERY.into(),
            description: format!(
                "Run ONE read-only SQL statement (SELECT / WITH … SELECT / SHOW / EXPLAIN without ANALYZE) and get the rows back. \
                 The session is read-only: INSERT, UPDATE, DELETE, DDL and other writes are rejected. \
                 At most {MODEL_ROWS} rows are returned to you; use aggregates, WHERE and LIMIT to keep results small. \
                 To change data, do not use this tool: write the statement in a ```sql code block in your reply for the user to confirm."
            ),
            parameters: json!({
                "type": "object",
                "properties": { "sql": { "type": "string", "description": "A single read-only SQL statement" } },
                "required": ["sql"],
            }),
        },
    ]
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// 查询结果 → 给模型的文本:制表符分隔,行数与总长都有上限,超出注明。
pub fn result_for_model(r: &QueryResult, capped: bool) -> String {
    if r.columns.is_empty() {
        return format!("OK, {} rows affected", r.affected.unwrap_or(0));
    }
    let mut out = format!("{} row(s){}\n", r.rows.len(), if capped { " or more (capped)" } else { "" });
    out.push_str(&r.columns.join("\t"));
    let mut shown = 0;
    for row in &r.rows {
        let line: Vec<String> = row.iter().map(|c| c.as_deref().map(|v| cut(v, CELL_CHARS)).unwrap_or_else(|| "NULL".into())).collect();
        let line = line.join("\t");
        if shown >= MODEL_ROWS || out.len() + line.len() > MODEL_CHARS {
            out.push_str(&format!("\n… {} more row(s) not shown", r.rows.len() - shown));
            break;
        }
        out.push('\n');
        out.push_str(&line);
        shown += 1;
    }
    out
}

/// 在某个连接上执行工具。
pub struct DbTools<'a> {
    pub state: &'a AppState,
    pub kind: DbKind,
    pub id: String,
}

fn fail(msg: String) -> ToolOutput {
    ToolOutput {
        for_model: format!("Error: {msg}"),
        ok: false,
        summary: msg.clone(),
        detail: json!({ "error": msg }),
    }
}

impl DbTools<'_> {
    async fn list_tables(&self) -> ToolOutput {
        match service::list_objects(self.state, &self.kind, &self.id).await {
            Ok(names) => ToolOutput {
                for_model: if names.is_empty() {
                    "(no tables)".into()
                } else {
                    let mut s = names.iter().take(MAX_TABLES).cloned().collect::<Vec<_>>().join("\n");
                    if names.len() > MAX_TABLES {
                        s.push_str(&format!("\n… {} more", names.len() - MAX_TABLES));
                    }
                    s
                },
                ok: true,
                summary: t!("ai.tool.tables", n = names.len()).to_string(),
                detail: json!({ "tables": names }),
            },
            Err(e) => fail(e.display()),
        }
    }

    async fn describe_table(&self, args: &Value) -> ToolOutput {
        let Some(table) = args.get("table").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) else {
            return fail(t!("ai.tool.missing_arg", arg = "table").to_string());
        };
        match service::table_detail(self.state, &self.kind, &self.id, table).await {
            Ok(d) => {
                let mut s = format!("Table {table}\nColumns:");
                for c in &d.columns {
                    s.push_str(&format!(
                        "\n- {} {}{}{}{}{}",
                        c.name,
                        c.data_type,
                        if c.is_pk { " PRIMARY KEY" } else { "" },
                        if c.not_null { " NOT NULL" } else { "" },
                        c.default.as_deref().map(|v| format!(" DEFAULT {v}")).unwrap_or_default(),
                        c.comment.as_deref().filter(|v| !v.is_empty()).map(|v| format!(" -- {v}")).unwrap_or_default(),
                    ));
                }
                if !d.indexes.is_empty() {
                    s.push_str("\nIndexes:");
                    for i in &d.indexes {
                        s.push_str(&format!(
                            "\n- {} ({}){}",
                            i.name,
                            i.columns,
                            if i.is_primary { " PRIMARY" } else if i.is_unique { " UNIQUE" } else { "" }
                        ));
                    }
                }
                ToolOutput {
                    for_model: s,
                    ok: true,
                    summary: t!("ai.tool.columns", table = table, n = d.columns.len()).to_string(),
                    detail: json!({ "table": table, "columns": d.columns, "indexes": d.indexes }),
                }
            }
            Err(e) => fail(e.display()),
        }
    }

    async fn run_query(&self, args: &Value) -> ToolOutput {
        let Some(sql) = args.get("sql").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()) else {
            return fail(t!("ai.tool.missing_arg", arg = "sql").to_string());
        };
        let sql = sql.trim_end_matches(';').trim();
        if !is_read_only(sql) {
            let msg = t!("ai.tool.not_read_only").to_string();
            return ToolOutput {
                for_model: "Error: run_query only accepts a single read-only statement. To change data, put the SQL in a ```sql code block in your reply so the user can review and run it.".into(),
                ok: false,
                summary: msg.clone(),
                detail: json!({ "sql": sql, "error": msg }),
            };
        }
        match service::query_read_only_timeout(self.state, &self.kind, &self.id, sql, UI_ROWS, QUERY_TIMEOUT_MS).await {
            Ok(r) => {
                let capped = r.rows.len() as u32 >= UI_ROWS;
                ToolOutput {
                    for_model: result_for_model(&r, capped),
                    ok: true,
                    summary: if capped {
                        t!("ai.tool.rows_capped", n = r.rows.len()).to_string()
                    } else {
                        t!("ai.tool.rows", n = r.rows.len()).to_string()
                    },
                    detail: json!({ "sql": sql, "columns": r.columns, "rows": r.rows, "capped": capped }),
                }
            }
            Err(e) => {
                let msg = e.display();
                ToolOutput {
                    for_model: format!("Error: {msg}"),
                    ok: false,
                    summary: msg.clone(),
                    detail: json!({ "sql": sql, "error": msg }),
                }
            }
        }
    }
}

impl ToolRunner for DbTools<'_> {
    fn run<'a>(&'a self, name: &'a str, args: &'a Value) -> ToolFuture<'a> {
        Box::pin(async move {
            match name {
                LIST_TABLES => self.list_tables().await,
                DESCRIBE_TABLE => self.describe_table(args).await,
                RUN_QUERY => self.run_query(args).await,
                other => fail(t!("ai.tool.unknown", name = other).to_string()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(rows: usize, width: usize) -> QueryResult {
        QueryResult {
            columns: vec!["id".into(), "name".into()],
            rows: (0..rows).map(|i| vec![Some(i.to_string()), Some("x".repeat(width))]).collect(),
            affected: None,
        }
    }

    #[test]
    fn small_results_are_complete() {
        let s = result_for_model(&result(3, 4), false);
        assert!(s.starts_with("3 row(s)\nid\tname\n0\txxxx"));
        assert!(!s.contains("more row"));
    }

    #[test]
    fn rows_and_size_are_capped() {
        let s = result_for_model(&result(120, 4), false);
        assert!(s.contains("… 70 more row(s) not shown"), "{s}");
        let s = result_for_model(&result(40, 500), true);
        assert!(s.len() < MODEL_CHARS + 400, "{}", s.len());
        assert!(s.contains("more row(s) not shown"));
        assert!(s.starts_with("40 row(s) or more (capped)"));
        // 单元格截断
        assert!(!s.contains(&"x".repeat(CELL_CHARS + 1)));
    }

    #[test]
    fn statements_without_columns_report_affected() {
        let r = QueryResult { columns: vec![], rows: vec![], affected: Some(2) };
        assert_eq!(result_for_model(&r, false), "OK, 2 rows affected");
    }

    #[test]
    fn specs_are_well_formed() {
        let s = specs();
        assert_eq!(s.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), [LIST_TABLES, DESCRIBE_TABLE, RUN_QUERY]);
        assert!(s.iter().all(|t| t.parameters["type"] == "object"));
    }
}
