//! AI 回复的分段模型:普通文本与 SQL 代码块交替。

use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    Sql(String),
}

/// 形似 SQL 的裸代码块也按 SQL 处理(AI 偶尔忘记标注语言)。
static SQL_LIKE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(with|select|insert|update|delete|create|drop|alter|truncate|explain|show|pragma|grant|revoke|begin|comment)\b").unwrap()
});

/// 把 assistant 回复切成文本段与 SQL 段。支持 ```sql 标注块与形似 SQL 的无标注块;
/// 其余语言的代码块保留为文本段。流式渲染时未闭合的结尾代码块同样生效。
pub fn parse_segments(content: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut rest = content;
    loop {
        let Some(open) = rest.find("```") else {
            push_text(&mut segments, rest);
            break;
        };
        let after_ticks = &rest[open + 3..];
        let Some(nl) = after_ticks.find('\n') else {
            push_text(&mut segments, rest);
            break;
        };
        let lang = after_ticks[..nl].trim();
        if lang.contains('`') {
            push_text(&mut segments, rest);
            break;
        }
        push_text(&mut segments, &rest[..open]);
        let body_start = &after_ticks[nl + 1..];
        let (body, consumed_to) = match body_start.find("```") {
            Some(close) => (&body_start[..close], open + 3 + nl + 1 + close + 3),
            None => (body_start, rest.len()),
        };
        let lang = lang.to_lowercase();
        let body_trimmed = body.trim_end();
        if lang == "sql" || (lang.is_empty() && SQL_LIKE.is_match(body_trimmed)) {
            if !body_trimmed.trim().is_empty() {
                segments.push(Segment::Sql(body_trimmed.trim().to_string()));
            }
        } else {
            push_text(&mut segments, &rest[open..consumed_to]);
        }
        rest = &rest[consumed_to..];
    }
    segments
}

fn push_text(segments: &mut Vec<Segment>, text: &str) {
    let trimmed = text.trim_matches('\n');
    if trimmed.is_empty() {
        return;
    }
    if let Some(Segment::Text(prev)) = segments.last_mut() {
        prev.push('\n');
        prev.push_str(trimmed);
    } else {
        segments.push(Segment::Text(trimmed.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_text_and_sql_blocks() {
        let s = parse_segments(
            "先建表:\n```sql\nCREATE TABLE t (id int);\n```\n然后查询\n```\nselect 1\n```",
        );
        assert_eq!(
            s,
            vec![
                Segment::Text("先建表:".into()),
                Segment::Sql("CREATE TABLE t (id int);".into()),
                Segment::Text("然后查询".into()),
                Segment::Sql("select 1".into()),
            ]
        );
    }

    #[test]
    fn other_languages_stay_text_and_unclosed_block_streams() {
        let s = parse_segments("```json\n{}\n```\n```sql\nselect");
        assert_eq!(
            s,
            vec![
                Segment::Text("```json\n{}\n```".into()),
                Segment::Sql("select".into())
            ]
        );
    }
}
