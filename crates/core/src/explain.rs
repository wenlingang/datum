//! 执行计划:把三种库的 EXPLAIN 输出解析成同一棵树,供界面画成树形并高亮成本最高的节点。
//!
//! - PostgreSQL:`EXPLAIN (FORMAT JSON)`,可选 ANALYZE(只对只读语句开放,在只读会话里限时执行)
//! - MySQL 8.0.16+:`EXPLAIN FORMAT=TREE` / `EXPLAIN ANALYZE`(缩进文本树);更老的版本退回表格
//! - SQLite:`EXPLAIN QUERY PLAN`(按 parent 组树,没有成本)

use crate::core::connection::DbKind;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::pg::client::QueryResult;
use crate::service;
use crate::AppState;
use regex::Regex;
use rust_i18n::t;
use serde::Serialize;
use std::sync::LazyLock;

/// 计划树的一个节点。
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlanNode {
    /// 节点类型:Seq Scan、Hash Join、Table scan on c……
    pub label: String,
    /// 对象与条件:表、索引、过滤条件等(可能为空)
    pub detail: String,
    /// 预估总成本(含子节点);SQLite 没有
    pub cost: Option<f64>,
    /// 自身成本 = 总成本 - 子节点总成本(不小于 0)
    pub self_cost: Option<f64>,
    /// 预估行数
    pub rows: Option<f64>,
    /// 实际耗时(毫秒,仅 ANALYZE)
    pub actual_ms: Option<f64>,
    /// 实际行数(仅 ANALYZE)
    pub actual_rows: Option<f64>,
    /// 全表扫描(没走索引)
    pub full_scan: bool,
    pub children: Vec<PlanNode>,
}

/// 一次 Explain 的结果:计划树(解析不了为 None)与原始输出(表格形态)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub tree: Option<PlanNode>,
    pub raw: QueryResult,
    pub analyzed: bool,
    /// 总耗时(毫秒,仅 ANALYZE 且库给出时)
    pub total_ms: Option<f64>,
}

/// 计算自身成本:总成本减去子节点总成本。
fn fill_self_cost(n: &mut PlanNode) {
    for c in &mut n.children {
        fill_self_cost(c);
    }
    if let Some(total) = n.cost {
        let kids: f64 = n.children.iter().filter_map(|c| c.cost).sum();
        n.self_cost = Some((total - kids).max(0.0));
    }
}

// ---------- PostgreSQL ----------

fn pg_node(v: &serde_json::Value) -> PlanNode {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let f = |k: &str| v.get(k).and_then(|x| x.as_f64());
    let node_type = s("Node Type");
    let mut detail = vec![];
    if !s("Relation Name").is_empty() {
        let alias = s("Alias");
        let rel = s("Relation Name");
        detail.push(if alias.is_empty() || alias == rel { rel } else { format!("{rel} {alias}") });
    }
    if !s("Index Name").is_empty() {
        detail.push(format!("index {}", s("Index Name")));
    }
    for k in ["Join Type", "Index Cond", "Hash Cond", "Merge Cond", "Filter", "Sort Key", "Group Key"] {
        match v.get(k) {
            Some(serde_json::Value::String(x)) if !x.is_empty() => detail.push(format!("{k}: {x}")),
            Some(serde_json::Value::Array(a)) if !a.is_empty() => {
                let items: Vec<String> = a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
                detail.push(format!("{k}: {}", items.join(", ")));
            }
            _ => {}
        }
    }
    let loops = f("Actual Loops").unwrap_or(1.0).max(1.0);
    PlanNode {
        full_scan: node_type == "Seq Scan",
        label: node_type,
        detail: detail.join(" · "),
        cost: f("Total Cost"),
        self_cost: None,
        rows: f("Plan Rows"),
        // 实际时间是每次循环的平均值,乘以循环次数才是总耗时
        actual_ms: f("Actual Total Time").map(|x| x * loops),
        actual_rows: f("Actual Rows").map(|x| x * loops),
        children: v.get("Plans").and_then(|p| p.as_array()).map(|a| a.iter().map(pg_node).collect()).unwrap_or_default(),
    }
}

/// 解析 `EXPLAIN (FORMAT JSON)` 的输出(一行一列的 JSON 数组)。返回 (树, 执行总耗时)。
pub fn parse_pg_json(text: &str) -> Option<(PlanNode, Option<f64>)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let top = v.as_array()?.first()?;
    let mut root = pg_node(top.get("Plan")?);
    fill_self_cost(&mut root);
    Some((root, top.get("Execution Time").and_then(|x| x.as_f64())))
}

// ---------- MySQL ----------

/// 数字:整数 / 小数 / 科学计数法(不吞掉 `..` 分隔符)
const NUM: &str = r"(\d+(?:\.\d+)?(?:e[+-]?\d+)?)";
static MY_COST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"\(cost={NUM}(?:\.\.{NUM})?\s+rows={NUM}\)")).unwrap());
static MY_ACTUAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"\(actual time={NUM}\.\.{NUM}\s+rows={NUM}\s+loops={NUM}\)")).unwrap());

/// 解析 `EXPLAIN FORMAT=TREE` / `EXPLAIN ANALYZE` 的缩进文本树。
pub fn parse_mysql_tree(text: &str) -> Option<PlanNode> {
    // (缩进, 节点);每行 `-> 描述  (cost=… rows=…) (actual time=…)`
    let mut stack: Vec<(usize, PlanNode)> = vec![];
    let mut roots: Vec<PlanNode> = vec![];
    for line in text.lines() {
        let Some(pos) = line.find("-> ") else { continue };
        let indent = pos;
        let body = &line[pos + 3..];
        let cost = MY_COST.captures(body);
        let actual = MY_ACTUAL.captures(body);
        let mut desc = body.to_string();
        for m in [cost.as_ref().and_then(|c| c.get(0)), actual.as_ref().and_then(|c| c.get(0))].into_iter().flatten() {
            desc = desc.replace(m.as_str(), "");
        }
        let desc = desc.trim().to_string();
        let num = |c: &Option<regex::Captures>, i: usize| c.as_ref().and_then(|c| c.get(i)).and_then(|m| m.as_str().parse::<f64>().ok());
        let (label, detail) = match desc.split_once(" on ") {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (desc.clone(), String::new()),
        };
        let lower = label.to_ascii_lowercase();
        let loops = num(&actual, 4).unwrap_or(1.0).max(1.0);
        let node = PlanNode {
            full_scan: lower.starts_with("table scan") && !detail.starts_with('<'),
            label,
            detail,
            // 有的版本只给一个成本数,有的给 启动..总
            cost: num(&cost, 2).or_else(|| num(&cost, 1)),
            self_cost: None,
            rows: num(&cost, 3),
            actual_ms: num(&actual, 2).map(|x| x * loops),
            actual_rows: num(&actual, 3).map(|x| x * loops),
            children: vec![],
        };
        // 弹出缩进不小于当前行的节点,挂到各自的父节点上
        while let Some((d, _)) = stack.last() {
            if *d < indent {
                break;
            }
            let (_, done) = stack.pop().unwrap();
            match stack.last_mut() {
                Some((_, parent)) => parent.children.push(done),
                None => roots.push(done),
            }
        }
        stack.push((indent, node));
    }
    while let Some((_, done)) = stack.pop() {
        match stack.last_mut() {
            Some((_, parent)) => parent.children.push(done),
            None => roots.push(done),
        }
    }
    let mut root = match roots.len() {
        0 => return None,
        1 => roots.pop().unwrap(),
        _ => PlanNode { label: "Query".into(), children: roots, ..Default::default() },
    };
    fill_self_cost(&mut root);
    Some(root)
}

// ---------- SQLite ----------

/// 解析 `EXPLAIN QUERY PLAN` 的行(id, parent, notused, detail)。
pub fn parse_sqlite_rows(rows: &[Vec<Option<String>>]) -> Option<PlanNode> {
    let items: Vec<(i64, i64, String)> = rows
        .iter()
        .filter_map(|r| {
            let id = r.first()?.as_deref()?.parse().ok()?;
            let parent = r.get(1)?.as_deref()?.parse().ok()?;
            Some((id, parent, r.get(3)?.clone().unwrap_or_default()))
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    fn build(parent: i64, items: &[(i64, i64, String)]) -> Vec<PlanNode> {
        items
            .iter()
            .filter(|(_, p, _)| *p == parent)
            .map(|(id, _, detail)| {
                let upper = detail.to_ascii_uppercase();
                let (label, rest) = detail.split_once(' ').unwrap_or((detail.as_str(), ""));
                PlanNode {
                    // SCAN 表(没有 USING INDEX)是全表扫描;SCAN 子查询 / CTE 不算
                    full_scan: upper.starts_with("SCAN ") && !upper.contains("USING") && !upper.contains("SUBQUERY") && !upper.contains("CONSTANT"),
                    label: label.to_string(),
                    detail: rest.to_string(),
                    children: build(*id, items),
                    ..Default::default()
                }
            })
            .collect()
    }
    let mut roots = build(0, &items);
    Some(if roots.len() == 1 { roots.pop().unwrap() } else { PlanNode { label: "QUERY PLAN".into(), children: roots, ..Default::default() } })
}

/// ANALYZE 的限时(毫秒)。
const ANALYZE_TIMEOUT_MS: u32 = 30_000;

/// 执行 Explain。`analyze` 会真正执行语句:只接受只读语句,并在只读会话里限时执行。
pub async fn explain(state: &AppState, kind: &DbKind, id: &str, sql: &str, analyze: bool) -> AppResult<Plan> {
    let sql = sql.trim().trim_end_matches(';');
    if analyze && !crate::sql::risk::is_read_only(sql) {
        return Err(AppError::new(ErrorKind::Query, t!("core.err.analyze_read_only")));
    }
    let run = |q: String| async move {
        if analyze {
            service::query_read_only_timeout(state, kind, id, &q, 10_000, ANALYZE_TIMEOUT_MS).await
        } else {
            service::query(state, kind, id, &q, Some(10_000)).await
        }
    };
    match kind {
        DbKind::Pg => {
            let opts = if analyze { "(ANALYZE, BUFFERS, FORMAT JSON)" } else { "(FORMAT JSON)" };
            let raw = run(format!("EXPLAIN {opts} {sql}")).await?;
            let text = raw.rows.first().and_then(|r| r.first().cloned().flatten()).unwrap_or_default();
            let parsed = parse_pg_json(&text);
            // 原始输出给文本版(人看、交给 AI 都更友好)
            let text_raw = if analyze { raw.clone() } else { run(format!("EXPLAIN {sql}")).await.unwrap_or(raw.clone()) };
            Ok(Plan {
                total_ms: parsed.as_ref().and_then(|(_, ms)| *ms),
                tree: parsed.map(|(t, _)| t),
                raw: text_raw,
                analyzed: analyze,
            })
        }
        DbKind::Mysql => {
            let q = if analyze { format!("EXPLAIN ANALYZE {sql}") } else { format!("EXPLAIN FORMAT=TREE {sql}") };
            match run(q).await {
                Ok(raw) => {
                    let text: String = raw.rows.iter().filter_map(|r| r.first().cloned().flatten()).collect::<Vec<_>>().join("\n");
                    let tree = parse_mysql_tree(&text);
                    let total_ms = tree.as_ref().and_then(|t| t.actual_ms);
                    Ok(Plan { tree, raw, analyzed: analyze, total_ms })
                }
                // 老版本不认 FORMAT=TREE:退回表格形态
                Err(_) if !analyze => {
                    let raw = run(format!("EXPLAIN {sql}")).await?;
                    Ok(Plan { tree: None, raw, analyzed: false, total_ms: None })
                }
                Err(e) => Err(e),
            }
        }
        DbKind::Sqlite => {
            if analyze {
                return Err(AppError::new(ErrorKind::Query, t!("core.err.analyze_unsupported")));
            }
            let raw = run(format!("EXPLAIN QUERY PLAN {sql}")).await?;
            Ok(Plan { tree: parse_sqlite_rows(&raw.rows), raw, analyzed: false, total_ms: None })
        }
        DbKind::Redis | DbKind::Mongo => Err(AppError::new(ErrorKind::Query, t!("core.err.analyze_unsupported"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_json_tree_with_self_cost_and_analyze() {
        let json = r#"[{"Plan": {"Node Type": "Hash Join", "Join Type": "Inner", "Total Cost": 120.5, "Plan Rows": 50,
            "Hash Cond": "(o.customer_id = c.id)", "Actual Total Time": 3.2, "Actual Rows": 48, "Actual Loops": 1,
            "Plans": [
              {"Node Type": "Seq Scan", "Relation Name": "orders", "Alias": "o", "Total Cost": 90.0, "Plan Rows": 5000,
               "Filter": "(status = 'paid'::text)", "Actual Total Time": 2.0, "Actual Rows": 1250, "Actual Loops": 1},
              {"Node Type": "Hash", "Total Cost": 20.0, "Plan Rows": 50,
               "Plans": [{"Node Type": "Index Scan", "Relation Name": "customers", "Alias": "c", "Index Name": "customers_pkey",
                          "Total Cost": 18.0, "Plan Rows": 50, "Actual Total Time": 0.05, "Actual Rows": 1, "Actual Loops": 50}]}
            ]}, "Planning Time": 0.2, "Execution Time": 3.5}]"#;
        let (root, total) = parse_pg_json(json).unwrap();
        assert_eq!(total, Some(3.5));
        assert_eq!(root.label, "Hash Join");
        assert_eq!(root.self_cost, Some(10.5));
        assert!(root.detail.contains("Hash Cond: (o.customer_id = c.id)"));
        let scan = &root.children[0];
        assert!(scan.full_scan);
        assert_eq!(scan.detail, "orders o · Filter: (status = 'paid'::text)");
        let idx = &root.children[1].children[0];
        assert!(!idx.full_scan);
        assert_eq!(idx.detail, "customers c · index customers_pkey");
        // 每次循环的平均值 × 循环次数
        assert_eq!(idx.actual_ms, Some(2.5));
        assert_eq!(idx.actual_rows, Some(50.0));
        assert!(parse_pg_json("not json").is_none());
    }

    #[test]
    fn mysql_tree_text() {
        let text = "-> Sort: orders DESC\n    -> Table scan on <temporary>\n        -> Aggregate using temporary table\n            -> Nested loop inner join  (cost=3.25 rows=5) (actual time=0.1..0.4 rows=5 loops=1)\n                -> Table scan on c  (cost=0.75 rows=5) (actual time=0.05..0.08 rows=5 loops=1)\n                -> Index lookup on o using fk_customer (customer_id=c.id)  (cost=0.35 rows=1) (actual time=0.02..0.03 rows=1 loops=5)";
        let root = parse_mysql_tree(text).unwrap();
        assert_eq!(root.label, "Sort: orders DESC");
        let join = &root.children[0].children[0].children[0];
        assert_eq!(join.label, "Nested loop inner join");
        assert_eq!(join.cost, Some(3.25));
        assert_eq!(join.children.len(), 2);
        let scan = &join.children[0];
        assert_eq!((scan.label.as_str(), scan.detail.as_str(), scan.full_scan), ("Table scan", "c", true));
        let lookup = &join.children[1];
        assert_eq!(lookup.label, "Index lookup");
        assert!(lookup.detail.starts_with("o using fk_customer"));
        assert_eq!(lookup.actual_ms, Some(0.15));
        // 临时表的扫描不算全表扫描
        assert!(!root.children[0].full_scan);
        assert_eq!(join.self_cost, Some(2.15));
        // 新版本的成本写法 cost=启动..总
        let n = parse_mysql_tree("-> Filter: (x > 1)  (cost=0.1..2.5 rows=3)").unwrap();
        assert_eq!(n.cost, Some(2.5));
        assert!(parse_mysql_tree("id | select_type").is_none());
    }

    #[test]
    fn sqlite_rows_tree() {
        let r = |id: &str, p: &str, d: &str| vec![Some(id.to_string()), Some(p.to_string()), Some("0".to_string()), Some(d.to_string())];
        let rows = vec![
            r("2", "0", "SCAN orders"),
            r("4", "0", "SEARCH customers USING INTEGER PRIMARY KEY (rowid=?)"),
            r("7", "0", "USE TEMP B-TREE FOR ORDER BY"),
        ];
        let root = parse_sqlite_rows(&rows).unwrap();
        assert_eq!(root.label, "QUERY PLAN");
        assert_eq!(root.children.len(), 3);
        assert!(root.children[0].full_scan);
        assert_eq!(root.children[0].detail, "orders");
        assert!(!root.children[1].full_scan);
        let nested = parse_sqlite_rows(&[r("1", "0", "CO-ROUTINE sub"), r("3", "1", "SCAN t")]).unwrap();
        assert_eq!(nested.children[0].label, "SCAN");
    }
}
