//! ER 关系图:由表/列/外键构建节点与边,并做分层布局(纯函数,UI 只负责绘制)。

use crate::schema::{ColumnsByTable, ErdSchema, ForeignKey};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// 整库模式下表数超过该值时节点默认折叠(只显示键列)。
pub const COLLAPSE_THRESHOLD: usize = 30;
/// 展开态下单表最多直接显示的行数,超出折入「+N 个字段」。
pub const ROW_CAP: usize = 12;

/// 节点几何(与 UI 的表节点绘制约定一致)。
pub const NODE_HEADER_H: f32 = 34.;
pub const NODE_ROW_H: f32 = 22.;
pub const NODE_PAD_Y: f32 = 6.;
pub const NODE_MIN_W: f32 = 200.;
pub const NODE_MAX_W: f32 = 320.;
const LAYER_GAP: f32 = 70.;
const NODE_GAP: f32 = 40.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErdColumn {
    pub name: String,
    pub data_type: String,
    pub is_pk: bool,
    pub is_fk: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErdNode {
    pub table: String,
    /// 可见列(折叠时只含 PK/FK 列)
    pub columns: Vec<ErdColumn>,
    pub collapsed: bool,
    /// 未显示的列数(尾行「+N 个字段」)
    pub hidden_count: usize,
    pub is_focus: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErdEdge {
    /// 引用方表 + 约束名(复合外键合并为一条边)
    pub id: String,
    pub source: String,
    pub target: String,
    /// "col → refCol",复合外键多行拼接
    pub label: String,
    pub inferred: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ErdGraph {
    pub nodes: Vec<ErdNode>,
    pub edges: Vec<ErdEdge>,
}

/// 按命名约定推断逻辑外键:`xxx_id` 列 → 名为 xxx / xxxs / xxxes / xxies 的表(大小写不敏感),
/// 且目标表存在 `id` 列。已有物理外键覆盖的(表, 列)不重复推断;自引用跳过。
pub fn infer_foreign_keys(
    tables: &[String],
    columns: &ColumnsByTable,
    existing: &[ForeignKey],
) -> Vec<ForeignKey> {
    let by_lower: HashMap<String, &String> = tables.iter().map(|t| (t.to_lowercase(), t)).collect();
    let covered: HashSet<(&str, &str)> = existing
        .iter()
        .map(|f| (f.table.as_str(), f.column.as_str()))
        .collect();
    let has_id = |t: &str| {
        columns
            .get(t)
            .is_some_and(|cs| cs.iter().any(|c| c.name.eq_ignore_ascii_case("id")))
    };
    let mut out = Vec::new();
    for t in tables {
        for c in columns.get(t).map(Vec::as_slice).unwrap_or_default() {
            let lower = c.name.to_lowercase();
            let Some(base) = lower.strip_suffix("_id").filter(|b| !b.is_empty()) else {
                continue;
            };
            if covered.contains(&(t.as_str(), c.name.as_str())) {
                continue;
            }
            let mut candidates = vec![base.to_string(), format!("{base}s"), format!("{base}es")];
            if let Some(stem) = base.strip_suffix('y') {
                candidates.push(format!("{stem}ies"));
            }
            let Some(ref_table) = candidates.iter().find_map(|n| by_lower.get(n)) else {
                continue;
            };
            if *ref_table == t || !has_id(ref_table) {
                continue;
            }
            out.push(ForeignKey {
                constraint: format!("inferred_{t}_{}", c.name),
                table: t.clone(),
                column: c.name.clone(),
                ref_table: (*ref_table).clone(),
                ref_column: "id".into(),
                inferred: true,
            });
        }
    }
    out
}

/// 与某表有外键关联的表集合(双向),自引用不算。
pub fn neighbors_of(table: &str, fks: &[ForeignKey]) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for fk in fks {
        if fk.table == table {
            set.insert(fk.ref_table.clone());
        }
        if fk.ref_table == table {
            set.insert(fk.table.clone());
        }
    }
    set.remove(table);
    set
}

pub struct GraphOptions<'a> {
    /// 聚焦表:只保留中心表与其直接邻居
    pub focus: Option<&'a str>,
    /// 表 → 是否折叠的覆盖;未覆盖的按 COLLAPSE_THRESHOLD 自动决定
    pub collapsed: &'a HashMap<String, bool>,
    /// 表 → 完整显示全部字段(否则展开态按 ROW_CAP 截断)
    pub full: &'a HashMap<String, bool>,
}

pub fn build_graph(
    tables: &[String],
    columns: &ColumnsByTable,
    schema: &ErdSchema,
    opts: &GraphOptions,
) -> ErdGraph {
    let visible: Vec<&String> = match opts.focus {
        Some(focus) => {
            let mut keep = neighbors_of(focus, &schema.fks);
            keep.insert(focus.to_string());
            tables.iter().filter(|t| keep.contains(*t)).collect()
        }
        None => tables.iter().collect(),
    };
    let visible_set: HashSet<&str> = visible.iter().map(|t| t.as_str()).collect();

    let mut fk_cols: HashMap<&str, HashSet<&str>> = HashMap::new();
    for fk in &schema.fks {
        fk_cols.entry(&fk.table).or_default().insert(&fk.column);
    }
    let auto_collapse = opts.focus.is_none() && visible.len() > COLLAPSE_THRESHOLD;

    let nodes = visible
        .iter()
        .map(|t| {
            let pks: HashSet<&str> = schema
                .pk_by_table
                .get(*t)
                .map(|v| v.iter().map(String::as_str).collect())
                .unwrap_or_default();
            let fks = fk_cols.get(t.as_str());
            let all: Vec<ErdColumn> = columns
                .get(*t)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|c| ErdColumn {
                    name: c.name.clone(),
                    data_type: c.data_type.clone(),
                    is_pk: pks.contains(c.name.as_str()),
                    is_fk: fks.is_some_and(|s| s.contains(c.name.as_str())),
                })
                .collect();
            let collapsed = opts.collapsed.get(*t).copied().unwrap_or(auto_collapse);
            let cols: Vec<ErdColumn> = if collapsed {
                all.iter().filter(|c| c.is_pk || c.is_fk).cloned().collect()
            } else if opts.full.get(*t).copied().unwrap_or(false) {
                all.clone()
            } else {
                all.iter().take(ROW_CAP).cloned().collect()
            };
            ErdNode {
                table: (*t).clone(),
                hidden_count: all.len() - cols.len(),
                columns: cols,
                collapsed,
                is_focus: opts.focus == Some(t.as_str()),
            }
        })
        .collect();

    // 同约束的外键行合并成一条逻辑边(约束名加表名兜底:mysql 不同表可重名约束)
    let mut groups: BTreeMap<(String, String), Vec<&ForeignKey>> = BTreeMap::new();
    let mut order: Vec<(String, String)> = Vec::new();
    for fk in &schema.fks {
        let key = (fk.table.clone(), fk.constraint.clone());
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(fk);
    }
    let edges = order
        .into_iter()
        .filter_map(|key| {
            let group = &groups[&key];
            let first = group[0];
            if !visible_set.contains(first.table.as_str())
                || !visible_set.contains(first.ref_table.as_str())
            {
                return None;
            }
            Some(ErdEdge {
                id: format!("{}|{}", first.table, first.constraint),
                source: first.table.clone(),
                target: first.ref_table.clone(),
                label: group
                    .iter()
                    .map(|f| {
                        format!(
                            "{} → {}",
                            f.column,
                            if f.ref_column.is_empty() {
                                "(pk)"
                            } else {
                                &f.ref_column
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                inferred: first.inferred,
            })
        })
        .collect();
    ErdGraph { nodes, edges }
}

/// 关系图搜索:大小写不敏感地匹配表名/列名/外键约束名,返回命中的表集合。
pub fn match_tables(
    query: &str,
    tables: &[String],
    columns: &ColumnsByTable,
    fks: &[ForeignKey],
) -> BTreeSet<String> {
    let q = query.trim().to_lowercase();
    let mut hits = BTreeSet::new();
    if q.is_empty() {
        return hits;
    }
    for t in tables {
        if t.to_lowercase().contains(&q)
            || columns
                .get(t)
                .is_some_and(|cs| cs.iter().any(|c| c.name.to_lowercase().contains(&q)))
        {
            hits.insert(t.clone());
        }
    }
    for fk in fks {
        if fk.constraint.to_lowercase().contains(&q) {
            for t in [&fk.table, &fk.ref_table] {
                if tables.contains(t) {
                    hits.insert(t.clone());
                }
            }
        }
    }
    hits
}

/// 估算节点渲染尺寸(等宽字体约 7px/字符)。
pub fn estimate_node_size(n: &ErdNode) -> (f32, f32) {
    let mut width = n.table.chars().count() as f32 * 8. + 60.;
    for c in &n.columns {
        width =
            width.max((c.name.chars().count() + c.data_type.chars().count() + 2) as f32 * 7. + 56.);
    }
    let width = width.clamp(NODE_MIN_W, NODE_MAX_W);
    let rows = n.columns.len().max(1) + usize::from(n.hidden_count > 0);
    let height = NODE_HEADER_H + rows as f32 * NODE_ROW_H + NODE_PAD_Y * 2.;
    (width, height)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl NodeRect {
    pub fn center_y(&self) -> f32 {
        self.y + self.h / 2.
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
}

/// 分层布局(方向:引用方在左,被引用表在右)。
///
/// 1. 有边的节点按"最长路径"分层(先用 DFS 去掉回边);
/// 2. 层内按重心法迭代排序,减少交叉;
/// 3. 层按列排开、层内纵向堆叠并居中;
/// 4. 没有任何关系的孤立表在下方按网格排布。
pub fn layout(graph: &ErdGraph) -> HashMap<String, NodeRect> {
    let sizes: HashMap<&str, (f32, f32)> = graph
        .nodes
        .iter()
        .map(|n| (n.table.as_str(), estimate_node_size(n)))
        .collect();
    let ids: Vec<&str> = graph.nodes.iter().map(|n| n.table.as_str()).collect();
    let mut out: HashMap<String, NodeRect> = HashMap::new();

    let mut succ: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut connected: HashSet<&str> = HashSet::new();
    for e in &graph.edges {
        if e.source == e.target {
            connected.insert(&e.source);
            continue;
        }
        succ.entry(&e.source).or_default().push(&e.target);
        connected.insert(&e.source);
        connected.insert(&e.target);
    }

    // 去回边:DFS 标记,只保留指向未完成节点之外的边
    let mut dag: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut state: HashMap<&str, u8> = HashMap::new(); // 1 = 访问中, 2 = 完成
    fn dfs<'a>(
        v: &'a str,
        succ: &HashMap<&'a str, Vec<&'a str>>,
        state: &mut HashMap<&'a str, u8>,
        dag: &mut HashMap<&'a str, Vec<&'a str>>,
    ) {
        state.insert(v, 1);
        for &w in succ.get(v).map(Vec::as_slice).unwrap_or_default() {
            match state.get(w).copied().unwrap_or(0) {
                1 => {} // 回边,丢弃
                0 => {
                    dag.entry(v).or_default().push(w);
                    dfs(w, succ, state, dag);
                }
                _ => dag.entry(v).or_default().push(w),
            }
        }
        state.insert(v, 2);
    }
    for &v in &ids {
        if connected.contains(v) && !state.contains_key(v) {
            dfs(v, &succ, &mut state, &mut dag);
        }
    }

    // 最长路径分层:layer(v) = max(layer(u) + 1),u → v
    let mut layer: HashMap<&str, usize> = ids
        .iter()
        .filter(|v| connected.contains(*v))
        .map(|v| (*v, 0))
        .collect();
    let mut changed = true;
    let mut guard = 0;
    while changed && guard < ids.len() + 1 {
        changed = false;
        guard += 1;
        for (&u, ws) in &dag {
            let lu = layer[u];
            for &w in ws {
                if layer[w] < lu + 1 {
                    layer.insert(w, lu + 1);
                    changed = true;
                }
            }
        }
    }

    let layer_count = layer.values().copied().max().map_or(0, |m| m + 1);
    let _ = &layer_count;
    let mut layers: Vec<Vec<&str>> = vec![Vec::new(); layer_count];
    for &v in &ids {
        if let Some(&l) = layer.get(v) {
            layers[l].push(v);
        }
    }

    // 重心法排序:正反各扫几轮
    let mut neighbors: HashMap<&str, Vec<&str>> = HashMap::new();
    for (&u, ws) in &dag {
        for &w in ws {
            neighbors.entry(u).or_default().push(w);
            neighbors.entry(w).or_default().push(u);
        }
    }
    for sweep in 0..6 {
        let pos: HashMap<&str, f32> = layers
            .iter()
            .flat_map(|l| l.iter().enumerate().map(|(i, v)| (*v, i as f32)))
            .collect();
        let range: Vec<usize> = if sweep % 2 == 0 {
            (1..layers.len()).collect()
        } else {
            (0..layers.len().saturating_sub(1)).rev().collect()
        };
        for li in range {
            let mut keyed: Vec<(f32, usize, &str)> = layers[li]
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let ns: Vec<f32> = neighbors
                        .get(v)
                        .map(|ns| {
                            ns.iter()
                                .filter_map(|n| {
                                    let nl = layer[n];
                                    let adjacent = if sweep % 2 == 0 {
                                        nl + 1 == li
                                    } else {
                                        nl == li + 1
                                    };
                                    adjacent.then(|| pos[n])
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let key = if ns.is_empty() {
                        i as f32
                    } else {
                        ns.iter().sum::<f32>() / ns.len() as f32
                    };
                    (key, i, *v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(a.1.cmp(&b.1)));
            layers[li] = keyed.into_iter().map(|(_, _, v)| v).collect();
        }
    }

    // 坐标:层内纵向堆叠并整体居中。某层过高(例如几十张表都引用同一张中心表)时
    // 折成多列,避免整图变成一条细长竖条、适配视野后缩到看不清。
    let total_area: f32 = layers
        .iter()
        .flatten()
        .map(|v| (sizes[v].0 + LAYER_GAP) * (sizes[v].1 + NODE_GAP))
        .sum();
    let tallest_node = layers
        .iter()
        .flatten()
        .map(|v| sizes[v].1)
        .fold(0., f32::max);
    let target_h = (total_area.sqrt() * 0.8).max(900.).max(tallest_node);
    let columns: Vec<Vec<Vec<&str>>> = layers
        .iter()
        .map(|l| {
            let mut cols: Vec<Vec<&str>> = vec![vec![]];
            let mut h = 0.;
            for v in l {
                let nh = sizes[v].1;
                if h > 0. && h + nh > target_h {
                    cols.push(vec![]);
                    h = 0.;
                }
                cols.last_mut().unwrap().push(*v);
                h += nh + NODE_GAP;
            }
            cols
        })
        .collect();
    let col_height = |c: &Vec<&str>| {
        c.iter().map(|v| sizes[v].1).sum::<f32>() + NODE_GAP * c.len().saturating_sub(1) as f32
    };
    let max_h = columns.iter().flatten().map(col_height).fold(0., f32::max);
    let mut x = 0.;
    for cols in &columns {
        for (ci, col) in cols.iter().enumerate() {
            let w = col.iter().map(|v| sizes[v].0).fold(0., f32::max);
            let mut y = (max_h - col_height(col)) / 2.;
            for v in col {
                let (nw, nh) = sizes[v];
                out.insert(v.to_string(), NodeRect { x, y, w: nw, h: nh });
                y += nh + NODE_GAP;
            }
            // 同层的折行列之间用较小间距,层与层之间保留连线空间
            x += w + if ci + 1 < cols.len() {
                NODE_GAP
            } else {
                LAYER_GAP
            };
        }
    }

    // 孤立表:网格排布在关系区下方
    let isolated: Vec<&str> = ids
        .iter()
        .copied()
        .filter(|v| !connected.contains(v))
        .collect();
    if !isolated.is_empty() {
        let cols = ((isolated.len() as f32).sqrt().ceil() as usize)
            .max(1)
            .max(if layer_count > 0 { 1 } else { 1 });
        let cell_w = isolated.iter().map(|v| sizes[v].0).fold(0., f32::max) + NODE_GAP;
        let top = if layer_count > 0 {
            max_h + LAYER_GAP
        } else {
            0.
        };
        let mut row_y = top;
        for chunk in isolated.chunks(cols) {
            let row_h = chunk.iter().map(|v| sizes[v].1).fold(0., f32::max);
            for (i, v) in chunk.iter().enumerate() {
                let (nw, nh) = sizes[v];
                out.insert(
                    v.to_string(),
                    NodeRect {
                        x: i as f32 * cell_w,
                        y: row_y,
                        w: nw,
                        h: nh,
                    },
                );
            }
            row_y += row_h + NODE_GAP;
        }
    }
    out
}

/// 局部重叠消解:保持整体排布,只把互相压住的节点沿最小穿透方向推开。
/// `fixed` 指定的节点(刚展开长高的表)固定不动;两个可动节点相撞时各让一半。
pub fn resolve_overlaps(rects: &mut HashMap<String, NodeRect>, fixed: Option<&str>) {
    const MARGIN: f32 = 28.;
    let ids: Vec<String> = {
        let mut v: Vec<String> = rects.keys().cloned().collect();
        v.sort();
        v
    };
    for _ in 0..40 {
        let mut moved = false;
        for i in 0..ids.len() {
            for j in i + 1..ids.len() {
                let (a, b) = (rects[&ids[i]], rects[&ids[j]]);
                let px = (a.w + b.w) / 2. + MARGIN - ((a.x + a.w / 2.) - (b.x + b.w / 2.)).abs();
                let py = (a.h + b.h) / 2. + MARGIN - ((a.y + a.h / 2.) - (b.y + b.h / 2.)).abs();
                if px <= 0. || py <= 0. {
                    continue;
                }
                let (a_fixed, b_fixed) = (
                    fixed == Some(ids[i].as_str()),
                    fixed == Some(ids[j].as_str()),
                );
                if a_fixed && b_fixed {
                    continue;
                }
                moved = true;
                let horizontal = px < py;
                let (pen, sign) = if horizontal {
                    (
                        px,
                        if a.x + a.w / 2. <= b.x + b.w / 2. {
                            1.
                        } else {
                            -1.
                        },
                    )
                } else {
                    (
                        py,
                        if a.y + a.h / 2. <= b.y + b.h / 2. {
                            1.
                        } else {
                            -1.
                        },
                    )
                };
                let (da, db) = if a_fixed {
                    (0., sign * pen)
                } else if b_fixed {
                    (-sign * pen, 0.)
                } else {
                    (-sign * pen / 2., sign * pen / 2.)
                };
                let ra = rects.get_mut(&ids[i]).unwrap();
                if horizontal {
                    ra.x += da
                } else {
                    ra.y += da
                }
                let rb = rects.get_mut(&ids[j]).unwrap();
                if horizontal {
                    rb.x += db
                } else {
                    rb.y += db
                }
            }
        }
        if !moved {
            break;
        }
    }
}

/// 直角折线路由:源表右侧中点 → 目标表左侧中点;目标在左侧时绕行。
pub fn route_edge(src: &NodeRect, dst: &NodeRect) -> Vec<(f32, f32)> {
    let (sx, sy) = (src.right(), src.y + NODE_HEADER_H / 2.);
    let (tx, ty) = (dst.x, dst.y + NODE_HEADER_H / 2.);
    if tx >= sx + 24. {
        let mid = (sx + tx) / 2.;
        vec![(sx, sy), (mid, sy), (mid, ty), (tx, ty)]
    } else {
        // 回绕:先向右伸出,再绕到目标左侧
        let out_x = sx + 20.;
        let in_x = tx - 20.;
        let bend_y = if (sy - ty).abs() < 1. {
            src.y.min(dst.y) - 24.
        } else {
            (src.y + src.h).max(dst.y + dst.h) + 24.
        };
        vec![
            (sx, sy),
            (out_x, sy),
            (out_x, bend_y),
            (in_x, bend_y),
            (in_x, ty),
            (tx, ty),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ColumnBrief;

    fn cols(spec: &[(&str, &[&str])]) -> ColumnsByTable {
        spec.iter()
            .map(|(t, cs)| {
                (
                    t.to_string(),
                    cs.iter()
                        .map(|c| ColumnBrief {
                            name: c.to_string(),
                            data_type: "int".into(),
                        })
                        .collect(),
                )
            })
            .collect()
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn infers_naming_convention_keys() {
        let tables = names(&["users", "orders", "categories", "posts"]);
        let columns = cols(&[
            ("users", &["id"]),
            ("orders", &["id", "user_id"]),
            ("categories", &["id"]),
            ("posts", &["id", "category_id", "orders_id", "missing_id"]),
        ]);
        let fks = infer_foreign_keys(&tables, &columns, &[]);
        let pairs: Vec<(String, String)> = fks
            .iter()
            .map(|f| (f.column.clone(), f.ref_table.clone()))
            .collect();
        assert!(pairs.contains(&("user_id".into(), "users".into())));
        assert!(pairs.contains(&("category_id".into(), "categories".into())));
        assert!(pairs.contains(&("orders_id".into(), "orders".into())));
        assert!(!pairs.iter().any(|p| p.0 == "missing_id"));
    }

    #[test]
    fn composite_fk_merges_into_one_edge_and_focus_filters() {
        let tables = names(&["parent", "child", "other"]);
        let columns = cols(&[
            ("parent", &["x", "y"]),
            ("child", &["a", "b"]),
            ("other", &["id"]),
        ]);
        let fk = |c: &str, r: &str| ForeignKey {
            constraint: "fk_comp".into(),
            table: "child".into(),
            column: c.into(),
            ref_table: "parent".into(),
            ref_column: r.into(),
            inferred: false,
        };
        let schema = ErdSchema {
            fks: vec![fk("a", "x"), fk("b", "y")],
            pk_by_table: Default::default(),
        };
        let empty = HashMap::new();
        let g = build_graph(
            &tables,
            &columns,
            &schema,
            &GraphOptions {
                focus: Some("parent"),
                collapsed: &empty,
                full: &empty,
            },
        );
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].label, "a → x\nb → y");
    }

    #[test]
    fn layout_places_referenced_tables_to_the_right() {
        let tables = names(&["orders", "users", "lonely"]);
        let columns = cols(&[
            ("orders", &["id", "user_id"]),
            ("users", &["id"]),
            ("lonely", &["id"]),
        ]);
        let schema = ErdSchema {
            fks: vec![ForeignKey {
                constraint: "fk".into(),
                table: "orders".into(),
                column: "user_id".into(),
                ref_table: "users".into(),
                ref_column: "id".into(),
                inferred: false,
            }],
            pk_by_table: Default::default(),
        };
        let empty = HashMap::new();
        let g = build_graph(
            &tables,
            &columns,
            &schema,
            &GraphOptions {
                focus: None,
                collapsed: &empty,
                full: &empty,
            },
        );
        let pos = layout(&g);
        assert!(pos["users"].x > pos["orders"].x);
        assert!(pos["lonely"].y > pos["orders"].y);
    }

    #[test]
    fn overlaps_are_pushed_apart_keeping_fixed_node() {
        let mut rects = HashMap::new();
        rects.insert(
            "a".to_string(),
            NodeRect {
                x: 0.,
                y: 0.,
                w: 100.,
                h: 100.,
            },
        );
        rects.insert(
            "b".to_string(),
            NodeRect {
                x: 50.,
                y: 10.,
                w: 100.,
                h: 100.,
            },
        );
        resolve_overlaps(&mut rects, Some("a"));
        assert_eq!(rects["a"].x, 0.);
        assert!(rects["b"].x >= 128. || rects["b"].y >= 128.);
    }
}
