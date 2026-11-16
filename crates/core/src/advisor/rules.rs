//! 按规则从语句形态生成候选索引(纯函数)。
//!
//! 每张表:等值条件的列在前,接一个范围列;没有范围列(或范围列正好是排序的第一列)时接 ORDER BY 的列,
//! 让索引顺带免掉排序;再不然接 GROUP BY。JOIN 的连接列另成一个候选(内表按连接列查找)。
//! 已有索引的前缀能覆盖的候选丢掉(等值列之间顺序无关),同一张表里被另一个候选前缀覆盖的也丢掉。

use super::shape::{Block, ColRef, Pred, Shape, TableRef};
use super::{Candidate, IndexCol, Source};
use crate::sql::Dialect;
use rust_i18n::t;
use std::collections::HashMap;

/// 最多给几个候选。
pub const MAX_CANDIDATES: usize = 5;
/// 一个候选最多几列。
const MAX_COLUMNS: usize = 5;

/// 一张表的结构(规则只需要列名与已有索引的列序)。
#[derive(Debug, Clone, Default)]
pub struct TableMeta {
    pub columns: Vec<String>,
    /// 已有索引:(索引名, 有序列名)
    pub indexes: Vec<(String, Vec<String>)>,
}

/// 一张表在一个查询块里的用法。
#[derive(Debug, Default)]
struct Usage {
    eq: Vec<String>,
    range: Vec<String>,
    join: Vec<String>,
    /// ORDER BY 全部落在这张表上时的列(带方向)
    order: Option<Vec<(String, bool)>>,
    group: Vec<String>,
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.iter().any(|x| x.eq_ignore_ascii_case(&s)) {
        v.push(s);
    }
}

/// 块里的表(下标)按别名 / 表名找。
struct Scope<'a> {
    tables: Vec<(&'a TableRef, Option<&'a TableMeta>)>,
    /// 派生表、CTE 等非真实表的名字
    opaque: Vec<String>,
}

impl Scope<'_> {
    /// 列 → (表下标, 真实列名)。表没有结构信息、列不存在、没限定且多张表都有时返回 None。
    fn resolve(&self, c: &ColRef) -> Option<(usize, String)> {
        let real = |i: usize| -> Option<(usize, String)> {
            let meta = self.tables[i].1?;
            let name = meta.columns.iter().find(|x| **x == c.name).or_else(|| meta.columns.iter().find(|x| x.eq_ignore_ascii_case(&c.name)))?;
            Some((i, name.clone()))
        };
        match &c.qual {
            Some(q) => {
                if self.opaque.iter().any(|o| o.eq_ignore_ascii_case(q)) {
                    return None;
                }
                let i = self.tables.iter().position(|(t, _)| t.alias.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(q)))
                    .or_else(|| self.tables.iter().position(|(t, _)| t.alias.is_none() && t.name.eq_ignore_ascii_case(q)))?;
                real(i)
            }
            None => {
                let hits: Vec<_> = (0..self.tables.len()).filter_map(real).collect();
                // 派生表也可能有这一列:只有一张真实表、没有派生表时才认没限定的列
                if hits.len() == 1 && (self.opaque.is_empty() || self.tables.len() > 1) {
                    hits.into_iter().next()
                } else {
                    None
                }
            }
        }
    }
}

/// 生成规则候选。`metas` 的键是表名(小写)。
pub fn candidates(shape: &Shape, metas: &HashMap<String, TableMeta>, d: Dialect) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = vec![];
    for block in &shape.blocks {
        for c in block_candidates(block, shape, metas, d) {
            if out.len() >= MAX_CANDIDATES {
                return out;
            }
            if !out.iter().any(|x| x.table.eq_ignore_ascii_case(&c.table) && covers(&x.cols(), &c.cols(), 0)) {
                out.push(c);
            }
        }
    }
    out
}

pub(crate) fn meta_for<'a>(metas: &'a HashMap<String, TableMeta>, t: &TableRef) -> Option<&'a TableMeta> {
    metas.get(&t.name.to_lowercase())
}

fn block_candidates(block: &Block, shape: &Shape, metas: &HashMap<String, TableMeta>, d: Dialect) -> Vec<Candidate> {
    let is_cte = |t: &TableRef| t.schema.is_none() && shape.ctes.iter().any(|c| c.eq_ignore_ascii_case(&t.name));
    let mut opaque = block.derived.clone();
    let mut tables = vec![];
    for t in &block.tables {
        if is_cte(t) {
            opaque.push(t.alias.clone().unwrap_or_else(|| t.name.clone()));
        } else {
            tables.push((t, meta_for(metas, t)));
        }
    }
    let scope = Scope { tables, opaque };
    let mut usage: Vec<Usage> = scope.tables.iter().map(|_| Usage::default()).collect();
    for p in &block.preds {
        match p {
            Pred::Eq(c) => {
                if let Some((i, col)) = scope.resolve(c) {
                    push_unique(&mut usage[i].eq, col);
                }
            }
            Pred::Range(c) => {
                if let Some((i, col)) = scope.resolve(c) {
                    push_unique(&mut usage[i].range, col);
                }
            }
            Pred::Join(a, b) => {
                if let (Some((i, ca)), Some((j, cb))) = (scope.resolve(a), scope.resolve(b)) {
                    if i != j {
                        push_unique(&mut usage[i].join, ca);
                        push_unique(&mut usage[j].join, cb);
                    }
                } else if let Some((i, col)) = scope.resolve(a).or_else(|| scope.resolve(b)) {
                    // 另一边是派生表 / CTE:这一边仍按连接列查找
                    push_unique(&mut usage[i].join, col);
                }
            }
        }
    }
    if let Some(order) = &block.order {
        let resolved: Option<Vec<(usize, String, bool)>> = order.iter().map(|o| scope.resolve(&o.col).map(|(i, c)| (i, c, o.desc))).collect();
        if let Some(r) = resolved {
            if let Some(first) = r.first().map(|x| x.0) {
                if r.iter().all(|x| x.0 == first) {
                    let mut cols: Vec<(String, bool)> = vec![];
                    for (_, c, desc) in r {
                        if !cols.iter().any(|x| x.0.eq_ignore_ascii_case(&c)) {
                            cols.push((c, desc));
                        }
                    }
                    usage[first].order = Some(cols);
                }
            }
        }
    }
    let group: Option<Vec<(usize, String)>> = block.group.iter().map(|g| scope.resolve(g)).collect();
    if let Some(g) = group {
        if let Some(first) = g.first().map(|x| x.0) {
            if g.iter().all(|x| x.0 == first) {
                for (_, c) in g {
                    push_unique(&mut usage[first].group, c);
                }
            }
        }
    }

    let mut out = vec![];
    for (i, u) in usage.iter().enumerate() {
        let (t, meta) = scope.tables[i];
        let Some(meta) = meta else { continue };
        for (mut cols, reason, eq_len) in table_candidates(u, meta) {
            cols.truncate(MAX_COLUMNS);
            if cols.is_empty() {
                continue;
            }
            let plain: Vec<String> = cols.iter().map(|c| c.name.clone()).collect();
            if covered_by_existing(meta, &plain, eq_len.min(plain.len())) {
                continue;
            }
            if out.iter().any(|x: &Candidate| covers(&x.cols(), &plain, 0)) {
                continue;
            }
            out.push(Candidate::new(t, cols, reason, Source::Rule, d));
        }
    }
    out
}

/// 一张表的候选:(列, 理由, 前几列是等值列)。
fn table_candidates(u: &Usage, meta: &TableMeta) -> Vec<(Vec<IndexCol>, String, usize)> {
    let mut out = vec![];
    let col = |n: &str| IndexCol { name: n.to_string(), desc: false };
    let mut base: Vec<IndexCol> = u.eq.iter().map(|c| col(c)).collect();
    let eq_part = |cols: &[String]| t!("core.advisor.reason.eq", cols = cols.join(", ")).to_string();
    let mut reasons: Vec<String> = vec![];
    if !u.eq.is_empty() {
        reasons.push(eq_part(&u.eq));
    }
    let order_cols = |order: &[(String, bool)]| -> Vec<IndexCol> {
        // 方向一致时不写 DESC(索引可以倒着扫);方向混合时照写
        let mixed = order.iter().any(|x| x.1 != order[0].1);
        order.iter().map(|(n, d)| IndexCol { name: n.clone(), desc: mixed && *d }).collect()
    };
    let order_names = |order: &[(String, bool)]| order.iter().map(|x| x.0.clone()).collect::<Vec<_>>().join(", ");
    let extend = |base: &mut Vec<IndexCol>, more: Vec<IndexCol>| {
        for c in more {
            if !base.iter().any(|x| x.name.eq_ignore_ascii_case(&c.name)) {
                base.push(c);
            }
        }
    };
    if let Some(range) = u.range.first() {
        let mut cols = base.clone();
        let mut why = reasons.clone();
        match &u.order {
            Some(order) if order[0].0.eq_ignore_ascii_case(range) => {
                extend(&mut cols, order_cols(order));
                why.push(t!("core.advisor.reason.range", cols = range).to_string());
                why.push(t!("core.advisor.reason.order", cols = order_names(order)).to_string());
            }
            _ => {
                extend(&mut cols, vec![col(range)]);
                why.push(t!("core.advisor.reason.range", cols = range).to_string());
            }
        }
        out.push((cols, why.join("; "), u.eq.len()));
        // 范围列与排序不兼容:另给一个「等值 + 排序」的候选,免掉排序(验证时比较两者)
        if let Some(order) = &u.order {
            if !order[0].0.eq_ignore_ascii_case(range) {
                let mut cols = base.clone();
                extend(&mut cols, order_cols(order));
                let mut why = reasons.clone();
                why.push(t!("core.advisor.reason.order", cols = order_names(order)).to_string());
                out.push((cols, why.join("; "), u.eq.len()));
            }
        }
    } else if let Some(order) = &u.order {
        extend(&mut base, order_cols(order));
        let mut why = reasons.clone();
        why.push(t!("core.advisor.reason.order", cols = order_names(order)).to_string());
        out.push((base.clone(), why.join("; "), u.eq.len()));
    } else if !u.group.is_empty() {
        extend(&mut base, u.group.iter().map(|c| col(c)).collect());
        let mut why = reasons.clone();
        why.push(t!("core.advisor.reason.group", cols = u.group.join(", ")).to_string());
        out.push((base.clone(), why.join("; "), u.eq.len()));
    } else if !base.is_empty() {
        out.push((base.clone(), reasons.join("; "), u.eq.len()));
    }
    // 连接列已有索引(多半是主键)时不再给连接候选:再加等值列也省不了什么
    if !u.join.is_empty() && !covered_by_existing(meta, &u.join, u.join.len()) {
        let mut cols: Vec<IndexCol> = u.join.iter().map(|c| col(c)).collect();
        extend(&mut cols, u.eq.iter().map(|c| col(c)).collect());
        let mut why = vec![t!("core.advisor.reason.join", cols = u.join.join(", ")).to_string()];
        if !u.eq.is_empty() {
            why.push(eq_part(&u.eq));
        }
        // 连接列与等值列都是等值匹配,彼此顺序无关
        let n = cols.len();
        out.push((cols, why.join("; "), n));
    }
    out
}

/// `index` 的前缀能否覆盖 `cand`:逐列相同;`cand` 前 `eq_len` 列是等值列,彼此顺序无关。
pub(crate) fn covers(index: &[String], cand: &[String], eq_len: usize) -> bool {
    if index.len() < cand.len() {
        return false;
    }
    let lower = |v: &[String]| v.iter().map(|s| s.to_lowercase()).collect::<Vec<_>>();
    let (idx, c) = (lower(&index[..cand.len()]), lower(cand));
    if idx == c {
        return true;
    }
    if eq_len == 0 {
        return false;
    }
    let mut a = idx[..eq_len].to_vec();
    let mut b = c[..eq_len].to_vec();
    a.sort();
    b.sort();
    a == b && idx[eq_len..] == c[eq_len..]
}

pub(crate) fn covered_by_existing(meta: &TableMeta, cand: &[String], eq_len: usize) -> bool {
    meta.indexes.iter().any(|(_, cols)| covers(cols, cand, eq_len))
}

#[cfg(test)]
mod tests {
    use super::super::shape::parse;
    use super::*;

    fn meta(cols: &[&str], indexes: &[(&str, &[&str])]) -> TableMeta {
        TableMeta {
            columns: cols.iter().map(|s| s.to_string()).collect(),
            indexes: indexes.iter().map(|(n, c)| (n.to_string(), c.iter().map(|s| s.to_string()).collect())).collect(),
        }
    }

    fn metas() -> HashMap<String, TableMeta> {
        let mut m = HashMap::new();
        m.insert("orders".into(), meta(&["id", "customer_id", "status", "created_at", "total"], &[("orders_pkey", &["id"])]));
        m.insert("customers".into(), meta(&["id", "name", "country", "email"], &[("customers_pkey", &["id"])]));
        m.insert("order items".into(), meta(&["id", "Order Id", "Sku"], &[]));
        m
    }

    fn run(sql: &str, d: Dialect) -> Vec<String> {
        candidates(&parse(sql, d).unwrap(), &metas(), d).iter().map(|c| format!("{}({})", c.table, c.cols().join(","))).collect()
    }

    #[test]
    fn equality_then_range() {
        assert_eq!(
            run("SELECT * FROM orders WHERE created_at > '2026-01-01' AND status = 'paid'", Dialect::Pg),
            vec!["orders(status,created_at)"]
        );
    }

    #[test]
    fn order_by_appended_when_compatible() {
        assert_eq!(run("SELECT * FROM orders WHERE status = 'paid' ORDER BY created_at DESC LIMIT 20", Dialect::Pg), vec!["orders(status,created_at)"]);
        // 范围列就是排序第一列:一个索引同时满足过滤与排序
        assert_eq!(
            run("SELECT * FROM orders WHERE customer_id = 1 AND created_at > '2026-01-01' ORDER BY created_at", Dialect::Pg),
            vec!["orders(customer_id,created_at)"]
        );
        // 不兼容:两个候选,交给验证比较
        assert_eq!(
            run("SELECT * FROM orders WHERE customer_id = 1 AND total > 100 ORDER BY created_at", Dialect::Pg),
            vec!["orders(customer_id,total)", "orders(customer_id,created_at)"]
        );
        // 方向混合时写出 DESC
        let c = candidates(&parse("SELECT * FROM orders ORDER BY status, created_at DESC", Dialect::Pg).unwrap(), &metas(), Dialect::Pg);
        assert_eq!(c[0].ddl, "CREATE INDEX idx_orders_status_created_at ON orders (status, created_at DESC);");
    }

    #[test]
    fn skips_columns_already_covered_by_index_prefix() {
        let mut m = metas();
        m.get_mut("orders").unwrap().indexes.push(("idx_s_c".into(), vec!["created_at".into(), "status".into(), "total".into()]));
        let shape = parse("SELECT * FROM orders WHERE status = 'paid' AND created_at = '2026-01-01'", Dialect::Pg).unwrap();
        assert!(candidates(&shape, &m, Dialect::Pg).is_empty(), "等值列顺序无关,已被 (created_at, status, …) 覆盖");
        // 主键已覆盖
        assert!(run("SELECT * FROM orders WHERE id = 5", Dialect::Pg).is_empty());
        // 范围列不在前缀里:不算覆盖
        let shape = parse("SELECT * FROM orders WHERE status = 'paid' AND total > 3", Dialect::Pg).unwrap();
        assert_eq!(candidates(&shape, &m, Dialect::Pg).len(), 1);
    }

    #[test]
    fn joins_aliases_and_unqualified_columns() {
        let got = run(
            "SELECT c.name, o.total FROM customers c JOIN orders o ON o.customer_id = c.id WHERE country = 'KE' AND o.status = 'paid'",
            Dialect::Pg,
        );
        // customers.id 是主键,不再建;orders 的过滤与连接各一个
        assert_eq!(got, vec!["customers(country)", "orders(status)", "orders(customer_id,status)"]);
    }

    #[test]
    fn quoting_names_and_ddl() {
        let c = candidates(
            &parse(r#"SELECT * FROM "Order Items" oi WHERE oi."Order Id" = 3 AND "Sku" = 'x'"#, Dialect::Pg).unwrap(),
            &metas(),
            Dialect::Pg,
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].table, "Order Items");
        assert_eq!(c[0].ddl, r#"CREATE INDEX idx_order_items_order_id_sku ON "Order Items" ("Order Id", "Sku");"#);
        assert_eq!(c[0].editor_sql, r#"CREATE INDEX CONCURRENTLY idx_order_items_order_id_sku ON "Order Items" ("Order Id", "Sku");"#);
        let m = candidates(&parse("SELECT * FROM orders WHERE status = ?", Dialect::Mysql).unwrap(), &metas(), Dialect::Mysql);
        assert_eq!(m[0].editor_sql, "ALTER TABLE orders ADD INDEX idx_orders_status (status), ALGORITHM=INPLACE, LOCK=NONE;");
        let s = candidates(&parse("SELECT * FROM orders WHERE STATUS = 'x'", Dialect::Sqlite).unwrap(), &metas(), Dialect::Sqlite);
        assert_eq!(s[0].ddl, "CREATE INDEX idx_orders_status ON orders (status);", "列名按表结构纠正大小写");
    }

    #[test]
    fn group_by_ctes_unknown_columns_and_cap() {
        assert_eq!(run("SELECT status, count(*) FROM orders GROUP BY status", Dialect::Pg), vec!["orders(status)"]);
        // CTE 名不是表;不存在的列丢掉
        assert_eq!(
            run("WITH x AS (SELECT * FROM orders WHERE status = 'a') SELECT * FROM x WHERE x.total > 1 AND nope = 2", Dialect::Pg),
            vec!["orders(status)"]
        );
        let many = run(
            "SELECT * FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.status = 'a' AND o.total > 1 AND c.country = 'x' AND c.email > 'a' ORDER BY o.created_at",
            Dialect::Pg,
        );
        assert!(many.len() <= MAX_CANDIDATES, "{many:?}");
    }

    #[test]
    fn cover_rules() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(covers(&s(&["a", "b", "c"]), &s(&["a", "b"]), 0));
        assert!(!covers(&s(&["a", "b"]), &s(&["b", "a"]), 0));
        assert!(covers(&s(&["a", "b"]), &s(&["b", "a"]), 2));
        assert!(!covers(&s(&["a", "b", "c"]), &s(&["b", "a", "d"]), 2));
        assert!(!covers(&s(&["a"]), &s(&["a", "b"]), 1));
    }
}
