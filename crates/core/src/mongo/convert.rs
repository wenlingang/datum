//! BSON → 展示:mongosh 风格文本(JSON 视图与文档编辑器)与单元格文本(表格视图)。
//!
//! `to_shell` 的输出恰好是 [`parse_value`](super::parse::parse_value) 能解析的语法,
//! 且类型标注齐全,保证"打开编辑器 → 原样保存"不改变任何 BSON 类型。

use base64::Engine as _;
use mongodb::bson::{spec::BinarySubtype, Bson, Document};
use std::collections::{HashMap, HashSet};

pub fn to_shell(v: &Bson, pretty: bool) -> String {
    let mut out = String::new();
    write(&mut out, v, pretty, 0);
    out
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
}

/// 能作为裸 key 输出的字段名(否则加引号)。
fn bare_key(k: &str) -> bool {
    !k.is_empty()
        && !k.starts_with(|c: char| c.is_ascii_digit())
        && k.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

fn newline(out: &mut String, pretty: bool, indent: usize) {
    if pretty {
        out.push('\n');
        out.push_str(&"  ".repeat(indent));
    }
}

fn write_doc(out: &mut String, d: &Document, pretty: bool, indent: usize) {
    if d.is_empty() {
        out.push_str("{}");
        return;
    }
    out.push('{');
    for (i, (k, v)) in d.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if pretty {
            newline(out, true, indent + 1);
        } else {
            out.push(' ');
        }
        out.push_str(&if bare_key(k) { k.clone() } else { json_str(k) });
        out.push_str(": ");
        write(out, v, pretty, indent + 1);
    }
    if pretty {
        newline(out, true, indent);
    } else {
        out.push(' ');
    }
    out.push('}');
}

fn write(out: &mut String, v: &Bson, pretty: bool, indent: usize) {
    match v {
        Bson::Document(d) => write_doc(out, d, pretty, indent),
        Bson::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if !pretty {
                        out.push(' ');
                    }
                }
                newline(out, pretty, indent + 1);
                write(out, item, pretty, indent + 1);
            }
            newline(out, pretty, indent);
            out.push(']');
        }
        other => out.push_str(&scalar_shell(other)),
    }
}

fn fmt_double(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0. {
            "Infinity".into()
        } else {
            "-Infinity".into()
        }
    } else if f.fract() == 0. && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        // Debug 输出最短且可精确回读的表示(如 0.1、1e20)
        format!("{f:?}")
    }
}

/// 毫秒 → `2024-01-02T03:04:05.678Z`;年份超出 0–9999 时返回 None。
fn iso_millis(ms: i64) -> Option<String> {
    use chrono::{Datelike as _, SecondsFormat};
    let d = chrono::DateTime::from_timestamp_millis(ms)?;
    (0..=9999)
        .contains(&d.year())
        .then(|| d.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn scalar_shell(v: &Bson) -> String {
    match v {
        Bson::String(s) => json_str(s),
        Bson::Boolean(b) => b.to_string(),
        Bson::Null => "null".into(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => format!("NumberLong({n})"),
        Bson::Double(f) => fmt_double(*f),
        Bson::Decimal128(d) => format!("NumberDecimal(\"{d}\")"),
        Bson::ObjectId(o) => format!("ObjectId(\"{o}\")"),
        Bson::DateTime(dt) => {
            let ms = dt.timestamp_millis();
            match iso_millis(ms) {
                Some(s) => format!("ISODate(\"{s}\")"),
                None => format!("new Date({ms})"),
            }
        }
        Bson::Binary(b) if b.subtype == BinarySubtype::Uuid && b.bytes.len() == 16 => {
            let bytes: [u8; 16] = b.bytes.clone().try_into().unwrap_or([0; 16]);
            format!("UUID(\"{}\")", mongodb::bson::Uuid::from_bytes(bytes))
        }
        Bson::Binary(b) => format!(
            "BinData({}, \"{}\")",
            u8::from(b.subtype),
            base64::engine::general_purpose::STANDARD.encode(&b.bytes)
        ),
        // 字面量里出现 / 或换行无法无损表达,改用构造器形式
        Bson::RegularExpression(r) if r.pattern.contains(['/', '\n']) => {
            format!("RegExp({}, {})", json_str(&r.pattern), json_str(&r.options))
        }
        Bson::RegularExpression(r) => format!("/{}/{}", r.pattern, r.options),
        Bson::Timestamp(t) => format!("Timestamp({}, {})", t.time, t.increment),
        Bson::MinKey => "MinKey()".into(),
        Bson::MaxKey => "MaxKey()".into(),
        // 以下类型只做展示:输出成解析器不接受的构造器形式,编辑器会拒绝保存。
        // 不能直接输出代码原文,`Code("1")` 之类会被解析成别的类型,甚至多出字段。
        Bson::JavaScriptCode(code) => format!("Code({})", json_str(code)),
        Bson::JavaScriptCodeWithScope(c) => format!(
            "Code({}, {})",
            json_str(&c.code),
            to_shell(&Bson::Document(c.scope.clone()), false)
        ),
        Bson::Symbol(sym) => format!("Symbol({})", json_str(sym)),
        Bson::Undefined => "undefined".into(),
        other => format!("Unsupported({})", json_str(&other.to_string())),
    }
}

/// 表格单元格文本:字符串不加引号,数字不带类型包装,日期为 ISO,嵌套值为紧凑 mongosh 文本。
pub fn cell_text(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(f) => fmt_double(*f),
        Bson::Decimal128(d) => d.to_string(),
        Bson::DateTime(dt) => {
            iso_millis(dt.timestamp_millis()).unwrap_or_else(|| dt.timestamp_millis().to_string())
        }
        other => to_shell(other, false),
    }
}

/// 一次查询的展示数据。`rows` 与关系库的 `QueryResult.rows` 同形,网格直接复用;
/// `docs` / `ids` 与 `rows` 按下标一一对应。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MongoResult {
    /// 顶层字段并集,按首次出现顺序,`_id` 固定第一列
    pub columns: Vec<String>,
    /// None = 该文档没有这个字段(与值为 null 区分)
    pub rows: Vec<Vec<Option<String>>>,
    /// 每篇文档的 mongosh 风格缩进文本(JSON 视图 / 文档编辑器)
    pub docs: Vec<String>,
    /// 原始 `_id`,编辑 / 删除时定位
    pub ids: Vec<Option<Bson>>,
    /// 列 → MongoDB 类型名(表头展示与网格配色)
    pub types: HashMap<String, String>,
    /// 结果被 max_rows 截断
    pub truncated: bool,
    /// 写操作或标量结果的摘要
    pub summary: Option<String>,
}

pub fn docs_to_result(docs: Vec<Document>, truncated: bool) -> MongoResult {
    let mut columns: Vec<String> = vec![];
    let mut seen: HashSet<&str> = HashSet::new();
    if docs.iter().any(|d| d.contains_key("_id")) {
        columns.push("_id".into());
        seen.insert("_id");
    }
    for d in &docs {
        for k in d.keys() {
            if seen.insert(k.as_str()) {
                columns.push(k.clone());
            }
        }
    }
    let rows = docs
        .iter()
        .map(|d| columns.iter().map(|c| d.get(c).map(cell_text)).collect())
        .collect();
    let texts = docs
        .iter()
        .map(|d| {
            let mut out = String::new();
            write_doc(&mut out, d, true, 0);
            out
        })
        .collect();
    MongoResult {
        rows,
        docs: texts,
        ids: docs.iter().map(|d| d.get("_id").cloned()).collect(),
        types: column_types(&docs),
        columns,
        truncated,
        summary: None,
    }
}

/// 单列结果(distinct、计数、库 / 集合列表)。
pub fn values_to_result(column: &str, values: Vec<Bson>) -> MongoResult {
    let types = values
        .iter()
        .find_map(type_name)
        .map(|t| HashMap::from([(column.to_string(), t.to_string())]))
        .unwrap_or_default();
    MongoResult {
        columns: vec![column.to_string()],
        rows: values.iter().map(|v| vec![Some(cell_text(v))]).collect(),
        docs: values.iter().map(|v| to_shell(v, true)).collect(),
        ids: vec![None; values.len()],
        types,
        truncated: false,
        summary: None,
    }
}

/// MongoDB 类型名;null 没有类型(列类型取第一个非 null 值)。
fn type_name(v: &Bson) -> Option<&'static str> {
    Some(match v {
        Bson::Null => return None,
        Bson::ObjectId(_) => "objectId",
        Bson::Binary(_) => "binData",
        Bson::Int32(_) => "int32",
        Bson::Int64(_) => "int64",
        Bson::Double(_) => "double",
        Bson::Decimal128(_) => "decimal",
        Bson::DateTime(_) => "date",
        Bson::Timestamp(_) => "timestamp",
        Bson::Boolean(_) => "bool",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        _ => "other",
    })
}

/// 列 → MongoDB 类型名(取该列第一个非 null 值),表头展示并决定网格配色(见 `grid::classify_cell`)。
pub fn column_types(docs: &[Document]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for d in docs {
        for (k, v) in d {
            if !out.contains_key(k) {
                if let Some(t) = type_name(v) {
                    out.insert(k.clone(), t.to_string());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mongo::parse::parse_value;
    use mongodb::bson::{
        doc, oid::ObjectId, spec::BinarySubtype, Binary, Bson, Decimal128, Regex, Timestamp,
    };

    fn sample() -> Bson {
        Bson::Document(doc! {
            "_id": ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60718").unwrap(),
            "i32": 7_i32,
            "i64": 9_007_199_254_740_993_i64,
            "dbl_int": 3.0_f64,
            "dbl": 0.1_f64,
            "big": 1e20_f64,
            "neg": -2.5_f64,
            "dec": "1.10".parse::<Decimal128>().unwrap(),
            "date": mongodb::bson::DateTime::from_millis(1_704_164_645_678),
            "old_date": mongodb::bson::DateTime::from_millis(-62_200_000_000_000),
            "uuid": Binary { subtype: BinarySubtype::Uuid, bytes: vec![0x12; 16] },
            "bin": Binary { subtype: BinarySubtype::Generic, bytes: vec![1, 2, 3] },
            "re": Regex { pattern: "^a\\/b".into(), options: "i".into() },
            "re_slash": Regex { pattern: "a/b".into(), options: "".into() },
            "re_plain": Regex { pattern: "^x[0-9]+$".into(), options: "im".into() },
            "ts": Timestamp { time: 1, increment: 2 },
            "s": "quote\" nl\n ctl\u{1} emoji😀 'single' back\\slash",
            "b": true,
            "n": Bson::Null,
            "min": Bson::MinKey,
            "max": Bson::MaxKey,
            "a b": 1,
            "nested": { "arr": [1, "x", { "y": [] }], "empty": {} },
        })
    }

    #[test]
    fn shell_text_round_trips_exactly() {
        let v = sample();
        for pretty in [false, true] {
            let text = to_shell(&v, pretty);
            assert_eq!(parse_value(&text).unwrap(), v, "pretty={pretty}\n{text}");
        }
    }

    #[test]
    fn shell_text_shapes() {
        assert_eq!(to_shell(&Bson::Int64(5), false), "NumberLong(5)");
        assert_eq!(to_shell(&Bson::Double(3.0), false), "3.0");
        assert_eq!(to_shell(&Bson::Double(f64::NAN), false), "NaN");
        assert_eq!(
            to_shell(&Bson::Document(doc! { "a": 1, "b": [1, 2] }), true),
            "{\n  a: 1,\n  b: [\n    1,\n    2\n  ]\n}"
        );
        assert_eq!(
            to_shell(&Bson::Document(doc! { "a b": 1, "$set": {} }), false),
            "{ \"a b\": 1, $set: {} }"
        );
    }

    #[test]
    fn unsupported_types_do_not_round_trip_silently() {
        // 代码文本本身可能就是合法的值或字段片段,不能原样输出
        for code in ["x", "1", "true", "{}", "null", "{a:1}", "1, b: 2"] {
            let v = Bson::Document(doc! { "code": Bson::JavaScriptCode(code.into()) });
            assert!(parse_value(&to_shell(&v, true)).is_err(), "Code({code:?})");
        }
        let v = Bson::Document(doc! { "s": Bson::Symbol("1".into()) });
        assert!(parse_value(&to_shell(&v, false)).is_err());
        let v = Bson::Document(doc! { "u": Bson::Undefined });
        assert!(parse_value(&to_shell(&v, false)).is_err());
    }

    #[test]
    fn cell_text_is_readable() {
        assert_eq!(cell_text(&Bson::String("hi".into())), "hi");
        assert_eq!(cell_text(&Bson::Int64(5)), "5");
        assert_eq!(cell_text(&Bson::Double(3.0)), "3.0");
        assert_eq!(cell_text(&Bson::Null), "null");
        assert_eq!(
            cell_text(&Bson::DateTime(mongodb::bson::DateTime::from_millis(0))),
            "1970-01-01T00:00:00.000Z"
        );
        assert_eq!(cell_text(&Bson::Document(doc! { "a": 1 })), "{ a: 1 }");
    }

    #[test]
    fn docs_to_result_columns_and_missing_vs_null() {
        let r = docs_to_result(
            vec![
                doc! { "name": "a", "_id": 1 },
                doc! { "_id": 2, "age": 3, "name": Bson::Null },
            ],
            false,
        );
        assert_eq!(r.columns, vec!["_id", "name", "age"]);
        assert_eq!(r.rows[0], vec![Some("1".into()), Some("a".into()), None]);
        assert_eq!(
            r.rows[1],
            vec![Some("2".into()), Some("null".into()), Some("3".into())]
        );
        assert_eq!(r.ids, vec![Some(Bson::Int32(1)), Some(Bson::Int32(2))]);
        assert_eq!(r.docs.len(), 2);
        assert!(r.docs[0].starts_with("{\n  name: \"a\""));
    }

    #[test]
    fn values_to_result_is_single_column() {
        let r = values_to_result("city", vec![Bson::String("x".into()), Bson::Int32(1)]);
        assert_eq!(r.columns, vec!["city"]);
        assert_eq!(r.rows, vec![vec![Some("x".into())], vec![Some("1".into())]]);
        assert_eq!(r.ids, vec![None, None]);
    }

    #[test]
    fn column_types_from_first_non_null() {
        let t = column_types(&[
            doc! { "_id": ObjectId::new(), "n": Bson::Null },
            doc! { "n": 1_i64, "at": mongodb::bson::DateTime::from_millis(0) },
        ]);
        assert_eq!(t.get("_id").map(String::as_str), Some("objectId"));
        assert_eq!(t.get("n").map(String::as_str), Some("int64"));
        assert_eq!(t.get("at").map(String::as_str), Some("date"));
    }

    #[test]
    fn results_carry_column_types() {
        let r = docs_to_result(
            vec![
                doc! { "_id": ObjectId::new(), "n": Bson::Null, "code": Bson::JavaScriptCode("x".into()) },
                doc! { "n": 1_i64 },
            ],
            false,
        );
        assert_eq!(r.types.get("_id").map(String::as_str), Some("objectId"));
        assert_eq!(r.types.get("n").map(String::as_str), Some("int64"));
        // 无法回读成 BSON 的类型也要有类型名
        assert_eq!(r.types.get("code").map(String::as_str), Some("other"));
        let v = values_to_result("city", vec![Bson::Null, Bson::String("x".into())]);
        assert_eq!(v.types.get("city").map(String::as_str), Some("string"));
    }
}
