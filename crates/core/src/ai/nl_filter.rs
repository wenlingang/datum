//! 表浏览的自然语言筛选:一句话(「最近 7 天、状态失败、金额大于 100」)→ 现有的且/或筛选条件。
//!
//! 只让模型输出 JSON,本地严格校验:列名必须存在(大小写不敏感时纠正成真实列名)、操作符在白名单内、
//! 需要值的操作符必须给值;不合格的条件丢弃并说明。结果填进筛选条,用户看得见、改得了。

use crate::ai::chat::{self, ChatMessage};
use crate::ai::provider;
use crate::ai::settings::AiSettingsRepo;
use crate::commands::AppState;
use serde::{Deserialize, Serialize};

/// 筛选条上可用的操作符(与前端 `FilterOp` 一致)。
pub const OPS: &[&str] = &["contains", "eq", "ne", "starts", "ends", "gt", "lt", "gte", "lte", "empty", "notEmpty"];

/// 发给模型的样例行数与单值长度上限(帮模型认出枚举值的写法,如 `failed` / `FAILED`)。
const SAMPLE_ROWS: usize = 5;
const SAMPLE_CHARS: usize = 40;

/// 一列:名称与类型(类型可能未知)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    #[serde(default)]
    pub data_type: Option<String>,
}

/// 一条筛选条件。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Condition {
    pub col: String,
    pub op: String,
    #[serde(default)]
    pub value: String,
}

/// 转换结果。`dropped` 是被丢弃的条件(列不存在、操作符不支持等)的原样描述。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NlFilter {
    pub conj: String,
    pub filters: Vec<Condition>,
    /// 模型的补充说明(如「且 / 或不能混用,已按且处理」),用用户的语言
    pub note: Option<String>,
    pub dropped: Vec<String>,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    conj: Option<String>,
    #[serde(default)]
    filters: Vec<RawCondition>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct RawCondition {
    #[serde(default)]
    col: String,
    #[serde(default)]
    op: String,
    #[serde(default)]
    value: serde_json::Value,
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= SAMPLE_CHARS {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(SAMPLE_CHARS).collect::<String>())
    }
}

/// 拼 prompt:system 讲规则与输出格式,user 带列、样例与问题。`now` 形如 `2026-11-12 21:30 (Thursday)`。
pub fn messages(question: &str, columns: &[ColumnInfo], samples: &[Vec<Option<String>>], now: &str) -> Vec<ChatMessage> {
    let system = format!(
        "You convert a natural-language filter request into filter conditions for a data grid.\n\
         Reply with ONE JSON object and nothing else (no code fence, no prose):\n\
         {{\"conj\":\"and\"|\"or\",\"filters\":[{{\"col\":\"<column>\",\"op\":\"<op>\",\"value\":\"<text>\"}}],\"note\":\"<optional>\"}}\n\
         Rules:\n\
         - col must be one of the given column names, spelled exactly.\n\
         - op is one of: {ops}. contains/starts/ends/eq/ne are case-insensitive text matches; gt/lt/gte/lte compare numbers \
         numerically and other values as text, so write dates and times as ISO text (YYYY-MM-DD or YYYY-MM-DD HH:MM:SS) \
         matching the format seen in the sample rows; empty/notEmpty take no value (use \"\").\n\
         - All conditions share one connective (conj). A range like \"between A and B\" is two conditions joined with and.\n\
         - Resolve relative dates (\"last 7 days\", \"this month\", \"yesterday\") against the current time: {now}.\n\
         - Use the spelling of values seen in the sample rows (e.g. FAILED vs failed) when the user names a category.\n\
         - If part of the request cannot be expressed (mixing and/or, a column that does not exist, computed expressions), \
         keep what can be expressed and explain the rest briefly in note, in the same language as the request. \
         Otherwise omit note.\n\
         - If nothing can be expressed, return an empty filters list with a note.",
        ops = OPS.join(", "),
    );
    let cols: Vec<String> = columns
        .iter()
        .map(|c| match &c.data_type {
            Some(t) if !t.is_empty() => format!("{} ({t})", c.name),
            _ => c.name.clone(),
        })
        .collect();
    let mut user = format!("Columns: {}\n", cols.join(", "));
    if !samples.is_empty() {
        user.push_str("Sample rows (same column order):\n");
        for row in samples.iter().take(SAMPLE_ROWS) {
            let cells: Vec<String> = row.iter().map(|v| v.as_deref().map(truncate).unwrap_or_else(|| "NULL".into())).collect();
            user.push_str(&format!("- {}\n", cells.join(" | ")));
        }
    }
    user.push_str(&format!("Request: {question}"));
    vec![
        ChatMessage { role: "system".into(), content: system },
        ChatMessage { role: "user".into(), content: user },
    ]
}

/// 从回复里取出 JSON 对象:容忍代码块包裹、前后多余文字。
fn json_slice(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    (end > start).then(|| &reply[start..=end])
}

/// 解析并校验模型回复。
pub fn parse(reply: &str, columns: &[ColumnInfo]) -> Result<NlFilter, String> {
    let raw: Raw = json_slice(reply)
        .and_then(|s| serde_json::from_str(s).ok())
        .ok_or_else(|| rust_i18n::t!("ai.err.nl_filter_unreadable").to_string())?;
    let conj = match raw.conj.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("or") => "or",
        _ => "and",
    };
    let mut filters = vec![];
    let mut dropped = vec![];
    for c in raw.filters {
        let value = match c.value {
            serde_json::Value::String(s) => s,
            serde_json::Value::Null => String::new(),
            v => v.to_string(),
        };
        let describe = || format!("{} {} {}", c.col, c.op, value).trim().to_string();
        let Some(col) = columns
            .iter()
            .find(|x| x.name == c.col)
            .or_else(|| columns.iter().find(|x| x.name.eq_ignore_ascii_case(&c.col)))
        else {
            dropped.push(describe());
            continue;
        };
        let Some(op) = OPS.iter().find(|o| o.eq_ignore_ascii_case(&c.op)) else {
            dropped.push(describe());
            continue;
        };
        let needs_value = *op != "empty" && *op != "notEmpty";
        if needs_value && value.trim().is_empty() {
            dropped.push(describe());
            continue;
        }
        filters.push(Condition {
            col: col.name.clone(),
            op: op.to_string(),
            value: if needs_value { value.trim().to_string() } else { String::new() },
        });
    }
    let note = raw.note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    Ok(NlFilter { conj: conj.into(), filters, note, dropped })
}

/// 当前本地时间,给模型解析「最近 7 天」之类的相对日期。
pub fn now_text() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M (%A)").to_string()
}

/// 调用当前配置的模型完成转换(一次性请求,不经过对话面板,也不进对话历史)。
pub async fn translate(
    state: &AppState,
    question: &str,
    columns: &[ColumnInfo],
    samples: &[Vec<Option<String>>],
) -> Result<NlFilter, String> {
    let settings = AiSettingsRepo::new(&state.config_dir).load();
    let ep = provider::resolve(&settings)?;
    let msgs = messages(question, columns, samples, &now_text());
    let full = std::sync::Mutex::new(String::new());
    chat::stream_chat(&ep, &msgs, &|e| {
        if let chat::ChatEvent::Done { full: f } = e {
            *full.lock().unwrap() = f;
        }
    })
    .await?;
    let reply = full.into_inner().unwrap();
    parse(&reply, columns)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<ColumnInfo> {
        ["id", "status", "amount", "created_at", "Note"]
            .iter()
            .map(|n| ColumnInfo { name: n.to_string(), data_type: None })
            .collect()
    }

    #[test]
    fn parses_plain_and_fenced_json() {
        let r = parse(
            "```json\n{\"conj\":\"and\",\"filters\":[{\"col\":\"status\",\"op\":\"eq\",\"value\":\"failed\"},\
             {\"col\":\"amount\",\"op\":\"gt\",\"value\":100},{\"col\":\"created_at\",\"op\":\"gte\",\"value\":\"2026-11-05\"}]}\n```",
            &cols(),
        )
        .unwrap();
        assert_eq!(r.conj, "and");
        assert_eq!(r.filters.len(), 3);
        assert_eq!(r.filters[1], Condition { col: "amount".into(), op: "gt".into(), value: "100".into() });
        assert!(r.dropped.is_empty() && r.note.is_none());
    }

    #[test]
    fn fixes_case_and_drops_invalid_conditions() {
        let r = parse(
            r#"Sure! {"conj":"OR","filters":[
                {"col":"note","op":"NOTEMPTY","value":"ignored"},
                {"col":"missing","op":"eq","value":"x"},
                {"col":"id","op":"between","value":"1,5"},
                {"col":"status","op":"eq","value":"  "}
            ],"note":"  "}"#,
            &cols(),
        )
        .unwrap();
        assert_eq!(r.conj, "or");
        assert_eq!(r.filters, vec![Condition { col: "Note".into(), op: "notEmpty".into(), value: String::new() }]);
        assert_eq!(r.dropped.len(), 3);
        assert!(r.note.is_none(), "空白说明不显示");
    }

    #[test]
    fn unreadable_reply_is_an_error() {
        assert!(parse("I cannot do that", &cols()).is_err());
        assert!(parse("{not json}", &cols()).is_err());
    }

    #[test]
    fn prompt_carries_columns_samples_and_time() {
        let mut c = cols();
        c[2].data_type = Some("numeric".into());
        let long = "x".repeat(100);
        let m = messages("最近 7 天失败的", &c, &[vec![Some("1".into()), None, Some(long)]], "2026-11-12 21:30 (Thursday)");
        assert_eq!(m.len(), 2);
        assert!(m[0].content.contains("2026-11-12 21:30"));
        assert!(m[0].content.contains("notEmpty"));
        assert!(m[1].content.contains("amount (numeric)"));
        assert!(m[1].content.contains("1 | NULL | "));
        assert!(m[1].content.contains('…') && !m[1].content.contains(&"x".repeat(41)));
        assert!(m[1].content.ends_with("Request: 最近 7 天失败的"));
    }
}
