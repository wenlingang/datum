//! ER 图:取整库外键 / 主键,构图、分层布局、推开重叠、搜索(纯计算部分同步)。
//!
//! 图与坐标的算法都在 `datum_core::erd`;这里只做 JSON 进出。平移缩放、连线走线、
//! 绘制在 TSX(`app/erd`)。

use crate::runtime::{err, from, json, spawn, state};
use datum_core::core::connection::DbKind;
use datum_core::erd::{self, ErdGraph, GraphOptions, NodeRect};
use datum_core::schema::{ColumnsByTable, ErdSchema, ForeignKey};
use datum_core::service;
use nsh_core::JsonValue;
use nsh_core::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;

/// `build` 的选项(TSX 传入)。
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct BuildOptions {
    /// 聚焦表:只保留它与直接邻居
    focus: Option<String>,
    /// 表 → 是否折叠(只显示键列);未给出的按表数自动决定
    collapsed: HashMap<String, bool>,
    /// 表 → 显示全部字段(否则展开态截断)
    full: HashMap<String, bool>,
    /// 是否加入按命名约定推断的外键
    inferred: bool,
}

/// 物理外键 + (可选)推断外键。
fn with_inferred(tables: &[String], columns: &ColumnsByTable, schema: ErdSchema, inferred: bool) -> ErdSchema {
    if !inferred {
        return schema;
    }
    let extra = erd::infer_foreign_keys(tables, columns, &schema.fks);
    ErdSchema { fks: schema.fks.into_iter().chain(extra).collect(), ..schema }
}

/// 构图:`{ nodes: [{ table, columns, collapsed, hiddenCount, isFocus, w, h }], edges, fks }`。
/// `fks` 为实际参与的外键(含推断),供搜索按约束名匹配。
fn build_json(tables: Vec<String>, columns: ColumnsByTable, schema: ErdSchema, opts: BuildOptions) -> Result<JsonValue, String> {
    let schema = with_inferred(&tables, &columns, schema, opts.inferred);
    let graph = erd::build_graph(
        &tables,
        &columns,
        &schema,
        &GraphOptions { focus: opts.focus.as_deref(), collapsed: &opts.collapsed, full: &opts.full },
    );
    let nodes = graph
        .nodes
        .iter()
        .map(|n| {
            let (w, h) = erd::estimate_node_size(n);
            let mut v = json(n)?;
            v["w"] = w.into();
            v["h"] = h.into();
            Ok(v)
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(serde_json::json!({ "nodes": nodes, "edges": json(&graph.edges)?, "fks": json(&schema.fks)? }))
}

/// ER 图。
#[module]
pub struct Erd;

#[module]
impl Erd {
    /// 整库外键 + 主键:`{ fks: [{ constraint, table, column, refTable, refColumn, inferred }], pkByTable }`。
    pub async fn schema(kind: String, id: String) -> Result<JsonValue, String> {
        let k: DbKind = from(JsonValue::String(kind), "kind")?;
        spawn(async move { service::fetch_erd_schema(&state(), &k, &id).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 构图(见 `build_json`)。节点的 `w` / `h` 为估算的绘制尺寸。
    pub fn build(tables: JsonValue, columns: JsonValue, schema: JsonValue, opts: JsonValue) -> Result<JsonValue, String> {
        build_json(
            from(tables, "tables")?,
            from(columns, "columns")?,
            from(schema, "schema")?,
            if opts.is_null() { BuildOptions::default() } else { from(opts, "opts")? },
        )
    }

    /// 分层布局:`{ 表名: { x, y, w, h } }`。`graph` 为 `build` 结果(多余字段忽略)。
    pub fn layout(graph: JsonValue) -> Result<JsonValue, String> {
        let graph: ErdGraph = from(graph, "graph")?;
        json(erd::layout(&graph))
    }

    /// 就地推开互相压住的节点;`fixed` 指定的表不动(刚展开长高的表)。
    pub fn resolve(rects: JsonValue, fixed: Option<String>) -> Result<JsonValue, String> {
        let mut rects: HashMap<String, NodeRect> = from(rects, "rects")?;
        erd::resolve_overlaps(&mut rects, fixed.as_deref());
        json(rects)
    }

    /// 搜索:表名 / 列名 / 外键约束名(大小写不敏感)命中的表。
    pub fn match_tables(query: String, tables: JsonValue, columns: JsonValue, fks: JsonValue) -> Result<Vec<String>, String> {
        let tables: Vec<String> = from(tables, "tables")?;
        let columns: ColumnsByTable = from(columns, "columns")?;
        let fks: Vec<ForeignKey> = from(fks, "fks")?;
        Ok(erd::match_tables(&query, &tables, &columns, &fks).into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json as j;

    fn sample() -> (JsonValue, JsonValue, JsonValue) {
        let tables = j!(["customers", "orders", "notes"]);
        let columns = j!({
            "customers": [{ "name": "id", "dataType": "int" }, { "name": "name", "dataType": "text" }],
            "orders": [{ "name": "id", "dataType": "int" }, { "name": "customer_id", "dataType": "int" }],
            "notes": [{ "name": "id", "dataType": "int" }, { "name": "order_id", "dataType": "int" }],
        });
        let schema = j!({
            "fks": [{ "constraint": "fk_o_c", "table": "orders", "column": "customer_id",
                      "refTable": "customers", "refColumn": "id", "inferred": false }],
            "pkByTable": { "customers": ["id"], "orders": ["id"], "notes": ["id"] },
        });
        (tables, columns, schema)
    }

    #[test]
    fn build_returns_nodes_with_size_and_edges() {
        let (t, c, s) = sample();
        let g = Erd::build(t, c, s, JsonValue::Null).unwrap();
        assert_eq!(g["nodes"].as_array().unwrap().len(), 3);
        let orders = g["nodes"].as_array().unwrap().iter().find(|n| n["table"] == "orders").unwrap();
        assert!(orders["w"].as_f64().unwrap() >= erd::NODE_MIN_W as f64);
        assert!(orders["h"].as_f64().unwrap() > 0.0);
        assert_eq!(orders["columns"][1]["isFk"], true);
        assert_eq!(orders["hiddenCount"], 0);
        assert_eq!(g["edges"], j!([{ "id": "orders|fk_o_c", "source": "orders", "target": "customers",
                                     "label": "customer_id → id", "inferred": false }]));
    }

    #[test]
    fn build_adds_inferred_edges_on_request() {
        let (t, c, s) = sample();
        let g = Erd::build(t, c, s, j!({ "inferred": true })).unwrap();
        let edges = g["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().any(|e| e["source"] == "notes" && e["target"] == "orders" && e["inferred"] == true));
        assert_eq!(g["fks"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn build_focus_keeps_neighbors_only() {
        let (t, c, s) = sample();
        let g = Erd::build(t, c, s, j!({ "focus": "customers" })).unwrap();
        let mut names: Vec<_> = g["nodes"].as_array().unwrap().iter().map(|n| n["table"].as_str().unwrap().to_string()).collect();
        names.sort();
        assert_eq!(names, ["customers", "orders"]);
    }

    #[test]
    fn layout_places_referrer_left_of_referenced() {
        let (t, c, s) = sample();
        let g = Erd::build(t, c, s, JsonValue::Null).unwrap();
        let rects = Erd::layout(g).unwrap();
        assert_eq!(rects.as_object().unwrap().len(), 3);
        assert!(rects["orders"]["x"].as_f64().unwrap() < rects["customers"]["x"].as_f64().unwrap());
    }

    #[test]
    fn resolve_pushes_overlapping_apart_keeping_fixed() {
        let rects = j!({ "a": { "x": 0.0, "y": 0.0, "w": 200.0, "h": 100.0 },
                         "b": { "x": 10.0, "y": 10.0, "w": 200.0, "h": 100.0 } });
        let out = Erd::resolve(rects, Some("a".into())).unwrap();
        assert_eq!(out["a"], j!({ "x": 0.0, "y": 0.0, "w": 200.0, "h": 100.0 }));
        assert!(out["b"] != j!({ "x": 10.0, "y": 10.0, "w": 200.0, "h": 100.0 }));
    }

    #[test]
    fn match_tables_by_column_name() {
        let (t, c, s) = sample();
        let hits = Erd::match_tables("CUSTOMER_ID".into(), t, c, s["fks"].clone()).unwrap();
        assert_eq!(hits, ["orders"]);
    }
}
