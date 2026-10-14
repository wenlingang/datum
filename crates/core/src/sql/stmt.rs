//! SQL 文本的语句切分与分类。偏移量一律是 UTF-8 字节偏移,与编辑器光标一致。

use std::ops::Range;

/// 把 SQL 文本按顶层 `;` 切分为语句范围,忽略单引号字符串与 `--` 行注释里的分号。
pub fn split_statements(doc: &str) -> Vec<Range<usize>> {
    let bytes = doc.as_bytes();
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let mut in_str = false;
    while i < bytes.len() {
        let ch = bytes[i];
        if in_str {
            if ch == b'\'' {
                if bytes.get(i + 1) == Some(&b'\'') {
                    i += 2; // 转义的 ''
                    continue;
                }
                in_str = false;
            }
            i += 1;
            continue;
        }
        match ch {
            b'\'' => in_str = true,
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b';' => {
                ranges.push(start..i);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < bytes.len() {
        ranges.push(start..bytes.len());
    }
    ranges
}

/// 切分后去掉空白语句,返回每条语句的文本(已 trim)。
pub fn statements(doc: &str) -> Vec<String> {
    split_statements(doc)
        .into_iter()
        .map(|r| doc[r].trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 判断语句是否为"查询"(产生结果集),据此区分 SELECT 与 DDL/DML。
pub fn is_query(sql: &str) -> bool {
    const HEADS: [&str; 9] = [
        "with", "select", "show", "explain", "desc", "describe", "values", "table", "pragma",
    ];
    let first: String = sql
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect::<String>()
        .to_ascii_lowercase();
    HEADS.contains(&first.as_str())
}

/// 返回光标所在的那条语句(已 trim)。单条时即整段。
pub fn current_statement(doc: &str, caret: usize) -> String {
    let ranges = split_statements(doc);
    for r in &ranges {
        if caret <= r.end {
            return doc[r.clone()].trim().to_string();
        }
    }
    match ranges.last() {
        Some(r) => doc[r.clone()].trim().to_string(),
        None => doc.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_top_level_semicolons() {
        let doc = "select 1; select 'a;b'; -- c;d\nselect 3";
        let parts: Vec<&str> = split_statements(doc)
            .into_iter()
            .map(|r| doc[r].trim())
            .collect();
        assert_eq!(parts, vec!["select 1", "select 'a;b'", "-- c;d\nselect 3"]);
    }

    #[test]
    fn escaped_quote_stays_in_string() {
        let doc = "insert into t values ('it''s; fine'); select 1";
        assert_eq!(statements(doc).len(), 2);
    }

    #[test]
    fn classifies_queries() {
        assert!(is_query("  SELECT * FROM t"));
        assert!(is_query("with x as (select 1) select * from x"));
        assert!(is_query("PRAGMA table_info(t)"));
        assert!(!is_query("update t set a = 1"));
        assert!(!is_query("selected"));
    }

    #[test]
    fn current_statement_follows_caret() {
        let doc = "select 1;\nselect 2;\nselect 3";
        assert_eq!(current_statement(doc, 3), "select 1");
        assert_eq!(current_statement(doc, 12), "select 2");
        assert_eq!(current_statement(doc, doc.len()), "select 3");
    }

    #[test]
    fn multibyte_text_keeps_byte_offsets() {
        let doc = "select '中文;'; select 2";
        assert_eq!(statements(doc), vec!["select '中文;'", "select 2"]);
    }
}
