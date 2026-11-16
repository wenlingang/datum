//! 「让 AI 推断各列」:只把建表语句与列的结构发给模型(不发任何行数据),
//! 模型回一段 JSON 给出每列的生成器;本地严格校验(列存在、不是自增 / 生成列、
//! 生成器认得且适合列的类型、参数合法并收进类型范围),不合格的丢弃并列出。

use super::gen::{parse_date, Generator};
use super::infer::{clamp, generator_for_kind, KINDS};
use super::meta::{ColumnMeta, TableMeta};
use super::plan::ColumnPlan;
use crate::ai::chat::{self, ChatMessage};
use crate::ai::provider;
use crate::ai::settings::AiSettingsRepo;
use crate::commands::AppState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiSuggestion {
    pub columns: Vec<ColumnPlan>,
    /// 被丢弃的建议(列不存在、生成器不认识或不适合等)的原样描述
    pub dropped: Vec<String>,
    pub note: Option<String>,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    columns: Vec<RawColumn>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawColumn {
    name: String,
    generator: String,
    min: Option<f64>,
    max: Option<f64>,
    from: Option<String>,
    to: Option<String>,
    values: Option<Vec<serde_json::Value>>,
    value: Option<serde_json::Value>,
    min_len: Option<u32>,
    max_len: Option<u32>,
    len: Option<u32>,
    null_ratio: Option<f64>,
}

fn describe(col: &ColumnMeta) -> String {
    let mut flags = vec![];
    if !col.nullable {
        flags.push("NOT NULL".to_string());
    }
    if col.unique {
        flags.push("UNIQUE".into());
    }
    if col.auto.is_some() {
        flags.push("AUTO (skip)".into());
    }
    if col.fk.is_some() {
        flags.push("FOREIGN KEY (use foreignKey)".into());
    }
    if !col.values.is_empty() {
        flags.push(format!("allowed values: {}", col.values.join(" | ")));
    }
    format!("- {} {} {}", col.name, col.data_type, flags.join(", ")).trim_end().to_string()
}

/// 拼 prompt:system 讲生成器与输出格式,user 带建表语句与列清单(只有结构,没有数据)。
pub fn messages(meta: &TableMeta, ddl: &str, today: &str) -> Vec<ChatMessage> {
    let system = format!(
        "You choose fake-data generators for the columns of one database table.\n\
         Reply with ONE JSON object and nothing else (no code fence, no prose):\n\
         {{\"columns\":[{{\"name\":\"<column>\",\"generator\":\"<kind>\", ...params}}],\"note\":\"<optional>\"}}\n\
         Generator kinds: {kinds}.\n\
         Params (all optional): integer/decimal/float take \"min\" and \"max\" numbers; date/timestamp take \"from\" and \"to\" \
         as YYYY-MM-DD; pickList takes \"values\" (array of strings); fixed takes \"value\"; text takes \"minLen\"/\"maxLen\"; \
         bytes takes \"len\"; any nullable column may take \"nullRatio\" between 0 and 1.\n\
         Rules:\n\
         - Use the exact column names given. Skip columns marked AUTO. Use foreignKey for foreign-key columns.\n\
         - Pick what a realistic row would contain judging by the column name, type, comments and the table's purpose; \
         keep numbers inside the type's precision and realistic ranges (e.g. prices 1-500, ages 18-90).\n\
         - Prefer pickList with realistic values for status/type/category-like text columns.\n\
         - Today is {today}; recent timestamps should end today.\n\
         - Write note only if something needs the user's attention, in the language of the column comments if any, else English.",
        kinds = KINDS.join(", "),
    );
    let cols: Vec<String> = meta.columns.iter().map(describe).collect();
    let user = format!("Table: {}\nDDL:\n{}\n\nColumns:\n{}", meta.table, ddl.trim(), cols.join("\n"));
    vec![
        ChatMessage { role: "system".into(), content: system },
        ChatMessage { role: "user".into(), content: user },
    ]
}

fn json_slice(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    (end > start).then(|| &reply[start..=end])
}

fn text_of(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        v => v.to_string(),
    }
}

/// 解析并校验模型回复。`today` 为天数(日期参数的兜底)。
pub fn parse(reply: &str, meta: &TableMeta, today: i64) -> Result<AiSuggestion, String> {
    let raw: Raw = json_slice(reply)
        .and_then(|s| serde_json::from_str(s).ok())
        .ok_or_else(|| rust_i18n::t!("ai.err.nl_filter_unreadable").to_string())?;
    let mut columns = vec![];
    let mut dropped = vec![];
    for c in raw.columns {
        let label = format!("{} → {}", c.name, c.generator);
        let Some(col) = meta
            .columns
            .iter()
            .find(|x| x.name == c.name)
            .or_else(|| meta.columns.iter().find(|x| x.name.eq_ignore_ascii_case(&c.name)))
        else {
            dropped.push(label);
            continue;
        };
        let kind = KINDS.iter().find(|k| k.eq_ignore_ascii_case(c.generator.trim()));
        let Some(mut g) = kind.and_then(|k| generator_for_kind(k, col, today)) else {
            dropped.push(label);
            continue;
        };
        // 自增 / 生成列不写;外键列只能取父表的键
        if col.auto.is_some() || (col.fk.is_some() != matches!(g, Generator::ForeignKey)) {
            dropped.push(label);
            continue;
        }
        match &mut g {
            Generator::Integer { min, max } => {
                if let Some(v) = c.min {
                    *min = v.round() as i64;
                }
                if let Some(v) = c.max {
                    *max = v.round() as i64;
                }
            }
            Generator::Decimal { min, max, .. } | Generator::Float { min, max } => {
                if let Some(v) = c.min.filter(|v| v.is_finite()) {
                    *min = v;
                }
                if let Some(v) = c.max.filter(|v| v.is_finite()) {
                    *max = v;
                }
            }
            Generator::Date { from, to } | Generator::Timestamp { from, to } => {
                if let Some(v) = c.from.filter(|v| parse_date(v).is_some()) {
                    *from = v.chars().take(10).collect();
                }
                if let Some(v) = c.to.filter(|v| parse_date(v).is_some()) {
                    *to = v.chars().take(10).collect();
                }
            }
            Generator::PickList { values } => {
                let vs: Vec<String> = c.values.unwrap_or_default().iter().map(text_of).filter(|s| !s.is_empty()).take(200).collect();
                // 有枚举 / CHECK 清单的列只接受清单里的值
                let vs: Vec<String> = if col.values.is_empty() { vs } else { vs.into_iter().filter(|v| col.values.contains(v)).collect() };
                if vs.is_empty() {
                    dropped.push(label);
                    continue;
                }
                *values = vs;
            }
            Generator::Fixed { value } => *value = c.value.as_ref().map(text_of).unwrap_or_default(),
            Generator::Text { min_len, max_len } => {
                if let Some(v) = c.min_len {
                    *min_len = v;
                }
                if let Some(v) = c.max_len {
                    *max_len = v.max(1);
                }
            }
            Generator::Bytes { len } => {
                if let Some(v) = c.len {
                    *len = v.clamp(1, 1024);
                }
            }
            _ => {}
        }
        let required = !col.nullable && !col.has_default;
        if !g.fits(&col.base) || (required && matches!(g, Generator::Skip | Generator::Null)) || (!col.nullable && g == Generator::Null) {
            dropped.push(label);
            continue;
        }
        let null_ratio = if col.nullable && !col.unique { c.null_ratio.unwrap_or(0.0).clamp(0.0, 1.0) } else { 0.0 };
        columns.push(ColumnPlan { name: col.name.clone(), generator: clamp(g, col), null_ratio });
    }
    let note = raw.note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    Ok(AiSuggestion { columns, dropped, note })
}

/// 调用当前配置的模型(一次性请求,不经过对话面板)。只发结构,不发数据。
pub async fn suggest(state: &AppState, meta: &TableMeta, ddl: &str, today: i64) -> Result<AiSuggestion, String> {
    let settings = AiSettingsRepo::new(&state.config_dir).load();
    let ep = provider::resolve(&settings)?;
    let msgs = messages(meta, ddl, &super::gen::format_date(today));
    let full = std::sync::Mutex::new(String::new());
    chat::stream_chat(&ep, &msgs, &|e| {
        if let chat::ChatEvent::Done { full: f } = e {
            *full.lock().unwrap() = f;
        }
    })
    .await?;
    let reply = full.into_inner().unwrap();
    parse(&reply, meta, today)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdata::meta::{AutoKind, BaseType};

    fn col(name: &str, base: BaseType) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            data_type: "x".into(),
            base,
            nullable: true,
            has_default: false,
            auto: None,
            primary: false,
            unique: false,
            values: vec![],
            check_min: None,
            check_max: None,
            fk: None,
        }
    }

    fn meta() -> TableMeta {
        let mut id = col("id", BaseType::Int { min: i64::MIN, max: i64::MAX });
        id.auto = Some(AutoKind::Identity);
        let mut st = col("status", BaseType::Enum);
        st.values = vec!["new".into(), "paid".into()];
        let mut uid = col("user_id", BaseType::Int { min: 0, max: i64::MAX });
        uid.fk = Some("fk".into());
        let mut title = col("title", BaseType::Text { max_len: Some(5) });
        title.nullable = false;
        TableMeta {
            table: "orders".into(),
            columns: vec![
                id,
                col("Price", BaseType::Decimal { precision: Some(5), scale: 2 }),
                st,
                uid,
                title,
                col("placed_at", BaseType::Timestamp),
            ],
            uniques: vec![],
            fks: vec![],
        }
    }

    #[test]
    fn validates_and_clamps_suggestions() {
        let reply = r#"```json
        {"columns":[
          {"name":"id","generator":"integer"},
          {"name":"price","generator":"decimal","min":1,"max":99999999,"nullRatio":0.3},
          {"name":"status","generator":"pickList","values":["paid","bogus"]},
          {"name":"user_id","generator":"integer","min":1,"max":5},
          {"name":"title","generator":"null"},
          {"name":"placed_at","generator":"timestamp","from":"2025-01-01","to":"not a date"},
          {"name":"ghost","generator":"email"},
          {"name":"placed_at","generator":"teleport"}
        ],"note":"  ok  "}
        ```"#;
        let s = parse(reply, &meta(), 20_000).unwrap();
        let names: Vec<&str> = s.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Price", "status", "placed_at"]);
        let Generator::Decimal { max, scale, .. } = s.columns[0].generator else { panic!() };
        assert!((max - 999.99).abs() < 1e-9 && scale == 2);
        assert_eq!(s.columns[0].null_ratio, 0.3);
        assert_eq!(s.columns[1].generator, Generator::PickList { values: vec!["paid".into()] });
        let Generator::Timestamp { from, .. } = &s.columns[2].generator else { panic!() };
        assert_eq!(from, "2025-01-01");
        assert_eq!(s.dropped.len(), 5, "{:?}", s.dropped);
        assert_eq!(s.note.as_deref(), Some("ok"));
        assert!(parse("no json here", &meta(), 0).is_err());
    }

    #[test]
    fn prompt_has_structure_but_no_rows() {
        let m = messages(&meta(), "CREATE TABLE orders (...)", "2026-11-16");
        assert!(m[0].content.contains("pickList") && m[0].content.contains("2026-11-16"));
        assert!(m[1].content.contains("CREATE TABLE orders"));
        assert!(m[1].content.contains("status x allowed values: new | paid"));
        assert!(m[1].content.contains("id x AUTO (skip)"));
    }
}
