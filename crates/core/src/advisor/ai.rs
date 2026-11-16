//! AI 候选:把 SQL、相关表的建表语句与当前执行计划交给模型(不含任何行数据),
//! 让它只回 JSON;表名、列名在本地逐个校验(大小写纠正成真实名字),不合格的丢掉并说明。
//! 生产库没有允许 AI 读数据时,SQL 与计划里的字面量(字符串、数字)换成 `?`。

use super::rules::{covered_by_existing, meta_for, TableMeta};
use super::shape::{Shape, TableRef};
use super::{AiOutcome, Candidate, IndexCol, Source};
use crate::ai::chat::{self, ChatMessage};
use crate::ai::provider;
use crate::ai::settings::AiSettingsRepo;
use crate::core::connection::DbKind;
use crate::explain::{Plan, PlanNode};
use crate::service;
use crate::sql::Dialect;
use crate::AppState;
use regex::Regex;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::LazyLock;

/// AI 最多补几个候选。
pub const MAX_AI: usize = 3;
const MAX_REASON_CHARS: usize = 200;

static STR_LIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"'(?:[^']|'')*'").unwrap());
static NUM_LIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b").unwrap());

/// 字面量打码:字符串 → `'?'`,数字 → `?`(标识符里的数字不动)。
pub fn mask_literals(text: &str) -> String {
    let s = STR_LIT.replace_all(text, "'?'");
    NUM_LIT.replace_all(&s, "?").into_owned()
}

/// 计划树 → 缩进文本(给模型看)。
pub fn plan_text(tree: &PlanNode) -> String {
    fn go(n: &PlanNode, depth: usize, out: &mut String) {
        out.push_str(&"  ".repeat(depth));
        out.push_str(&n.label);
        if !n.detail.is_empty() {
            out.push_str(&format!(" [{}]", n.detail));
        }
        if let Some(c) = n.cost {
            out.push_str(&format!(" cost={c:.1}"));
        }
        if let Some(r) = n.rows {
            out.push_str(&format!(" rows={r:.0}"));
        }
        if n.full_scan {
            out.push_str(" FULL_SCAN");
        }
        out.push('\n');
        for c in &n.children {
            go(c, depth + 1, out);
        }
    }
    let mut s = String::new();
    go(tree, 0, &mut s);
    s
}

pub fn messages(dialect: &str, sql: &str, ddls: &[String], plan: Option<&str>, locale: &str) -> Vec<ChatMessage> {
    let system = format!(
        "You are a database index advisor for {dialect}. Propose B-tree indexes that would make the given query faster.\n\
         Reply with ONE JSON object and nothing else (no code fence, no prose):\n\
         {{\"indexes\":[{{\"table\":\"<table>\",\"columns\":[\"<column>\", \"<column> DESC\"],\"reason\":\"<short reason>\"}}]}}\n\
         Rules:\n\
         - At most {MAX_AI} indexes, best first. Only plain column indexes (no expressions, no partial indexes, no INCLUDE).\n\
         - table and columns must be names from the given CREATE TABLE statements, spelled exactly.\n\
         - Do not repeat an index that already exists (see the existing indexes) or one whose leading columns an existing index already covers.\n\
         - Order columns: equality filters first, then one range filter, then ORDER BY columns when that avoids a sort.\n\
         - reason: one short sentence in the language with locale code {locale}.\n\
         - If no index would help, return {{\"indexes\":[]}}."
    );
    let mut user = format!("Query:\n{sql}\n\nTables:\n");
    for d in ddls {
        user.push_str(d.trim());
        user.push_str("\n\n");
    }
    if let Some(p) = plan {
        user.push_str("Current plan:\n");
        user.push_str(p);
    }
    vec![ChatMessage { role: "system".into(), content: system }, ChatMessage { role: "user".into(), content: user }]
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    indexes: Vec<RawIndex>,
}

#[derive(Deserialize)]
struct RawIndex {
    #[serde(default)]
    table: String,
    #[serde(default)]
    columns: Vec<String>,
    #[serde(default)]
    reason: String,
}

fn json_slice(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    (end > start).then(|| &reply[start..=end])
}

/// 解析并校验模型回复,合格的追加到 `cands`(去重)。返回 (追加数, 丢掉的描述)。
pub fn merge(reply: &str, shape: &Shape, metas: &HashMap<String, TableMeta>, cands: &mut Vec<Candidate>, d: Dialect) -> Result<(usize, Vec<String>), String> {
    let raw: Raw = json_slice(reply).and_then(|s| serde_json::from_str(s).ok()).ok_or_else(|| rust_i18n::t!("core.advisor.err.ai_unreadable").to_string())?;
    let tables: Vec<&TableRef> = shape.blocks.iter().flat_map(|b| &b.tables).collect();
    let mut added = 0;
    let mut dropped = vec![];
    for r in raw.indexes {
        let describe = || format!("{}({})", r.table, r.columns.join(", "));
        if added >= MAX_AI {
            break;
        }
        let want = r.table.rsplit('.').next().unwrap_or("").trim_matches(|c| c == '"' || c == '`');
        let Some(t) = tables.iter().find(|t| t.name == want).or_else(|| tables.iter().find(|t| t.name.eq_ignore_ascii_case(want))) else {
            dropped.push(describe());
            continue;
        };
        let Some(meta) = meta_for(metas, t) else {
            dropped.push(describe());
            continue;
        };
        let mut cols: Vec<IndexCol> = vec![];
        let mut ok = !r.columns.is_empty() && r.columns.len() <= 5;
        for c in &r.columns {
            let c = c.trim();
            let (name, desc) = match c.rsplit_once(char::is_whitespace) {
                Some((n, dir)) if dir.eq_ignore_ascii_case("desc") => (n.trim(), true),
                Some((n, dir)) if dir.eq_ignore_ascii_case("asc") => (n.trim(), false),
                _ => (c, false),
            };
            let name = name.trim_matches(|c| c == '"' || c == '`');
            match meta.columns.iter().find(|x| *x == name).or_else(|| meta.columns.iter().find(|x| x.eq_ignore_ascii_case(name))) {
                Some(real) if !cols.iter().any(|x| x.name == *real) => cols.push(IndexCol { name: real.clone(), desc }),
                _ => ok = false,
            }
        }
        if !ok {
            dropped.push(describe());
            continue;
        }
        let plain: Vec<String> = cols.iter().map(|c| c.name.clone()).collect();
        if covered_by_existing(meta, &plain, 0) {
            dropped.push(describe());
            continue;
        }
        // 与已有候选相同:不重复列出
        if cands.iter().any(|x| x.table.eq_ignore_ascii_case(&t.name) && x.cols().iter().map(|s| s.to_lowercase()).eq(plain.iter().map(|s| s.to_lowercase()))) {
            continue;
        }
        let reason: String = r.reason.trim().chars().take(MAX_REASON_CHARS).collect();
        let reason = if reason.is_empty() { rust_i18n::t!("core.advisor.reason.ai").to_string() } else { reason };
        cands.push(Candidate::new(t, cols, reason, Source::Ai, d));
        added += 1;
    }
    Ok((added, dropped))
}

/// 调用当前配置的模型补充候选(一次性请求,不进对话历史)。
#[allow(clippy::too_many_arguments)]
pub async fn suggest(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    sql: &str,
    shape: &Shape,
    metas: &HashMap<String, TableMeta>,
    baseline: Option<&Plan>,
    cands: &mut Vec<Candidate>,
    d: Dialect,
) -> AiOutcome {
    let settings = AiSettingsRepo::new(&state.config_dir).load();
    let ep = match provider::resolve(&settings) {
        Ok(ep) => ep,
        Err(e) => return AiOutcome { error: Some(e), ..Default::default() },
    };
    let masked = !crate::ai::commands::data_allowed(state, &settings, id);
    let mask = |s: &str| if masked { mask_literals(s) } else { s.to_string() };
    let mut ddls = vec![];
    let mut seen: Vec<String> = vec![];
    for t in shape.blocks.iter().flat_map(|b| &b.tables) {
        let Some(meta) = meta_for(metas, t) else { continue };
        if seen.contains(&t.name.to_lowercase()) {
            continue;
        }
        seen.push(t.name.to_lowercase());
        let mut ddl = service::create_table_ddl(state, kind, id, &t.name).await.unwrap_or_default();
        if !meta.indexes.is_empty() {
            let list: Vec<String> = meta.indexes.iter().map(|(n, c)| format!("{n}({})", c.join(", "))).collect();
            ddl.push_str(&format!("\n-- existing indexes on {}: {}", t.name, list.join("; ")));
        }
        // MySQL 的 SHOW CREATE TABLE 带 DEFAULT 值、AUTO_INCREMENT 计数等,一并打码
        ddls.push(mask(&ddl));
    }
    let plan = baseline.and_then(|p| p.tree.as_ref()).map(|t| mask(&plan_text(t)));
    let msgs = messages(d.display_name(), &mask(sql), &ddls, plan.as_deref(), &rust_i18n::locale());
    let full = std::sync::Mutex::new(String::new());
    let r = chat::stream_chat(&ep, &msgs, &|e| {
        if let chat::ChatEvent::Done { full: f } = e {
            *full.lock().unwrap() = f;
        }
    })
    .await;
    if let Err(e) = r {
        return AiOutcome { error: Some(e), masked, ..Default::default() };
    }
    let reply = full.into_inner().unwrap();
    match merge(&reply, shape, metas, cands, d) {
        Ok((added, dropped)) => AiOutcome { added, dropped, error: None, masked },
        Err(e) => AiOutcome { error: Some(e), masked, ..Default::default() },
    }
}

#[cfg(test)]
mod tests {
    use super::super::shape::parse;
    use super::*;

    fn metas() -> HashMap<String, TableMeta> {
        let mut m = HashMap::new();
        m.insert(
            "orders".into(),
            TableMeta { columns: vec!["id".into(), "status".into(), "created_at".into()], indexes: vec![("orders_pkey".into(), vec!["id".into()])] },
        );
        m
    }

    #[test]
    fn merges_valid_and_drops_invalid() {
        let shape = parse("SELECT * FROM orders o WHERE o.status = 'x' ORDER BY created_at", Dialect::Pg).unwrap();
        let mut cands = vec![];
        let (added, dropped) = merge(
            r#"```json
            {"indexes":[
              {"table":"public.Orders","columns":["STATUS","created_at desc"],"reason":"filter then sort"},
              {"table":"orders","columns":["nope"],"reason":"x"},
              {"table":"customers","columns":["id"]},
              {"table":"orders","columns":["id"],"reason":"pk"},
              {"table":"orders","columns":["status","created_at DESC"]}
            ]}```"#,
            &shape,
            &metas(),
            &mut cands,
            Dialect::Pg,
        )
        .unwrap();
        assert_eq!(added, 1);
        assert_eq!(cands[0].source, Source::Ai);
        assert_eq!(cands[0].ddl, "CREATE INDEX idx_orders_status_created_at ON orders (status, created_at DESC);");
        assert_eq!(cands[0].reason, "filter then sort");
        assert_eq!(dropped, vec!["orders(nope)", "customers(id)", "orders(id)"]);
        assert!(merge("no json here", &shape, &metas(), &mut cands, Dialect::Pg).is_err());
    }

    #[test]
    fn masks_literals_but_not_identifiers() {
        assert_eq!(mask_literals("where t1.a = 'it''s' and b > 10.5 and c2 = 3"), "where t1.a = '?' and b > ? and c2 = ?");
        let m = messages("PostgreSQL", "select 1", &["CREATE TABLE t (a int)".into()], Some("Seq Scan [t]\n"), "zh-CN");
        assert!(m[0].content.contains("zh-CN") && m[0].content.contains("\"indexes\""));
        assert!(m[1].content.contains("CREATE TABLE t (a int)") && m[1].content.contains("Current plan:\nSeq Scan [t]"));
    }
}
