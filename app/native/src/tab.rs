//! 查询 / 表浏览标签页用到的纯计算:结果网格(分级着色、对齐、列宽、排序、筛选、导出)、
//! 合成 INSERT、编辑器补全与关键字大写。全部同步、无 IO。
//!
//! 文本偏移:TSX 给的、拿到的都是 UTF-16 偏移(JS 字符串的下标),这里与 core 的字节偏移互转。

use crate::runtime::from;
use crate::text::utf16_to_byte;
use datum_core::completion::{self, ItemKind};
use datum_core::core::connection::DbKind;
use datum_core::grid::{self, Align, CellTier, Conj, FilterOp, FilterRow};
use datum_core::json::{self, Kind, PathSeg};
use datum_core::pg::browse::{ColumnInfo, TableDetail};
use datum_core::schema::{ColumnBrief, ColumnsByTable, ForeignKey};
use datum_core::sql::{Dialect, build};
use nsh_core::JsonValue;
use nsh_core::prelude::*;
use serde::Deserialize;
use serde_json::json;

type Rows = Vec<Vec<Option<String>>>;

fn dialect(kind: &str) -> Result<Dialect, String> {
    let k: DbKind = from(JsonValue::String(kind.to_string()), "kind")?;
    Dialect::from_kind(&k).ok_or_else(|| format!("{kind} 没有 SQL 方言"))
}

fn rows_of(rows: JsonValue) -> Result<Rows, String> {
    from(rows, "rows")
}

/// TSX 侧的一条筛选条件。`path` / `jsonType` 只有在 JSON 树上点字段生成的条件才有。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilterJs {
    id: f64,
    col: String,
    op: String,
    value: String,
    #[serde(default)]
    path: Option<Vec<PathSeg>>,
    #[serde(default)]
    json_type: Option<Kind>,
}

fn op_of(s: &str) -> FilterOp {
    match s {
        "eq" => FilterOp::Eq,
        "ne" => FilterOp::Ne,
        "starts" => FilterOp::Starts,
        "ends" => FilterOp::Ends,
        "gt" => FilterOp::Gt,
        "lt" => FilterOp::Lt,
        "gte" => FilterOp::Gte,
        "lte" => FilterOp::Lte,
        "empty" => FilterOp::Empty,
        "notEmpty" => FilterOp::NotEmpty,
        _ => FilterOp::Contains,
    }
}

fn conj_of(s: &str) -> Conj {
    if s == "or" { Conj::Or } else { Conj::And }
}

fn filters_of(filters: JsonValue) -> Result<Vec<FilterRow>, String> {
    let list: Vec<FilterJs> = from(filters, "filters")?;
    Ok(list
        .into_iter()
        .map(|f| FilterRow {
            id: f.id as u64,
            col: f.col,
            op: op_of(&f.op),
            value: f.value,
            path: f.path.unwrap_or_default(),
            json_type: f.json_type,
        })
        .collect())
}

/// 字节偏移 → UTF-16 偏移。
fn byte_to_utf16(s: &str, byte: usize) -> usize {
    s[..byte.min(s.len())].encode_utf16().count()
}

fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Keyword => "keyword",
        ItemKind::Table => "class",
        ItemKind::Column => "field",
        ItemKind::Snippet => "snippet",
        ItemKind::Command => "function",
        ItemKind::Key => "constant",
    }
}

/// 结果网格与编辑器的纯计算。
#[module]
pub struct Grid;

#[module]
impl Grid {
    /// 单元格分级着色:`{ tier, mono }`,tier 为 `strong` / `normal` / `dim` / `hash` / `null` / `negative`。
    pub fn classify_cell(value: Option<String>, data_type: Option<String>) -> JsonValue {
        let s = grid::classify_cell(value.as_deref(), data_type.as_deref());
        let tier = match s.tier {
            CellTier::Strong => "strong",
            CellTier::Normal => "normal",
            CellTier::Dim => "dim",
            CellTier::Hash => "hash",
            CellTier::Null => "null",
            CellTier::Negative => "negative",
        };
        json!({ "tier": tier, "mono": s.mono })
    }

    /// 按列类型推断对齐:`left` / `right` / `center`。
    pub fn col_align(data_type: Option<String>) -> String {
        match grid::col_align(data_type.as_deref()) {
            Align::Left => "left",
            Align::Right => "right",
            Align::Center => "center",
        }
        .into()
    }

    /// 列宽铺满结果区:内容窄时多余宽度分给可伸缩列,内容宽时保持(横向滚动)。
    pub fn column_widths(auto_widths: Vec<f64>, container_w: f64, default_w: f64, flexible: Vec<bool>) -> Vec<f64> {
        let autos: Vec<f32> = auto_widths.iter().map(|w| *w as f32).collect();
        grid::column_widths(&autos, container_w as f32, default_w as f32, &flexible)
            .into_iter()
            .map(f64::from)
            .collect()
    }

    /// 按某列排序后的行下标(数值感知,NULL 恒在最后)。
    pub fn sort_order(rows: JsonValue, col_ix: u32, descending: bool) -> Result<Vec<u32>, String> {
        let rows = rows_of(rows)?;
        if rows.iter().any(|r| r.len() <= col_ix as usize) {
            return Ok((0..rows.len() as u32).collect());
        }
        Ok(grid::sort_order(&rows, col_ix as usize, descending)
            .into_iter()
            .map(|i| i as u32)
            .collect())
    }

    /// 按条件筛出命中行的下标(保持原顺序);`conj` 为 `and` / `or`。
    pub fn filter_indices(
        rows: JsonValue,
        columns: Vec<String>,
        filters: JsonValue,
        conj: String,
    ) -> Result<Vec<u32>, String> {
        let rows = rows_of(rows)?;
        let filters = filters_of(filters)?;
        Ok(grid::filter_indices(&rows, &columns, &filters, conj_of(&conj))
            .into_iter()
            .map(|i| i as u32)
            .collect())
    }

    /// 条件集的规范化签名(比较草稿与已应用是否一致)。
    pub fn filters_signature(filters: JsonValue, conj: String) -> Result<String, String> {
        let filters = filters_of(filters)?;
        Ok(format!("{:?}", grid::filters_signature(&filters, conj_of(&conj))))
    }

    /// 结果 → CSV。
    pub fn to_csv(columns: Vec<String>, rows: JsonValue) -> Result<String, String> {
        let rows = rows_of(rows)?;
        let refs: Vec<&Vec<Option<String>>> = rows.iter().collect();
        Ok(grid::to_csv(&columns, &refs))
    }

    /// 结果 → Markdown 表格。
    pub fn to_markdown(columns: Vec<String>, rows: JsonValue) -> Result<String, String> {
        let rows = rows_of(rows)?;
        let refs: Vec<&Vec<Option<String>>> = rows.iter().collect();
        Ok(grid::to_markdown(&columns, &refs))
    }

    /// 由结果某一行的真实值合成 INSERT(NULL 原样,其余按方言转义)。
    pub fn build_insert_row(
        table: String,
        columns: Vec<String>,
        values: JsonValue,
        kind: String,
    ) -> Result<String, String> {
        let values: Vec<Option<String>> = from(values, "values")?;
        Ok(build::build_insert_row(&table, &columns, &values, dialect(&kind)?))
    }

    /// 由列名合成 INSERT 模板(值用 `<列名>` 占位)。
    pub fn build_insert(table: String, columns: Vec<String>, kind: String) -> Result<String, String> {
        let detail = TableDetail {
            columns: columns
                .into_iter()
                .map(|name| ColumnInfo {
                    name,
                    data_type: String::new(),
                    default: None,
                    comment: None,
                    not_null: false,
                    is_pk: false,
                })
                .collect(),
            indexes: vec![],
        };
        Ok(build::build_insert(&table, &detail, dialect(&kind)?))
    }

    /// 表浏览的分页 SQL,带服务端筛选:`filters` / `conj` 同筛选条,`types` 为 `{ 列名: 类型名 }`
    /// (决定按数值、日期还是文本比较)。没有有效条件时与普通分页 SQL 相同。
    #[allow(clippy::too_many_arguments)]
    pub fn filtered_page_sql(
        table: String,
        page: u32,
        page_size: u32,
        kind: String,
        sort_col: Option<String>,
        descending: Option<bool>,
        filters: JsonValue,
        conj: String,
        types: JsonValue,
    ) -> Result<String, String> {
        let d = dialect(&kind)?;
        let filters = filters_of(filters)?;
        let types: std::collections::HashMap<String, String> = if types.is_null() { Default::default() } else { from(types, "types")? };
        let w = build::where_clause(&filters, conj_of(&conj), d, &types);
        let sort = sort_col.as_deref().map(|c| (c, descending.unwrap_or(false)));
        Ok(build::page_sql_where(&table, page as usize, page_size as usize, d, sort, w.as_deref()))
    }

    /// 编辑器补全:`{ start, end, items: [{ label, detail, kind, insert }] }`(偏移为 UTF-16),
    /// 无候选为 null。`columns` 为 `{ 表名: [{ name, dataType }] }`,`explicit` 为用户主动请求。
    pub fn completions(
        kind: String,
        text: String,
        caret: u32,
        tables: Vec<String>,
        columns: JsonValue,
        explicit: bool,
        fks: JsonValue,
    ) -> Result<JsonValue, String> {
        let k: DbKind = from(JsonValue::String(kind.clone()), "kind")?;
        let offset = utf16_to_byte(&text, caret as usize);
        let result = match k {
            DbKind::Redis => completion::redis_completions(&text, offset, &tables, explicit),
            DbKind::Mongo => completion::mongo_completions(&text, offset, &tables, explicit),
            _ => {
                let raw: std::collections::BTreeMap<String, Vec<BriefJs>> = if columns.is_null() {
                    Default::default()
                } else {
                    from(columns, "columns")?
                };
                let cols: ColumnsByTable = raw
                    .into_iter()
                    .map(|(t, list)| {
                        (
                            t,
                            list.into_iter()
                                .map(|c| ColumnBrief {
                                    name: c.name,
                                    data_type: c.data_type,
                                })
                                .collect(),
                        )
                    })
                    .collect();
                let fks: Vec<ForeignKey> = if fks.is_null() { vec![] } else { from(fks, "fks")? };
                completion::sql_completions_with(&text, offset, &tables, &cols, &fks, explicit)
            }
        };
        let Some(r) = result else {
            return Ok(JsonValue::Null);
        };
        let items: Vec<JsonValue> = r
            .items
            .into_iter()
            .map(|i| {
                json!({
                    "label": i.label,
                    "detail": i.detail,
                    "kind": kind_name(i.kind),
                    "insert": i.insert,
                })
            })
            .collect();
        Ok(json!({
            "start": byte_to_utf16(&text, r.replace.start),
            "end": byte_to_utf16(&text, r.replace.end),
            "items": items,
        }))
    }

    /// 光标是否在某列的取值位置(`col = '…`、`col IN (…`):
    /// `{ table, column, dataType, start, end, prefix, quoted }`(UTF-16),否则 null。
    pub fn value_context(text: String, caret: u32, columns: JsonValue) -> Result<JsonValue, String> {
        if columns.is_null() {
            return Ok(JsonValue::Null);
        }
        let raw: std::collections::BTreeMap<String, Vec<BriefJs>> = from(columns, "columns")?;
        let cols: ColumnsByTable = raw
            .into_iter()
            .map(|(t, list)| {
                (t, list.into_iter().map(|c| ColumnBrief { name: c.name, data_type: c.data_type }).collect())
            })
            .collect();
        let offset = utf16_to_byte(&text, caret as usize);
        let Some(v) = completion::value_context(&text, offset, &cols) else {
            return Ok(JsonValue::Null);
        };
        let data_type = cols
            .get(&v.table)
            .and_then(|cs| cs.iter().find(|c| c.name == v.column))
            .map(|c| c.data_type.clone())
            .unwrap_or_default();
        Ok(json!({
            "table": v.table,
            "column": v.column,
            "dataType": data_type,
            "start": byte_to_utf16(&text, v.replace.start),
            "end": byte_to_utf16(&text, v.replace.end),
            "prefix": v.prefix,
            "quoted": v.quoted,
        }))
    }

    /// 光标是否在 JSON 列的键名位置(`col->'…`、`col->'a'->>'…`):
    /// `{ table, column, dataType, path, start, end, prefix }`(UTF-16),否则 null。是否 JSON 类型由界面判断。
    pub fn json_key_context(text: String, caret: u32, columns: JsonValue) -> Result<JsonValue, String> {
        if columns.is_null() {
            return Ok(JsonValue::Null);
        }
        let raw: std::collections::BTreeMap<String, Vec<BriefJs>> = from(columns, "columns")?;
        let cols: ColumnsByTable = raw
            .into_iter()
            .map(|(t, list)| {
                (t, list.into_iter().map(|c| ColumnBrief { name: c.name, data_type: c.data_type }).collect())
            })
            .collect();
        let offset = utf16_to_byte(&text, caret as usize);
        let Some(v) = completion::json_key_context(&text, offset, &cols) else {
            return Ok(JsonValue::Null);
        };
        let data_type = cols
            .get(&v.table)
            .and_then(|cs| cs.iter().find(|c| c.name == v.column))
            .map(|c| c.data_type.clone())
            .unwrap_or_default();
        Ok(json!({
            "table": v.table,
            "column": v.column,
            "dataType": data_type,
            "path": v.path,
            "start": byte_to_utf16(&text, v.replace.start),
            "end": byte_to_utf16(&text, v.replace.end),
            "prefix": v.prefix,
        }))
    }

    /// 解析 JSON 单元格:`{ ok: true, node }`(node 见 `json::Node::to_js`,对象保留原文键顺序、数字保留原文)
    /// 或 `{ ok: false, error: { kind, line, column, found } }`。
    pub fn json_parse(text: String) -> JsonValue {
        match json::parse(&text) {
            Ok(node) => json!({ "ok": true, "node": node.to_js() }),
            Err(e) => json!({ "ok": false, "error": e }),
        }
    }

    /// 值看起来是 JSON 对象 / 数组(以 `{` / `[` 开头)且能解析:没有列类型时据此决定是否展开成树。
    pub fn is_json_document(text: String) -> bool {
        json::looks_like_document(&text) && json::parse(&text).is_ok()
    }

    /// 格式化(`pretty`,2 空格缩进)或压缩成一行;键顺序与数字写法不变。不是合法 JSON 时报错。
    pub fn json_format(text: String, pretty: bool) -> Result<String, String> {
        json::parse(&text)
            .map(|n| json::to_text(&n, pretty))
            .map_err(|e| format!("{:?} at {}:{}", e.kind, e.line, e.column))
    }

    /// 路径处的值,复制用:标量为其文本(字符串不带引号,null 为 `null`),对象 / 数组为格式化的 JSON。
    /// 路径不存在或不是合法 JSON 为 null。
    pub fn json_value_at(text: String, path: JsonValue) -> Result<Option<String>, String> {
        let path: Vec<PathSeg> = from(path, "path")?;
        let Ok(doc) = json::parse(&text) else { return Ok(None) };
        Ok(doc.at(&path).map(|n| match n {
            json::Node::Object(_) | json::Node::Array(_) => json::to_text(n, true),
            _ => n.as_text().unwrap_or_else(|| "null".into()),
        }))
    }

    /// 字段路径的写法(复制路径、筛选条上显示):PG 为 `"col"->'a'->0->>'b'`,
    /// MySQL / SQLite 为 `$.a[0].b`(`JSON_EXTRACT` / `json_extract` 的路径)。
    pub fn json_path_text(kind: String, column: String, path: JsonValue) -> Result<String, String> {
        let path: Vec<PathSeg> = from(path, "path")?;
        // 没有 SQL 方言的(Redis 值的 JSON 树)也用 `$.a[0].b`:通用、jq 之类工具也认
        Ok(match dialect(&kind) {
            Ok(d @ Dialect::Pg) => json::pg_path(&build::ident(&column, d), &path, true),
            _ => json::dollar_path(&path),
        })
    }

    /// 刚输入边界字符时,紧邻前面的 SQL 关键字转大写:`{ start, end, text }`(UTF-16),否则 null。
    pub fn uppercase_keyword_before(text: String, caret: u32) -> JsonValue {
        let offset = utf16_to_byte(&text, caret as usize);
        match completion::uppercase_keyword_before(&text, offset) {
            Some((range, upper)) => json!({
                "start": byte_to_utf16(&text, range.start),
                "end": byte_to_utf16(&text, range.end),
                "text": upper,
            }),
            None => JsonValue::Null,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BriefJs {
    name: String,
    #[serde(default)]
    data_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_round_trip() {
        let s = "选 a";
        assert_eq!(byte_to_utf16(s, 3), 1);
        assert_eq!(utf16_to_byte(s, 1), 3);
    }

    #[test]
    fn uppercases_keyword() {
        let v = Grid::uppercase_keyword_before("select ".into(), 7);
        assert_eq!(v["text"], "SELECT");
        assert_eq!(v["start"], 0);
        assert_eq!(v["end"], 6);
    }

    #[test]
    fn filters_and_sorts() {
        let rows = json!([["2", "b"], ["10", null], ["1", "a"]]);
        let order = Grid::sort_order(rows.clone(), 0, false).unwrap();
        assert_eq!(order, vec![2, 0, 1]);
        let hit = Grid::filter_indices(
            rows,
            vec!["n".into(), "s".into()],
            json!([{ "id": 1, "col": "s", "op": "notEmpty", "value": "" }]),
            "and".into(),
        )
        .unwrap();
        assert_eq!(hit, vec![0, 2]);
    }

    #[test]
    fn json_cells_and_path_filters() {
        let ok = Grid::json_parse(r#"{"b": 1.10, "a": [null]}"#.into());
        assert_eq!(ok["node"]["entries"][0]["key"], "b");
        assert_eq!(ok["node"]["entries"][0]["node"]["text"], "1.10");
        let bad = Grid::json_parse("{\"a\": }".into());
        assert_eq!((bad["ok"].clone(), bad["error"]["kind"].clone(), bad["error"]["column"].clone()), (json!(false), json!("unexpected"), json!(7)));
        assert_eq!(Grid::json_format(r#"{ "a" : [1, 2] }"#.into(), false).unwrap(), r#"{"a":[1,2]}"#);
        assert!(Grid::json_format("{".into(), true).is_err());
        assert_eq!(Grid::json_value_at(r#"{"a":{"s":"x"}}"#.into(), json!(["a", "s"])).unwrap().as_deref(), Some("x"));
        assert_eq!(Grid::json_path_text("pg".into(), "doc".into(), json!(["a", 0.0])).unwrap(), "\"doc\"->'a'->>0");
        assert_eq!(Grid::json_path_text("mysql".into(), "doc".into(), json!(["a", 0])).unwrap(), "$.a[0]");
        assert_eq!(Grid::json_path_text("redis".into(), "value".into(), json!(["a", 0])).unwrap(), "$.a[0]");
        assert!(Grid::is_json_document(" [1]".into()));
        assert!(!Grid::is_json_document("42".into()));
        // JS 传来的 JSON 路径条件:客户端按路径筛,服务端拼成 PG 的 WHERE
        let filters = json!([{ "id": 1, "col": "doc", "op": "eq", "value": "3", "path": ["a"], "jsonType": "number" }]);
        let rows = json!([[r#"{"a": 3}"#], [r#"{"a": "3"}"#]]);
        assert_eq!(Grid::filter_indices(rows, vec!["doc".into()], filters.clone(), "and".into()).unwrap(), vec![0]);
        let sql = Grid::filtered_page_sql("t".into(), 0, 100, "pg".into(), None, None, filters, "and".into(), json!({ "doc": "jsonb" })).unwrap();
        assert_eq!(sql, r#"SELECT * FROM "t" WHERE ("doc" @> '{"a":3}'::jsonb) LIMIT 100 OFFSET 0"#);
    }
}
