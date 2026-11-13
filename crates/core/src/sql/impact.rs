//! 写操作的影响范围预估:把 UPDATE / DELETE、TRUNCATE、DROP TABLE 改写成只读的
//! `SELECT COUNT(*)` 与预览查询(纯函数)。
//!
//! - 单表:`SELECT * FROM 表 WHERE …`
//! - PG / SQLite 的 `UPDATE … FROM`、PG 的 `DELETE … USING`:`SELECT * FROM 目标表 WHERE EXISTS
//!   (SELECT 1 FROM 其余表 WHERE 条件)`——目标表的每一行至多算一次,与数据库的实际行为一致
//! - MySQL 的多表 UPDATE / DELETE(JOIN 或逗号):只改一张表时 `SELECT DISTINCT 目标.* FROM 联表 WHERE …`
//!   (有主键时精确;没有主键时完全相同的行只算一次)
//! - `WITH … UPDATE / DELETE`:CTE 原样带到预估查询前面;CTE 里有写操作(PG 的数据修改 CTE)不预估
//!
//! 其余(一次改多张表、存储过程等)返回 None,界面显示「无法预估」,不去猜。改写结果由调用方在
//! 数据库强制只读的会话里执行,即使改写有偏差也写不进数据。
//!
//! 统计的是「条件匹配的行数」:MySQL 的 UPDATE 返回的受影响行数只算值真正变了的行,可能更少。

use super::Dialect;

/// 预览查询每条最多带回的行数。
pub const PREVIEW_ROWS: u32 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpactQuery {
    /// 返回一行一列:匹配的行数
    pub count_sql: String,
    /// 匹配行的前若干行(`SELECT *`)
    pub rows_sql: String,
    /// 匹配的全部行(`SELECT *`,不限行数;撤销前抓快照用)
    pub snapshot_sql: String,
    /// 整表操作(TRUNCATE / DROP TABLE):数的是表里现有的行
    pub whole_table: bool,
    /// 目标表,照语句原文(可带 schema 与引号,不含别名)
    pub table: String,
    pub op: WriteOp,
}

/// 写语句的种类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOp {
    Delete,
    /// SET 里赋值的列(去掉别名前缀与引号);解析不了(如 PG 的 `(a, b) = (…)`)为 None
    Update { set_columns: Option<Vec<String>> },
    /// TRUNCATE / DROP TABLE
    WholeTable,
}

/// 去掉一段标识符的引号(`"x"`、`` `x` ``、`[x]`;成对的引号还原)。
pub fn unquote(ident: &str) -> String {
    let t = ident.trim();
    let inner = |open: char, close: char| {
        t.strip_prefix(open)
            .and_then(|r| r.strip_suffix(close))
            .map(|r| r.replace(&format!("{close}{close}"), &close.to_string()))
    };
    inner('"', '"').or_else(|| inner('`', '`')).or_else(|| inner('[', ']')).unwrap_or_else(|| t.to_string())
}

/// 读一个(可带 schema 前缀、各段可加引号的)标识符,返回 (原文, 剩余部分)。
fn read_qualified(text: &str) -> Option<(&str, &str)> {
    let t = text.trim_start();
    let start = text.len() - t.len();
    let b = t.as_bytes();
    let mut i = 0;
    loop {
        match b.get(i)? {
            q @ (b'"' | b'`') => {
                i += 1;
                loop {
                    if *b.get(i)? == *q {
                        if b.get(i + 1) == Some(q) {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'[' => i += t[i..].find(']')? + 1,
            c if c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$' || *c >= 0x80 => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || b[i] >= 0x80) {
                    i += 1;
                }
            }
            _ => return None,
        }
        if b.get(i) == Some(&b'.') {
            i += 1;
            continue;
        }
        return Some((&text[start..start + i], &t[i..]));
    }
}

/// `FROM` 部分(`ONLY schema.t AS a`)→ 表名原文。
fn table_of(from: &str) -> Option<String> {
    let mut t = from.trim_start();
    if t.len() >= 5 && t[..5].eq_ignore_ascii_case("only ") {
        t = &t[5..];
    }
    let (name, _) = read_qualified(t)?;
    Some(name.trim().trim_end_matches('*').trim().to_string())
}

/// SET 段 → 赋值的列名;遇到元组赋值等解析不了的写法返回 None。
fn set_columns(set: &str, commas: &[usize], base: usize) -> Option<Vec<String>> {
    let mut cuts: Vec<usize> = commas.iter().filter(|&&c| c >= base).map(|c| c - base).filter(|&c| c < set.len()).collect();
    cuts.push(set.len());
    let mut out = Vec::new();
    let mut from = 0;
    for cut in cuts {
        let piece = &set[from..cut];
        from = cut + 1;
        let (name, rest) = read_qualified(piece)?;
        if !rest.trim_start().starts_with('=') {
            return None;
        }
        let last = name.rsplit('.').next().unwrap_or(name);
        out.push(unquote(last));
    }
    (!out.is_empty()).then_some(out)
}

/// 顶层(不在括号、字符串、注释、引号标识符里)的一个单词。
#[derive(Debug, Clone)]
struct Word {
    start: usize,
    end: usize,
    upper: String,
}

/// 顶层扫描结果:单词与逗号位置。
struct Scan {
    words: Vec<Word>,
    commas: Vec<usize>,
}

/// 按方言扫描语句的顶层结构。不认识的写法不会误判为顶层关键字:
/// 字符串、注释、引号标识符、括号里的内容一律跳过。
fn scan(sql: &str, d: Dialect) -> Option<Scan> {
    let b = sql.as_bytes();
    let mut words = Vec::new();
    let mut commas = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    let mysql = d == Dialect::Mysql;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\'' | b'"' | b'`' => {
                // MySQL 的单双引号字符串支持反斜杠转义;引号成对出现表示转义
                let q = c;
                i += 1;
                loop {
                    if i >= b.len() {
                        return None;
                    }
                    if mysql && b[i] == b'\\' && q != b'`' {
                        i += 2;
                        continue;
                    }
                    if b[i] == q {
                        if b.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            b'[' if d == Dialect::Sqlite => {
                while i < b.len() && b[i] != b']' {
                    i += 1;
                }
                i += 1;
                continue;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'#' if mysql => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                // MySQL 的 /*! … */ 会执行:改写不了,放弃
                if mysql && b.get(i + 2) == Some(&b'!') {
                    return None;
                }
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            b'$' if d == Dialect::Pg => {
                // $tag$ … $tag$ 字符串
                let tag_end = sql[i + 1..].find('$').map(|p| i + 1 + p)?;
                let tag = &sql[i..=tag_end];
                if tag[1..tag.len() - 1].chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
                    let close = sql[tag_end + 1..].find(tag)?;
                    i = tag_end + 1 + close + tag.len();
                    continue;
                }
                i += 1;
                continue;
            }
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => commas.push(i),
            _ if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$') {
                    i += 1;
                }
                if depth == 0 {
                    words.push(Word {
                        start,
                        end: i,
                        upper: sql[start..i].to_ascii_uppercase(),
                    });
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    (depth == 0).then_some(Scan { words, commas })
}

impl Scan {
    fn find(&self, from: usize, any: &[&str]) -> Option<&Word> {
        self.words.iter().find(|w| w.start >= from && any.contains(&w.upper.as_str()))
    }
    fn has_comma_between(&self, a: usize, b: usize) -> bool {
        self.commas.iter().any(|&c| c >= a && c < b)
    }
    fn has_word_between(&self, a: usize, b: usize, any: &[&str]) -> bool {
        self.words
            .iter()
            .any(|w| w.start >= a && w.end <= b && any.contains(&w.upper.as_str()))
    }
}

const JOINS: &[&str] = &["JOIN", "STRAIGHT_JOIN"];

/// 表引用里紧跟在表名后、不是别名的关键字。
const NOT_ALIAS: &[&str] = &[
    "ON", "USING", "JOIN", "STRAIGHT_JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "NATURAL", "OUTER", "WHERE",
    "SET", "USE", "FORCE", "IGNORE", "PARTITION",
];

/// 在 MySQL 的表引用(`a JOIN b ON … , c AS x`)里找别名或表名为 `target` 的表,返回表名原文。
fn resolve_target(refs: &str, target: &str, d: Dialect) -> Option<String> {
    let s = scan(refs, d)?;
    // 表名出现的位置:开头、每个顶层逗号后、每个 JOIN 后
    let mut starts = vec![0];
    starts.extend(s.commas.iter().map(|c| c + 1));
    starts.extend(s.words.iter().filter(|w| JOINS.contains(&w.upper.as_str())).map(|w| w.end));
    let want = unquote(target);
    let mut found = None;
    for at in starts {
        let rest = &refs[at..];
        if rest.trim_start().starts_with('(') {
            continue; // 子查询 / 括号里的连接:不当目标
        }
        let Some((name, after)) = read_qualified(rest) else { continue };
        let name = name.trim();
        let mut alias = None;
        let mut tail = after.trim_start();
        if tail.len() >= 3 && tail[..3].eq_ignore_ascii_case("as ") {
            tail = &tail[3..];
        }
        if let Some((a, _)) = read_qualified(tail) {
            if !NOT_ALIAS.contains(&a.trim().to_ascii_uppercase().as_str()) && !a.contains('.') {
                alias = Some(unquote(a));
            }
        }
        let last = unquote(name.rsplit('.').next().unwrap_or(name));
        let hit = match &alias {
            Some(a) => a.eq_ignore_ascii_case(&want),
            None => last.eq_ignore_ascii_case(&want),
        };
        if hit {
            if found.is_some() {
                return None; // 同名出现两次:说不清是哪张
            }
            found = Some(name.to_string());
        }
    }
    found
}

/// SET 段里赋值的列都带同一个限定名(`a.x = …, a.y = …`)时返回它;否则(不带、混用)为 None。
fn set_qualifier(set: &str, commas: &[usize], base: usize) -> Option<String> {
    let mut cuts: Vec<usize> = commas.iter().filter(|&&c| c >= base).map(|c| c - base).filter(|&c| c < set.len()).collect();
    cuts.push(set.len());
    let mut from = 0;
    let mut q: Option<String> = None;
    for cut in cuts {
        let (name, _) = read_qualified(&set[from..cut])?;
        from = cut + 1;
        let (qual, _) = name.trim().rsplit_once('.')?;
        let qual = unquote(qual.rsplit('.').next().unwrap_or(qual));
        match &q {
            Some(prev) if !prev.eq_ignore_ascii_case(&qual) => return None,
            _ => q = Some(qual),
        }
    }
    q
}

/// `SELECT * FROM 目标 WHERE EXISTS (SELECT 1 FROM 其余 WHERE 条件)`:PG / SQLite 的多表写法。
fn exists_queries(target: &str, others: &str, cond: &str, op: WriteOp) -> Option<ImpactQuery> {
    let cond = cond.trim();
    let filter = if cond.is_empty() { String::new() } else { format!("\nWHERE {cond}\n") };
    let tail = format!("WHERE EXISTS (SELECT 1 FROM {}{filter}\n)", others.trim());
    queries(target, &tail, op)
}

/// `SELECT DISTINCT 目标.* FROM 联表 WHERE …`:MySQL 的多表写法。`qual` 是 SET / DELETE 里写的目标(别名或表名)。
fn distinct_queries(refs: &str, qual: &str, tail: &str, op: WriteOp, d: Dialect) -> Option<ImpactQuery> {
    let table = resolve_target(refs, qual, d)?;
    let inner = format!("SELECT DISTINCT {}.* FROM {}\n{}\n", qual.trim(), refs.trim(), tail.trim());
    Some(ImpactQuery {
        count_sql: format!("SELECT COUNT(*) FROM ({inner}) AS datum_impact"),
        rows_sql: format!("SELECT * FROM ({inner}) AS datum_impact LIMIT {PREVIEW_ROWS}"),
        snapshot_sql: inner,
        whole_table: false,
        table,
        op,
    })
}

/// 生成预估查询:`from` 为表(可带别名),`tail` 为 WHERE / ORDER BY / LIMIT 部分。
/// 各段之间一律换行:原文里的 `-- 注释` 只能注释到行尾,不能把后面拼上的 WHERE 或右括号吞掉。
fn queries(from: &str, tail: &str, op: WriteOp) -> Option<ImpactQuery> {
    let inner = format!("SELECT * FROM {}\n{}\n", from.trim(), tail.trim());
    Some(ImpactQuery {
        count_sql: format!("SELECT COUNT(*) FROM ({inner}) AS datum_impact"),
        rows_sql: format!("SELECT * FROM ({inner}) AS datum_impact LIMIT {PREVIEW_ROWS}"),
        snapshot_sql: inner,
        whole_table: op == WriteOp::WholeTable,
        table: table_of(from)?,
        op,
    })
}

/// 改写一条写语句;改写不了返回 None。
pub fn impact_query(stmt: &str, d: Dialect) -> Option<ImpactQuery> {
    let sql = stmt.trim().trim_end_matches(';').trim_end();
    let s = scan(sql, d)?;
    let first = s.words.first()?;
    match first.upper.as_str() {
        "WITH" => {
            // CTE:原样带到预估查询前面。CTE 里有写操作(PG 的数据修改 CTE)就不预估
            let body = s.find(first.end, &["UPDATE", "DELETE", "INSERT", "SELECT", "MERGE", "REPLACE"])?;
            if !matches!(body.upper.as_str(), "UPDATE" | "DELETE") {
                return None;
            }
            let cte = &sql[..body.start];
            let upper = cte.to_ascii_uppercase();
            let writes = upper
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|w| matches!(w, "INSERT" | "UPDATE" | "DELETE" | "MERGE"));
            if writes {
                return None;
            }
            let q = impact_query(&sql[body.start..], d)?;
            let pre = |x: &str| format!("{}\n{x}", cte.trim_end());
            Some(ImpactQuery {
                count_sql: pre(&q.count_sql),
                rows_sql: pre(&q.rows_sql),
                snapshot_sql: pre(&q.snapshot_sql),
                ..q
            })
        }
        "DELETE" => {
            let from_kw = s.words.get(1..)?.iter().find(|w| {
                !matches!(w.upper.as_str(), "LOW_PRIORITY" | "QUICK" | "IGNORE")
            })?;
            if from_kw.upper != "FROM" {
                // MySQL 多表 DELETE:`DELETE t1 FROM 联表 WHERE …`(只删一张表时才预估)
                if d != Dialect::Mysql {
                    return None;
                }
                let from = s.find(from_kw.start, &["FROM"])?;
                let targets = &sql[from_kw.start..from.start];
                if s.has_comma_between(from_kw.start, from.start) {
                    return None;
                }
                let qual = targets.trim().trim_end_matches(".*").trim();
                let tail = s.find(from.end, &["WHERE"]).map(|w| w.start).unwrap_or(sql.len());
                return distinct_queries(&sql[from.end..tail], qual, &sql[tail..], WriteOp::Delete, d);
            }
            let table_start = from_kw.end;
            let tail = s.find(table_start, &["WHERE", "ORDER", "LIMIT", "RETURNING", "USING"]);
            let tail_start = tail.map(|w| w.start).unwrap_or(sql.len());
            if let Some(using) = tail.filter(|w| w.upper == "USING") {
                let target = &sql[table_start..using.start];
                if s.has_comma_between(table_start, using.start) {
                    return None;
                }
                let where_kw = s.find(using.end, &["WHERE"]);
                let list_end = where_kw.map(|w| w.start).unwrap_or(sql.len());
                if d == Dialect::Mysql {
                    // MySQL:`DELETE FROM t1 USING 联表 WHERE …`
                    let qual = target.trim().trim_end_matches(".*").trim();
                    return distinct_queries(&sql[using.end..list_end], qual, &sql[list_end..], WriteOp::Delete, d);
                }
                // PG:`DELETE FROM t USING 其余表 WHERE …`
                let end = s.find(list_end, &["RETURNING"]).map(|w| w.start).unwrap_or(sql.len());
                let cond = where_kw.map(|w| &sql[w.end..end]).unwrap_or("");
                let list_end = s.find(using.end, &["WHERE", "RETURNING"]).map(|w| w.start).unwrap_or(sql.len());
                return exists_queries(target, &sql[using.end..list_end], cond, WriteOp::Delete);
            }
            if s.has_comma_between(table_start, tail_start) || s.has_word_between(table_start, tail_start, JOINS) {
                return None;
            }
            let end = s.find(tail_start, &["RETURNING"]).map(|w| w.start).unwrap_or(sql.len());
            queries(&sql[table_start..tail_start], &sql[tail_start..end], WriteOp::Delete)
        }
        "UPDATE" => {
            // 跳过修饰词:MySQL 的 LOW_PRIORITY / IGNORE,SQLite 的 OR REPLACE 等
            let mut idx = 1;
            while let Some(w) = s.words.get(idx) {
                match w.upper.as_str() {
                    "LOW_PRIORITY" | "IGNORE" => idx += 1,
                    "OR" => idx += 2,
                    _ => break,
                }
            }
            let table_start = match idx {
                1 => first.end,
                _ => s.words.get(idx - 1)?.end,
            };
            let set = s.find(table_start, &["SET"])?;
            let tail = s.find(set.end, &["WHERE", "ORDER", "LIMIT", "RETURNING", "FROM"]);
            let tail_start = tail.map(|w| w.start).unwrap_or(sql.len());
            let set_cols = set_columns(&sql[set.end..tail_start], &s.commas, set.end);
            let op = WriteOp::Update { set_columns: set_cols };
            if s.has_comma_between(table_start, set.start) || s.has_word_between(table_start, set.start, JOINS) {
                // MySQL 多表 UPDATE:SET 里的列都属于同一张表时才预估
                if d != Dialect::Mysql {
                    return None;
                }
                let qual = set_qualifier(&sql[set.end..tail_start], &s.commas, set.end)?;
                return distinct_queries(&sql[table_start..set.start], &qual, &sql[tail_start..], op, d);
            }
            if let Some(from) = tail.filter(|w| w.upper == "FROM") {
                // PG / SQLite 的 UPDATE … FROM
                if d == Dialect::Mysql {
                    return None;
                }
                let where_kw = s.find(from.end, &["WHERE"]);
                let list_end = s.find(from.end, &["WHERE", "RETURNING"]).map(|w| w.start).unwrap_or(sql.len());
                let end = s.find(list_end, &["RETURNING"]).map(|w| w.start).unwrap_or(sql.len());
                let cond = where_kw.map(|w| &sql[w.end..end]).unwrap_or("");
                let set_cols = set_columns(&sql[set.end..from.start], &s.commas, set.end);
                return exists_queries(
                    &sql[table_start..set.start],
                    &sql[from.end..list_end],
                    cond,
                    WriteOp::Update { set_columns: set_cols },
                );
            }
            let end = s.find(tail_start, &["RETURNING"]).map(|w| w.start).unwrap_or(sql.len());
            queries(&sql[table_start..set.start], &sql[tail_start..end], op)
        }
        "TRUNCATE" | "DROP" => {
            let mut idx = 1;
            if first.upper == "DROP" {
                if s.words.get(1)?.upper != "TABLE" {
                    return None;
                }
                idx = 2;
            }
            // TRUNCATE [TABLE] [ONLY]、DROP TABLE [IF EXISTS]
            let mut table_start = s.words.get(idx - 1)?.end;
            for kw in ["TABLE", "ONLY", "IF", "EXISTS"] {
                if let Some(w) = s.words.get(idx).filter(|w| w.upper == kw) {
                    table_start = w.end;
                    idx += 1;
                }
            }
            let stop = s
                .find(table_start, &["CASCADE", "RESTRICT", "RESTART", "CONTINUE"])
                .map(|w| w.start)
                .unwrap_or(sql.len());
            if s.has_comma_between(table_start, stop) || sql[table_start..stop].trim().is_empty() {
                return None;
            }
            queries(&sql[table_start..stop], "", WriteOp::WholeTable)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Dialect::*;

    fn squash(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ").replace(" )", ")")
    }

    fn q(sql: &str, d: Dialect) -> Option<(String, String)> {
        impact_query(sql, d).map(|q| (squash(&q.count_sql), squash(&q.rows_sql)))
    }

    fn count(sql: &str, d: Dialect) -> String {
        squash(&impact_query(sql, d).unwrap().count_sql)
    }

    #[test]
    fn delete_with_and_without_where() {
        assert_eq!(
            count("DELETE FROM orders WHERE status = 'cancelled';", Pg),
            "SELECT COUNT(*) FROM (SELECT * FROM orders WHERE status = 'cancelled') AS datum_impact"
        );
        assert_eq!(count("delete from orders", Pg), "SELECT COUNT(*) FROM (SELECT * FROM orders) AS datum_impact");
        let (_, rows) = q("DELETE FROM t WHERE id > 5", Sqlite).unwrap();
        assert_eq!(rows, "SELECT * FROM (SELECT * FROM t WHERE id > 5) AS datum_impact LIMIT 10");
    }

    #[test]
    fn update_keeps_where_and_drops_set_and_returning() {
        assert_eq!(
            count("UPDATE users SET name = 'x', age = (SELECT 1) WHERE id IN (1, 2) RETURNING *", Pg),
            "SELECT COUNT(*) FROM (SELECT * FROM users WHERE id IN (1, 2)) AS datum_impact"
        );
        assert_eq!(
            count("UPDATE `shop`.`items` AS i SET i.qty = 0", Mysql),
            "SELECT COUNT(*) FROM (SELECT * FROM `shop`.`items` AS i) AS datum_impact"
        );
        assert_eq!(
            count("UPDATE OR REPLACE t SET a = 1 WHERE b = 2", Sqlite),
            "SELECT COUNT(*) FROM (SELECT * FROM t WHERE b = 2) AS datum_impact"
        );
        // MySQL 的 ORDER BY … LIMIT 一并带上
        assert_eq!(
            count("UPDATE LOW_PRIORITY t SET a = 1 WHERE b = 2 ORDER BY id LIMIT 10", Mysql),
            "SELECT COUNT(*) FROM (SELECT * FROM t WHERE b = 2 ORDER BY id LIMIT 10) AS datum_impact"
        );
    }

    #[test]
    fn keywords_inside_strings_comments_and_parens_are_ignored() {
        assert_eq!(
            count("UPDATE t SET note = 'where set from' WHERE id = 1", Pg),
            "SELECT COUNT(*) FROM (SELECT * FROM t WHERE id = 1) AS datum_impact"
        );
        // 表名后的行尾注释:WHERE 必须另起一行,不能被注释吞掉
        let c = impact_query("DELETE FROM t -- where x\n WHERE id = (SELECT max(id) FROM t2 WHERE y = 1)", Pg)
            .unwrap()
            .count_sql;
        assert!(c.contains("t -- where x\nWHERE id = (SELECT max(id)"), "{c}");
        // 条件末尾的行尾注释:右括号也必须另起一行
        let c = impact_query("DELETE FROM t WHERE id = 1 -- only one", Pg).unwrap().count_sql;
        assert!(c.ends_with("-- only one\n) AS datum_impact"), "{c}");
        assert_eq!(
            count("UPDATE t SET body = $x$ where ; from $x$ WHERE id = 1", Pg),
            "SELECT COUNT(*) FROM (SELECT * FROM t WHERE id = 1) AS datum_impact"
        );
        assert_eq!(
            count(r"UPDATE t SET s = 'it\'s where' WHERE id = 1", Mysql),
            "SELECT COUNT(*) FROM (SELECT * FROM t WHERE id = 1) AS datum_impact"
        );
    }

    #[test]
    fn multi_table_forms() {
        // PG / SQLite:UPDATE … FROM、DELETE … USING 改成 EXISTS,目标表每行至多算一次
        assert_eq!(
            count("UPDATE t AS a SET x = s.x FROM s WHERE a.id = s.id RETURNING a.*", Pg),
            "SELECT COUNT(*) FROM (SELECT * FROM t AS a WHERE EXISTS (SELECT 1 FROM s WHERE a.id = s.id)) AS datum_impact"
        );
        assert_eq!(
            count("UPDATE t SET x = 1 FROM s JOIN u ON u.id = s.uid", Sqlite),
            "SELECT COUNT(*) FROM (SELECT * FROM t WHERE EXISTS (SELECT 1 FROM s JOIN u ON u.id = s.uid)) AS datum_impact"
        );
        let del = impact_query("DELETE FROM ONLY t USING s, u WHERE t.id = s.id AND s.k = u.k", Pg).unwrap();
        assert_eq!(
            squash(&del.count_sql),
            "SELECT COUNT(*) FROM (SELECT * FROM ONLY t WHERE EXISTS (SELECT 1 FROM s, u WHERE t.id = s.id AND s.k = u.k)) AS datum_impact"
        );
        assert_eq!(del.table, "t");
        let up = impact_query("UPDATE t SET a = s.a, b = 2 FROM s WHERE t.id = s.id", Pg).unwrap();
        assert_eq!(up.op, WriteOp::Update { set_columns: Some(vec!["a".into(), "b".into()]) });

        // MySQL:只改一张表的多表写法 → DISTINCT 目标.*
        let m = impact_query("UPDATE orders o JOIN users u ON u.id = o.uid SET o.flag = 1 WHERE u.vip = 1", Mysql).unwrap();
        assert_eq!(
            squash(&m.count_sql),
            "SELECT COUNT(*) FROM (SELECT DISTINCT o.* FROM orders o JOIN users u ON u.id = o.uid WHERE u.vip = 1) AS datum_impact"
        );
        assert_eq!(m.table, "orders");
        assert_eq!(m.op, WriteOp::Update { set_columns: Some(vec!["flag".into()]) });
        assert_eq!(
            count("UPDATE t1, t2 SET t1.a = t2.a WHERE t1.id = t2.id", Mysql),
            "SELECT COUNT(*) FROM (SELECT DISTINCT t1.* FROM t1, t2 WHERE t1.id = t2.id) AS datum_impact"
        );
        let d1 = impact_query("DELETE t1 FROM t1 JOIN t2 ON t1.id = t2.id WHERE t2.gone", Mysql).unwrap();
        assert_eq!(
            squash(&d1.count_sql),
            "SELECT COUNT(*) FROM (SELECT DISTINCT t1.* FROM t1 JOIN t2 ON t1.id = t2.id WHERE t2.gone) AS datum_impact"
        );
        assert_eq!(d1.table, "t1");
        let d2 = impact_query("DELETE FROM a.* USING `shop`.`orders` AS a LEFT JOIN b ON b.id = a.bid WHERE b.id IS NULL", Mysql).unwrap();
        assert_eq!(d2.table, "`shop`.`orders`");

        // 一次改多张表、说不清改哪张:不预估
        assert_eq!(q("UPDATE t1 JOIN t2 ON t1.id = t2.id SET t1.a = 1, t2.b = 2", Mysql), None);
        assert_eq!(q("UPDATE t1 JOIN t2 ON t1.id = t2.id SET a = 1", Mysql), None);
        assert_eq!(q("DELETE t1, t2 FROM t1 JOIN t2 ON t1.id = t2.id", Mysql), None);
        assert_eq!(q("DELETE x FROM t1 JOIN t2 ON t1.id = t2.id", Mysql), None, "目标不在联表里");
        assert_eq!(q("UPDATE t SET a = 1 FROM s", Mysql), None);
        assert_eq!(q("INSERT INTO t VALUES (1)", Pg), None);
        assert_eq!(q("DELETE FROM t WHERE x = 'unterminated", Pg), None);
        assert_eq!(q("DELETE FROM t WHERE x = 1 /*! OR 1=1 */", Mysql), None);
    }

    #[test]
    fn cte_prefix() {
        let c = impact_query("WITH old AS (SELECT id FROM t WHERE ts < now()) DELETE FROM t WHERE id IN (SELECT id FROM old)", Pg).unwrap();
        assert_eq!(
            squash(&c.count_sql),
            "WITH old AS (SELECT id FROM t WHERE ts < now()) SELECT COUNT(*) FROM (SELECT * FROM t WHERE id IN (SELECT id FROM old)) AS datum_impact"
        );
        assert!(squash(&c.snapshot_sql).starts_with("WITH old AS"));
        assert!(impact_query("WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 3) UPDATE t SET a = 1 WHERE id IN (SELECT n FROM r)", Mysql).is_some());
        // 数据修改 CTE、主体不是 UPDATE / DELETE:不预估
        assert_eq!(q("WITH d AS (DELETE FROM t RETURNING id) DELETE FROM u WHERE id IN (SELECT id FROM d)", Pg), None);
        assert_eq!(q("WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x", Pg), None);
        assert_eq!(q("WITH x AS (SELECT 1) SELECT * FROM x", Pg), None);
    }

    #[test]
    fn truncate_and_drop_table_count_existing_rows() {
        let q1 = impact_query("TRUNCATE TABLE ONLY public.logs RESTART IDENTITY CASCADE", Pg).unwrap();
        assert!(q1.whole_table);
        assert_eq!(squash(&q1.count_sql), "SELECT COUNT(*) FROM (SELECT * FROM public.logs) AS datum_impact");
        assert_eq!(
            count("DROP TABLE IF EXISTS \"Order Items\"", Pg),
            "SELECT COUNT(*) FROM (SELECT * FROM \"Order Items\") AS datum_impact"
        );
        assert_eq!(q("TRUNCATE a, b", Pg), None);
        assert_eq!(q("DROP TABLE a, b", Mysql), None);
        assert_eq!(q("DROP INDEX idx", Pg), None);
    }

    #[test]
    fn table_and_set_columns_are_parsed() {
        let q = impact_query("UPDATE public.\"Orders\" AS o SET o.status = 'x', \"Total\" = total * 2 WHERE id = 1", Pg).unwrap();
        assert_eq!(q.table, "public.\"Orders\"");
        assert_eq!(q.op, WriteOp::Update { set_columns: Some(vec!["status".into(), "Total".into()]) });
        assert_eq!(squash(&q.snapshot_sql), "SELECT * FROM public.\"Orders\" AS o WHERE id = 1");
        // 值里的逗号不算列分隔
        let q = impact_query("UPDATE t SET a = concat('x,y', b), c = 1", Mysql).unwrap();
        assert_eq!(q.op, WriteOp::Update { set_columns: Some(vec!["a".into(), "c".into()]) });
        // 元组赋值解析不了
        let q = impact_query("UPDATE t SET (a, b) = (1, 2) WHERE id = 1", Pg).unwrap();
        assert_eq!(q.op, WriteOp::Update { set_columns: None });
        let q = impact_query("DELETE FROM `shop`.`items` i WHERE i.id > 3", Mysql).unwrap();
        assert_eq!((q.table.as_str(), &q.op), ("`shop`.`items`", &WriteOp::Delete));
        assert_eq!(impact_query("TRUNCATE ONLY logs", Pg).unwrap().op, WriteOp::WholeTable);
        assert_eq!(unquote("\"a\"\"b\""), "a\"b");
        assert_eq!(unquote("[x y]"), "x y");
    }
}
