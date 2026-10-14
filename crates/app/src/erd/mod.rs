//! 关系图(ER 图)弹窗:整库或以某表为中心的结构图。
//!
//! 布局在 `datum_core::erd`(分层布局 + 局部推挤);这里负责绘制与交互:
//! 平移(拖背景 / 滚轮)、缩放(⌘ + 滚轮 / 按钮)、拖动节点、点击高亮关联、
//! 双击打开表、搜索高亮、小地图与导出 PNG。

mod export;

use crate::runtime;
use crate::store::{Stores, Toast};
use crate::theme::ActivePalette as _;
use crate::ui;
use crate::workspace::{Workspace, show_toast};
use datum_core::core::connection::DbKind;
use datum_core::erd::{
    self, COLLAPSE_THRESHOLD, ErdGraph, GraphOptions, NODE_HEADER_H, NODE_PAD_Y, NODE_ROW_H,
    NodeRect,
};
use datum_core::schema::ErdSchema;
use datum_core::service;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, AsyncWindowContext, Bounds, ClickEvent, Context, Entity, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, PathBuilder, Pixels, Point, Render, ScrollWheelEvent,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity,
    Window, canvas, div, point, px,
};
use rust_i18n::t;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::time::Duration;

/// (表名, 表→列, 外键含推断)
type ErdData = (Vec<String>, datum_core::schema::ColumnsByTable, ErdSchema);

const MIN_ZOOM: f32 = 0.1;
/// 低于该缩放只画表名色块
const LOD_SIMPLE: f32 = 0.45;
/// 自动适配时的最小缩放:保证字段文字可读(细节层级之上)
const FIT_READABLE: f32 = 0.5;
const MAX_ZOOM: f32 = 2.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    /// 表结构图:全部表完整字段
    Struct,
    /// 工程 ER 图:全部折叠,只显键列
    Er,
}

enum Drag {
    Pan {
        start: Point<Pixels>,
        pan: Point<f32>,
    },
    Node {
        table: String,
        start: Point<Pixels>,
        pos: (f32, f32),
        moved: bool,
    },
}

pub struct ErdView {
    workspace: WeakEntity<Workspace>,
    conn_id: Option<String>,
    /// 聚焦表(单表结构图);None = 整库
    focus: Option<String>,
    search: Entity<InputState>,
    collapsed: HashMap<String, bool>,
    full: HashMap<String, bool>,
    selected: Option<String>,
    show_inferred: bool,
    view_mode: Option<ViewMode>,
    graph: ErdGraph,
    pos: HashMap<String, NodeRect>,
    zoom: f32,
    pan: Point<f32>,
    drag: Option<Drag>,
    loading: bool,
    error: Option<String>,
    exporting: bool,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// 下一帧按画布尺寸适配视野(None = 全图,Some = 指定表集合)
    pending_fit: Option<Option<BTreeSet<String>>>,
    data_cache: RefCell<Option<Rc<ErdData>>>,
    hits_cache: RefCell<Option<(String, BTreeSet<String>)>>,
    _subscriptions: Vec<Subscription>,
    _task: Option<Task<()>>,
    _search_fit: Option<Task<()>>,
}

impl ErdView {
    fn new(
        conn_id: Option<String>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("erd.search_placeholder")));
        let conns = Stores::global(cx).conns.clone();
        let subs = vec![
            cx.subscribe(&search, |this, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    this.schedule_search_fit(cx);
                    cx.notify();
                }
            }),
            // 表/列/外键数据到达后重建图
            cx.observe(&conns, |this, _, cx| {
                this.invalidate();
                this.rebuild(true, cx)
            }),
        ];
        let mut this = Self {
            workspace,
            conn_id: None,
            focus: None,
            search,
            collapsed: HashMap::new(),
            full: HashMap::new(),
            selected: None,
            show_inferred: true,
            view_mode: None,
            graph: ErdGraph::default(),
            pos: HashMap::new(),
            zoom: 1.,
            pan: point(0., 0.),
            drag: None,
            loading: false,
            error: None,
            exporting: false,
            bounds: Rc::new(Cell::new(Bounds::default())),
            pending_fit: None,
            data_cache: RefCell::new(None),
            hits_cache: RefCell::new(None),
            _subscriptions: subs,
            _task: None,
            _search_fit: None,
        };
        if let Some(id) = conn_id {
            this.pick_conn(id, cx);
        }
        this
    }

    fn pick_conn(&mut self, id: String, cx: &mut Context<Self>) {
        self.invalidate();
        self.conn_id = Some(id.clone());
        self.focus = None;
        self.reset_view_state();
        let conns = Stores::global(cx).conns.clone();
        conns.update(cx, |s, cx| {
            if !s.connected.contains(&id) {
                s.load_tables(&id, cx).detach();
            }
        });
        if conns.read(cx).erd.contains_key(&id) {
            self.rebuild(false, cx);
        } else {
            self.load_schema(cx);
        }
        cx.notify();
    }

    fn reset_view_state(&mut self) {
        self.selected = None;
        self.collapsed.clear();
        self.full.clear();
        self.view_mode = None;
        self.pos.clear();
    }

    /// 拉取整库外键 + 主键并写回缓存(同连接再次打开秒开)。
    fn load_schema(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.conn_id.clone() else {
            return;
        };
        let Some(kind) = Stores::global(cx).conns.read(cx).get(&id).map(|c| c.kind) else {
            return;
        };
        if kind == DbKind::Redis {
            return;
        }
        self.loading = true;
        self.error = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let task_id = id.clone();
            let result = runtime::backend(move |s| async move {
                service::connect(&s, &kind, &task_id).await?;
                service::fetch_erd_schema(&s, &kind, &task_id).await
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                if this.conn_id.as_deref() != Some(id.as_str()) {
                    return; // 期间切换了库:丢弃过期结果
                }
                match result {
                    Ok(schema) => {
                        let conns = Stores::global(cx).conns.clone();
                        conns.update(cx, |s, cx| {
                            s.erd.insert(id.clone(), schema);
                            cx.notify();
                        });
                        this.rebuild(false, cx);
                    }
                    Err(e) => this.error = Some(e.display()),
                }
                cx.notify();
            });
        }));
    }

    /// 表、列、外键(含推断)的快照。推断要扫全部列,结果缓存到数据或开关变化为止,
    /// 渲染路径上只取 Rc,不再每帧克隆整库结构。
    fn data(&self, cx: &gpui_kit::App) -> Option<Rc<ErdData>> {
        if let Some(d) = self.data_cache.borrow().as_ref() {
            return Some(d.clone());
        }
        let d = Rc::new(self.compute_data(cx)?);
        *self.data_cache.borrow_mut() = Some(d.clone());
        Some(d)
    }

    fn invalidate(&self) {
        self.data_cache.borrow_mut().take();
        self.hits_cache.borrow_mut().take();
    }

    fn compute_data(&self, cx: &gpui_kit::App) -> Option<ErdData> {
        let id = self.conn_id.as_ref()?;
        let store = Stores::global(cx).conns.read(cx);
        let schema = store.erd.get(id)?.clone();
        let tables = store.tables.get(id).cloned().unwrap_or_default();
        let columns = store.columns.get(id).cloned().unwrap_or_default();
        let schema = if self.show_inferred {
            let inferred = erd::infer_foreign_keys(&tables, &columns, &schema.fks);
            ErdSchema {
                fks: schema.fks.iter().cloned().chain(inferred).collect(),
                ..schema
            }
        } else {
            schema
        };
        Some((tables, columns, schema))
    }

    /// 重建图。`keep_positions`:数据增量到达时保留已有坐标(新节点才重新布局)。
    fn rebuild(&mut self, keep_positions: bool, cx: &mut Context<Self>) {
        let Some(data) = self.data(cx) else {
            cx.notify();
            return;
        };
        let (tables, columns, schema) = (&data.0, &data.1, &data.2);
        let graph = erd::build_graph(
            tables,
            columns,
            schema,
            &GraphOptions {
                focus: self.focus.as_deref(),
                collapsed: &self.collapsed,
                full: &self.full,
            },
        );
        let same_nodes = keep_positions
            && graph.nodes.len() == self.pos.len()
            && graph.nodes.iter().all(|n| self.pos.contains_key(&n.table));
        if !same_nodes {
            self.pos = erd::layout(&graph);
            self.pending_fit = Some(None);
        } else {
            // 尺寸可能变了(列数据到达):按新尺寸就地推挤
            for n in &graph.nodes {
                let (w, h) = erd::estimate_node_size(n);
                if let Some(r) = self.pos.get_mut(&n.table) {
                    r.w = w;
                    r.h = h;
                }
            }
            erd::resolve_overlaps(&mut self.pos, None);
        }
        if self.view_mode.is_none()
            && self.focus.is_none()
            && tables.len() > COLLAPSE_THRESHOLD
            && self.collapsed.is_empty()
        {
            self.view_mode = Some(ViewMode::Er);
        }
        self.graph = graph;
        cx.notify();
    }

    /// 「+N 个字段」:一次直达该表完整字段。展开不整图重排(会把变大的节点挪走),
    /// 而是就地长高 + 推开被压住的邻居。
    fn expand(&mut self, table: &str, cx: &mut Context<Self>) {
        self.collapsed.insert(table.to_string(), false);
        self.full.insert(table.to_string(), true);
        let Some(data) = self.data(cx) else {
            return;
        };
        let (tables, columns, schema) = (&data.0, &data.1, &data.2);
        self.graph = erd::build_graph(
            tables,
            columns,
            schema,
            &GraphOptions {
                focus: self.focus.as_deref(),
                collapsed: &self.collapsed,
                full: &self.full,
            },
        );
        for n in &self.graph.nodes {
            let (w, h) = erd::estimate_node_size(n);
            if let Some(r) = self.pos.get_mut(&n.table) {
                r.w = w;
                r.h = h;
            }
        }
        erd::resolve_overlaps(&mut self.pos, Some(table));
        cx.notify();
    }

    fn set_view(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        let tables = self
            .conn_id
            .as_ref()
            .and_then(|id| Stores::global(cx).conns.read(cx).tables.get(id).cloned())
            .unwrap_or_default();
        self.view_mode = Some(mode);
        self.collapsed = tables
            .iter()
            .map(|t| (t.clone(), mode == ViewMode::Er))
            .collect();
        self.full = if mode == ViewMode::Struct {
            tables.iter().map(|t| (t.clone(), true)).collect()
        } else {
            HashMap::new()
        };
        self.pos.clear();
        self.rebuild(false, cx);
    }

    fn set_focus(&mut self, focus: Option<String>, cx: &mut Context<Self>) {
        self.focus = focus;
        self.reset_view_state();
        self.rebuild(false, cx);
    }

    fn hits(&self, cx: &gpui_kit::App) -> Option<BTreeSet<String>> {
        let q = self.search.read(cx).value().trim().to_string();
        if q.is_empty() {
            return None;
        }
        if let Some((key, hits)) = self.hits_cache.borrow().as_ref() {
            if *key == q {
                return Some(hits.clone());
            }
        }
        let data = self.data(cx)?;
        let hits = erd::match_tables(&q, &data.0, &data.1, &data.2.fks);
        *self.hits_cache.borrow_mut() = Some((q, hits.clone()));
        Some(hits)
    }

    /// 搜索命中集变化 → 视野聚焦到命中节点(防抖,避免逐字输入时视口乱跳)。
    fn schedule_search_fit(&mut self, cx: &mut Context<Self>) {
        self._search_fit = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(hits) = this.hits(cx).filter(|h| !h.is_empty()) {
                    this.pending_fit = Some(Some(hits));
                    cx.notify();
                }
            });
        }));
    }

    /// 把视野适配到给定表(None = 全部),最大缩放 1。
    /// `min_zoom`:打开/搜索时用可读下限(放不下就从左上角开始看),
    /// 工具栏的「适配视野」按钮用 MIN_ZOOM 看全貌。
    fn fit(&mut self, only: Option<&BTreeSet<String>>, min_zoom: f32) {
        let b = self.bounds.get();
        let (vw, vh) = (f32::from(b.size.width), f32::from(b.size.height));
        if vw <= 0. || vh <= 0. {
            return;
        }
        let rects: Vec<&NodeRect> = self
            .pos
            .iter()
            .filter(|(t, _)| only.is_none_or(|o| o.contains(*t)))
            .map(|(_, r)| r)
            .collect();
        if rects.is_empty() {
            return;
        }
        let (x0, y0) = rects
            .iter()
            .fold((f32::MAX, f32::MAX), |(x, y), r| (x.min(r.x), y.min(r.y)));
        let (x1, y1) = rects.iter().fold((f32::MIN, f32::MIN), |(x, y), r| {
            (x.max(r.x + r.w), y.max(r.y + r.h))
        });
        let pad = 0.15;
        let zoom = ((vw * (1. - pad)) / (x1 - x0))
            .min((vh * (1. - pad)) / (y1 - y0))
            .clamp(MIN_ZOOM, 1.);
        let zoom = zoom.max(min_zoom.min(1.));
        self.zoom = zoom;
        let (gw, gh) = ((x1 - x0) * zoom, (y1 - y0) * zoom);
        const INSET: f32 = 24.;
        // 放得下的方向居中,放不下的方向从起始边开始
        let pan_x = if gw <= vw - INSET * 2. {
            vw / 2. - (x0 + x1) / 2. * zoom
        } else {
            INSET - x0 * zoom
        };
        let pan_y = if gh <= vh - INSET * 2. {
            vh / 2. - (y0 + y1) / 2. * zoom
        } else {
            INSET - y0 * zoom
        };
        self.pan = point(pan_x, pan_y);
    }

    fn zoom_at(&mut self, factor: f32, anchor: (f32, f32)) {
        let new = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let k = new / self.zoom;
        self.pan = point(
            anchor.0 - (anchor.0 - self.pan.x) * k,
            anchor.1 - (anchor.1 - self.pan.y) * k,
        );
        self.zoom = new;
    }

    fn local(&self, p: Point<Pixels>) -> (f32, f32) {
        let o = self.bounds.get().origin;
        (f32::from(p.x - o.x), f32::from(p.y - o.y))
    }

    fn open_table(&mut self, table: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conn_id) = self.conn_id.clone() else {
            return;
        };
        let table = table.to_string();
        window.close_dialog(cx);
        let _ = self.workspace.update(cx, |ws, cx| {
            ws.open_table(&conn_id, &table, window, cx);
        });
    }

    fn export_png(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pos.is_empty() {
            return;
        }
        let svg = export::to_svg(&self.graph, &self.pos, cx);
        let name = format!(
            "erd_{}_{}.png",
            self.focus.clone().unwrap_or_else(|| "all".into()),
            datum_core::grid::file_timestamp()
        );
        let label = match self.view_mode {
            Some(ViewMode::Er) => t!("erd.exported_er"),
            Some(ViewMode::Struct) => t!("erd.exported_struct"),
            None => t!("erd.exported_diagram"),
        }
        .to_string();
        self.exporting = true;
        cx.notify();
        let png = runtime::spawn(async move { export::rasterize(&svg) });
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let result = png.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.exporting = false;
                match result {
                    Ok(bytes) => crate::workspace::save_bytes(name, bytes, label, window, cx),
                    Err(e) => show_toast(
                        &Toast::Error(t!("erd.export_failed", error = e).to_string().into()),
                        window,
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------ 渲染

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *cx.palette();
        let inferred = self.graph.edges.iter().filter(|e| e.inferred).count();
        let stats = if self.graph.nodes.is_empty() && self.loading {
            "…".to_string()
        } else {
            let base = t!(
                "erd.stats",
                tables = self.graph.nodes.len(),
                edges = self.graph.edges.len()
            );
            if inferred > 0 {
                format!("{base}{}", t!("erd.stats_inferred", n = inferred))
            } else {
                base.to_string()
            }
        };
        let no_edges = !self.loading
            && self.error.is_none()
            && !self.graph.nodes.is_empty()
            && self.graph.edges.is_empty();
        h_flex()
            .h(px(36.))
            .flex_shrink_0()
            .px_3p5()
            .gap_2p5()
            .bg(palette.panel)
            .border_b_1()
            .border_color(cx.theme().border)
            .when(self.focus.is_some(), |this| {
                this.child(
                    Button::new("erd-back")
                        .outline()
                        .xsmall()
                        .icon(ui::icon("undo-2"))
                        .label(t!("erd.back_to_all"))
                        .on_click(cx.listener(|this, _, _, cx| this.set_focus(None, cx))),
                )
            })
            .child(
                div()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(palette.fg_faint)
                    .child(stats),
            )
            .when(no_edges, |this| {
                this.child(div().text_xs().text_color(palette.fg_faint).child(
                    if self.focus.is_some() {
                        t!("erd.no_edges_table")
                    } else {
                        t!("erd.no_edges_db")
                    },
                ))
            })
            .child(div().flex_1())
            .when(self.loading, |this| {
                this.child(
                    ui::icon("loader-circle")
                        .small()
                        .text_color(palette.fg_faint),
                )
            })
            .child(
                Button::new("erd-inferred")
                    .outline()
                    .xsmall()
                    .selected(self.show_inferred)
                    .label(t!("erd.inferred"))
                    .tooltip(t!("erd.inferred_tooltip"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_inferred = !this.show_inferred;
                        this.invalidate();
                        this.pos.clear();
                        this.rebuild(false, cx);
                    })),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("erd-struct")
                            .outline()
                            .xsmall()
                            .selected(self.view_mode == Some(ViewMode::Struct))
                            .label(t!("erd.view_struct"))
                            .disabled(self.graph.nodes.is_empty())
                            .on_click(
                                cx.listener(|this, _, _, cx| this.set_view(ViewMode::Struct, cx)),
                            ),
                    )
                    .child(
                        Button::new("erd-er")
                            .outline()
                            .xsmall()
                            .selected(self.view_mode == Some(ViewMode::Er))
                            .label(t!("erd.view_er"))
                            .disabled(self.graph.nodes.is_empty())
                            .on_click(
                                cx.listener(|this, _, _, cx| this.set_view(ViewMode::Er, cx)),
                            ),
                    ),
            )
            .child(
                Button::new("erd-export")
                    .outline()
                    .xsmall()
                    .icon(ui::icon("image-down"))
                    .label(if self.exporting {
                        t!("erd.exporting")
                    } else {
                        t!("erd.export")
                    })
                    .loading(self.exporting)
                    .disabled(self.exporting || self.pos.is_empty())
                    .tooltip(t!("erd.export_tooltip"))
                    .on_click(cx.listener(|this, _, window, cx| this.export_png(window, cx))),
            )
            .child(
                Button::new("erd-refresh")
                    .outline()
                    .xsmall()
                    .icon(ui::icon("refresh-cw"))
                    .tooltip(t!("erd.refresh_tooltip"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pos.clear();
                        this.load_schema(cx);
                    })),
            )
    }

    fn node_state(
        &self,
        table: &str,
        hits: &Option<BTreeSet<String>>,
        keep: &Option<BTreeSet<String>>,
    ) -> (bool, bool) {
        // (高亮, 退后);搜索态优先于点击选中态
        match hits {
            Some(h) => (h.contains(table), !h.contains(table)),
            None => match keep {
                Some(k) => (false, !k.contains(table)),
                None => (false, false),
            },
        }
    }

    fn render_node(
        &self,
        n: &erd::ErdNode,
        rect: &NodeRect,
        hit: bool,
        dim: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = *cx.palette();
        let z = self.zoom;
        let mono = cx.theme().mono_font_family.clone();
        let focus_mode = self.focus.is_some();
        let selected = self.selected.as_deref() == Some(n.table.as_str());
        let table = n.table.clone();
        let table2 = n.table.clone();
        let table3 = n.table.clone();
        let table4 = n.table.clone();
        v_flex()
            .id(SharedString::from(format!("erd-node-{}", n.table)))
            .absolute()
            .left(px(self.pan.x + rect.x * z))
            .top(px(self.pan.y + rect.y * z))
            .w(px(rect.w * z))
            .rounded(px(8. * z))
            .overflow_hidden()
            .bg(palette.panel)
            .border_1()
            .border_color(if hit || selected {
                cx.theme().primary
            } else {
                cx.theme().border
            })
            .shadow_sm()
            .when(dim, |d| d.opacity(0.25))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let pos = this.pos.get(&table).map(|r| (r.x, r.y)).unwrap_or_default();
                    this.drag = Some(Drag::Node {
                        table: table.clone(),
                        start: e.position,
                        pos,
                        moved: false,
                    });
                }),
            )
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                if e.click_count() >= 2 {
                    this.open_table(&table2, window, cx);
                }
            }))
            .map(|this| {
                // 细节层级:缩得很小时只画表名(或纯色块),不再为上千个字段行排版文字
                if z < LOD_SIMPLE {
                    // 字号有下限,超出节点宽度的部分省略
                    return this.h(px(rect.h * z)).bg(palette.raised).child(
                        div()
                            .w_full()
                            .px(px((10. * z).max(2.)))
                            .pt(px((8. * z).max(1.)))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_family(mono.clone())
                            .text_size(px((13. * z).max(9.)))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(n.table.clone()),
                    );
                }
                this.child(
                    h_flex()
                        .h(px(NODE_HEADER_H * z))
                        .px(px(10. * z))
                        .gap(px(6. * z))
                        .bg(palette.raised)
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .when(n.is_focus, |d| {
                            d.border_t_2().border_color(cx.theme().primary)
                        })
                        .child(
                            ui::icon("table-2")
                                .size(px(12. * z))
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .font_family(mono.clone())
                                .text_size(px(13. * z))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(n.table.clone()),
                        )
                        .when(focus_mode && !n.is_focus, |this| {
                            this.child(
                                div()
                                    .id(SharedString::from(format!("erd-focus-{}", n.table)))
                                    .cursor_pointer()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_focus(Some(table3.clone()), cx)
                                    }))
                                    .child(
                                        ui::icon("crosshair")
                                            .size(px(12. * z))
                                            .text_color(cx.theme().muted_foreground),
                                    ),
                            )
                        }),
                )
                .child(
                    v_flex()
                        .py(px(NODE_PAD_Y * z))
                        .children(n.columns.iter().map(|c| {
                            h_flex()
                                .h(px(NODE_ROW_H * z))
                                .px(px(10. * z))
                                .gap(px(6. * z))
                                .child(div().w(px(12. * z)).flex_shrink_0().map(|d| {
                                    if c.is_pk {
                                        d.child(
                                            ui::icon("key-round")
                                                .size(px(11. * z))
                                                .text_color(palette.data),
                                        )
                                    } else if c.is_fk {
                                        d.child(
                                            ui::icon("link-2")
                                                .size(px(11. * z))
                                                .text_color(palette.fg_faint),
                                        )
                                    } else {
                                        d
                                    }
                                }))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .font_family(mono.clone())
                                        .text_size(px(12. * z))
                                        .child(c.name.clone()),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(11. * z))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(c.data_type.clone()),
                                )
                        }))
                        .when(n.columns.is_empty(), |this| {
                            this.child(
                                h_flex()
                                    .h(px(NODE_ROW_H * z))
                                    .px(px(10. * z))
                                    .italic()
                                    .text_size(px(12. * z))
                                    .text_color(palette.fg_faint)
                                    .child(t!("erd.no_key_columns")),
                            )
                        })
                        .when(n.hidden_count > 0, |this| {
                            this.child(
                                h_flex()
                                    .id(SharedString::from(format!("erd-more-{}", n.table)))
                                    .h(px(NODE_ROW_H * z))
                                    .px(px(10. * z))
                                    .gap(px(5. * z))
                                    .cursor_pointer()
                                    .text_size(px(11. * z))
                                    .text_color(palette.fg_faint)
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.expand(&table4, cx)),
                                    )
                                    .child(ui::icon("chevron-down").size(px(11. * z)))
                                    .child(t!("erd.more_columns", n = n.hidden_count).to_string()),
                            )
                        }),
                )
            })
    }

    fn render_canvas(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *cx.palette();
        let hits = self.hits(cx);
        let keep: Option<BTreeSet<String>> = self.selected.as_ref().and_then(|s| {
            let data = self.data(cx)?;
            let mut k = erd::neighbors_of(s, &data.2.fks);
            k.insert(s.clone());
            Some(k)
        });

        // 边:屏幕坐标折线 + 颜色/虚线/高亮
        struct EdgePaint {
            points: Vec<Point<Pixels>>,
            color: Hsla,
            dashed: bool,
            width: f32,
        }
        let z = self.zoom;
        let pan = self.pan;
        let to_screen = |(x, y): (f32, f32)| point(px(pan.x + x * z), px(pan.y + y * z));
        // 视野(世界坐标,带边距):只绘制与之相交的节点和连线
        let vb = self.bounds.get();
        let margin = 40. / z;
        let (vx0, vy0) = (-pan.x / z - margin, -pan.y / z - margin);
        let (vx1, vy1) = (
            vx0 + f32::from(vb.size.width) / z + margin * 2.,
            vy0 + f32::from(vb.size.height) / z + margin * 2.,
        );
        let in_view =
            |x0: f32, y0: f32, x1: f32, y1: f32| x1 >= vx0 && x0 <= vx1 && y1 >= vy0 && y0 <= vy1;
        // 缩得很小时虚线细节看不清,却要逐段细分路径:改用实线
        let dash_ok = z >= 0.5;
        let mut labels: Vec<(Point<f32>, String)> = vec![];
        let edges: Vec<EdgePaint> = self
            .graph
            .edges
            .iter()
            .filter_map(|e| {
                let (s, t) = (self.pos.get(&e.source)?, self.pos.get(&e.target)?);
                let lit = hits.is_none()
                    && self
                        .selected
                        .as_ref()
                        .is_some_and(|sel| *sel == e.source || *sel == e.target);
                let dim = match &hits {
                    Some(h) => !(h.contains(&e.source) && h.contains(&e.target)),
                    None => self.selected.is_some() && !lit,
                };
                let route = erd::route_edge(s, t);
                let (rx0, ry0, rx1, ry1) = route.iter().fold(
                    (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
                    |(a, b, c, d), (x, y)| (a.min(*x), b.min(*y), c.max(*x), d.max(*y)),
                );
                if !in_view(rx0, ry0, rx1, ry1) {
                    return None;
                }
                if lit {
                    let mid = route[route.len() / 2 - 1];
                    labels.push((point(pan.x + mid.0 * z, pan.y + mid.1 * z), e.label.clone()));
                }
                Some(EdgePaint {
                    points: route.into_iter().map(to_screen).collect(),
                    color: if lit {
                        cx.theme().primary
                    } else if dim {
                        palette.fg_faint.opacity(0.25)
                    } else {
                        palette.fg_faint
                    },
                    dashed: e.inferred && dash_ok,
                    width: if lit { 2. } else { 1.4 },
                })
            })
            .collect();

        let bounds = self.bounds.clone();
        let this = cx.entity().downgrade();
        let dot = palette.hairline;
        let grid_pan = (pan.x, pan.y);
        let painter = canvas(
            move |b, window, cx| {
                let old = bounds.get();
                bounds.set(b);
                if old.size != b.size {
                    let this = this.clone();
                    window.defer(cx, move |_, cx| {
                        let _ = this.update(cx, |this, cx| {
                            if this.pending_fit.is_none() {
                                this.pending_fit = Some(None);
                            }
                            cx.notify();
                        });
                    });
                }
            },
            move |b, _, window, _| {
                // 点阵背景
                // 缩小时把点距翻倍,避免一帧画上万个点
                let mut gap = 22. * z;
                while gap < 16. {
                    gap *= 2.;
                }
                let (ox, oy) = (grid_pan.0.rem_euclid(gap), grid_pan.1.rem_euclid(gap));
                let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
                let mut y = oy;
                while y < h {
                    let mut x = ox;
                    while x < w {
                        window.paint_quad(gpui_kit::fill(
                            Bounds::new(
                                b.origin + point(px(x), px(y)),
                                gpui_kit::size(px(1.2), px(1.2)),
                            ),
                            dot,
                        ));
                        x += gap;
                    }
                    y += gap;
                }
                for e in edges {
                    let pts: Vec<Point<Pixels>> = e.points.iter().map(|p| b.origin + *p).collect();
                    let mut builder = PathBuilder::stroke(px(e.width));
                    if e.dashed {
                        builder = builder.dash_array(&[px(6.), px(4.)]);
                    }
                    builder.move_to(pts[0]);
                    for p in &pts[1..] {
                        builder.line_to(*p);
                    }
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, e.color);
                    }
                    // 箭头指向被引用表
                    let tip = *pts.last().unwrap();
                    let s = (7. * z).clamp(4., 9.);
                    let mut arrow = PathBuilder::fill();
                    arrow.add_polygon(
                        &[
                            tip,
                            tip - point(px(s), px(s * 0.6)),
                            tip - point(px(s), px(-s * 0.6)),
                        ],
                        true,
                    );
                    if let Ok(path) = arrow.build() {
                        window.paint_path(path, e.color);
                    }
                }
            },
        )
        .absolute()
        .size_full();

        let nodes: Vec<gpui_kit::AnyElement> = self
            .graph
            .nodes
            .iter()
            .filter_map(|n| {
                let rect = *self.pos.get(&n.table)?;
                if !in_view(rect.x, rect.y, rect.x + rect.w, rect.y + rect.h) {
                    return None;
                }
                let (hit, dim) = self.node_state(&n.table, &hits, &keep);
                Some(self.render_node(n, &rect, hit, dim, cx).into_any_element())
            })
            .collect();

        let label_els = labels.into_iter().map(|(p, text)| {
            div()
                .absolute()
                .left(px(p.x + 4.))
                .top(px(p.y - 18.))
                .px_1p5()
                .rounded(px(4.))
                .bg(cx.theme().background)
                .border_1()
                .border_color(cx.theme().primary)
                .text_size(px(11.))
                .font_family(cx.theme().mono_font_family.clone())
                .child(text)
        });

        let _ = window;
        div()
            .id("erd-canvas")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .bg(palette.editor)
            .cursor(if matches!(self.drag, Some(Drag::Pan { .. })) {
                gpui_kit::CursorStyle::ClosedHand
            } else {
                gpui_kit::CursorStyle::Arrow
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.drag = Some(Drag::Pan {
                        start: e.position,
                        pan: this.pan,
                    });
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                let z = this.zoom;
                match &mut this.drag {
                    Some(Drag::Pan { start, pan }) => {
                        let d = e.position - *start;
                        this.pan = point(pan.x + f32::from(d.x), pan.y + f32::from(d.y));
                        cx.notify();
                    }
                    Some(Drag::Node {
                        table,
                        start,
                        pos,
                        moved,
                    }) => {
                        let d = e.position - *start;
                        let (dx, dy) = (f32::from(d.x) / z, f32::from(d.y) / z);
                        if dx.abs() + dy.abs() > 2. {
                            *moved = true;
                        }
                        if let Some(r) = this.pos.get_mut(table.as_str()) {
                            r.x = pos.0 + dx;
                            r.y = pos.1 + dy;
                        }
                        cx.notify();
                    }
                    None => {}
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    match this.drag.take() {
                        Some(Drag::Node {
                            table,
                            moved: false,
                            ..
                        }) => {
                            // 点击节点:选中 / 取消选中(相连边亮起显示列配对)
                            this.selected = if this.selected.as_deref() == Some(table.as_str()) {
                                None
                            } else {
                                Some(table)
                            };
                        }
                        Some(Drag::Pan { start: _, pan })
                            if (pan.x - this.pan.x).abs() + (pan.y - this.pan.y).abs() < 2. =>
                        {
                            this.selected = None; // 点空白处取消选中
                        }
                        _ => {}
                    }
                    cx.notify();
                }),
            )
            .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
                let d = e.delta.pixel_delta(px(20.));
                if e.modifiers.platform || e.modifiers.control {
                    let anchor = this.local(e.position);
                    this.zoom_at((1. + f32::from(d.y) * 0.004).clamp(0.5, 2.), anchor);
                } else {
                    this.pan = point(this.pan.x + f32::from(d.x), this.pan.y + f32::from(d.y));
                }
                cx.notify();
            }))
            .child(painter)
            .children(nodes)
            .children(label_els)
            .child(self.render_controls(cx))
            .when(self.focus.is_none() && !self.pos.is_empty(), |this| {
                this.child(self.render_minimap(cx))
            })
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let center = |this: &Self| {
            let b = this.bounds.get();
            (f32::from(b.size.width) / 2., f32::from(b.size.height) / 2.)
        };
        v_flex()
            .absolute()
            .left_3()
            .bottom_3()
            .p_0p5()
            .rounded(cx.theme().radius)
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_sm()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new("erd-zoom-in")
                    .ghost()
                    .xsmall()
                    .icon(ui::icon("zoom-in"))
                    .tooltip(t!("erd.zoom_in"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let c = center(this);
                        this.zoom_at(1.2, c);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("erd-zoom-out")
                    .ghost()
                    .xsmall()
                    .icon(ui::icon("zoom-out"))
                    .tooltip(t!("erd.zoom_out"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let c = center(this);
                        this.zoom_at(1. / 1.2, c);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("erd-fit")
                    .ghost()
                    .xsmall()
                    .icon(ui::icon("maximize"))
                    .tooltip(t!("erd.fit_view"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.fit(None, MIN_ZOOM);
                        cx.notify();
                    })),
            )
    }

    /// 小地图:节点缩略 + 当前视口框,点击跳转。
    fn render_minimap(&self, cx: &mut Context<Self>) -> impl IntoElement {
        const W: f32 = 180.;
        const H: f32 = 120.;
        let palette = *cx.palette();
        let (x0, y0) = self
            .pos
            .values()
            .fold((f32::MAX, f32::MAX), |(x, y), r| (x.min(r.x), y.min(r.y)));
        let (x1, y1) = self.pos.values().fold((f32::MIN, f32::MIN), |(x, y), r| {
            (x.max(r.x + r.w), y.max(r.y + r.h))
        });
        let b = self.bounds.get();
        let (vw, vh) = (f32::from(b.size.width), f32::from(b.size.height));
        // 视口在世界坐标中的范围,一并纳入小地图
        let (wx0, wy0) = (-self.pan.x / self.zoom, -self.pan.y / self.zoom);
        let (wx1, wy1) = (wx0 + vw / self.zoom, wy0 + vh / self.zoom);
        let (mx0, my0, mx1, my1) = (x0.min(wx0), y0.min(wy0), x1.max(wx1), y1.max(wy1));
        let s = (W / (mx1 - mx0)).min(H / (my1 - my0));
        let map = move |x: f32, y: f32| ((x - mx0) * s, (y - my0) * s);
        let (vx, vy) = map(wx0, wy0);
        div()
            .id("erd-minimap")
            .absolute()
            .right_3()
            .bottom_3()
            .w(px(W))
            .h(px(H))
            .rounded(cx.theme().radius)
            .overflow_hidden()
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_sm()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    // 小地图左上角 = 画布右下角减去尺寸
                    let b = this.bounds.get();
                    let left = f32::from(b.origin.x + b.size.width) - 12. - W;
                    let top = f32::from(b.origin.y + b.size.height) - 12. - H;
                    let (lx, ly) = (
                        f32::from(e.position.x) - left,
                        f32::from(e.position.y) - top,
                    );
                    let (wx, wy) = (lx / s + mx0, ly / s + my0);
                    this.pan = point(
                        f32::from(b.size.width) / 2. - wx * this.zoom,
                        f32::from(b.size.height) / 2. - wy * this.zoom,
                    );
                    cx.notify();
                }),
            )
            .children(self.pos.values().map(|r| {
                let (x, y) = map(r.x, r.y);
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px((r.w * s).max(1.)))
                    .h(px((r.h * s).max(1.)))
                    .bg(palette.fg_faint.opacity(0.5))
            }))
            .child(
                div()
                    .absolute()
                    .left(px(vx))
                    .top(px(vy))
                    .w(px((vw / self.zoom * s).max(2.)))
                    .h(px((vh / self.zoom * s).max(2.)))
                    .border_1()
                    .border_color(cx.theme().primary),
            )
    }
}

impl Render for ErdView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(only) = self.pending_fit.take() {
            if self.bounds.get().size.width > px(0.) {
                self.fit(only.as_ref(), FIT_READABLE);
            } else {
                self.pending_fit = Some(only);
            }
        }
        let palette = *cx.palette();
        let viewport = window.viewport_size();
        let height = (f32::from(viewport.height) * 0.86 - 120.).max(360.);
        let choices: Vec<(String, String, DbKind)> = Stores::global(cx)
            .conns
            .read(cx)
            .connections
            .iter()
            .filter(|c| c.kind.is_relational())
            .map(|c| (c.id.clone(), c.name.clone(), c.kind))
            .collect();
        let current = self
            .conn_id
            .as_ref()
            .and_then(|id| choices.iter().find(|c| c.0 == *id))
            .cloned();
        let this = cx.entity().downgrade();
        let make_picker = |id: &'static str, width: f32| {
            let choices = choices.clone();
            let this = this.clone();
            Button::new(id)
                .outline()
                .small()
                .w(px(width))
                .justify_start()
                .when_some(current.clone(), |b, (_, _, k)| b.icon(ui::db_icon(k)))
                .label(
                    current
                        .as_ref()
                        .map(|c| c.1.clone())
                        .unwrap_or_else(|| t!("erd.pick_database").to_string().into()),
                )
                .dropdown_caret(true)
                .dropdown_menu(move |menu: PopupMenu, _, _| {
                    choices.iter().fold(menu, |menu, (id, name, kind)| {
                        let (this, id) = (this.clone(), id.clone());
                        menu.item(
                            PopupMenuItem::new(name.clone())
                                .icon(ui::db_icon(*kind))
                                .on_click(move |_, _, cx| {
                                    let id = id.clone();
                                    let _ = this.update(cx, |v, cx| v.pick_conn(id, cx));
                                }),
                        )
                    })
                })
        };

        let body = if self.conn_id.is_none() {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_4()
                .bg(palette.editor)
                .child(
                    ui::icon("network")
                        .size(px(40.))
                        .text_color(palette.fg_faint),
                )
                .child(ui::hint(t!("erd.pick_database_hint").to_string(), cx))
                .child(make_picker("erd-conn-empty", 260.))
                .into_any_element()
        } else if let Some(err) = &self.error {
            h_flex()
                .flex_1()
                .p_6()
                .gap_2p5()
                .items_start()
                .child(
                    div()
                        .text_sm()
                        .text_color(palette.negative)
                        .child(t!("erd.load_failed", error = err).to_string()),
                )
                .child(
                    Button::new("erd-retry")
                        .outline()
                        .xsmall()
                        .label(t!("erd.retry"))
                        .on_click(cx.listener(|this, _, _, cx| this.load_schema(cx))),
                )
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .child(self.render_toolbar(cx))
                .child(self.render_canvas(window, cx))
                .into_any_element()
        };

        v_flex()
            .h(px(height))
            .gap_3()
            .child(
                h_flex()
                    .gap_2p5()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(
                        match &self.focus {
                            Some(f) => t!("erd.title_table", table = f).to_string(),
                            None => t!("erd.title").to_string(),
                        },
                    ))
                    .child(make_picker("erd-conn", 220.))
                    .child(
                        div().w(px(320.)).child(
                            Input::new(&self.search)
                                .small()
                                .prefix(ui::icon("search").small())
                                .disabled(self.conn_id.is_none()),
                        ),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .overflow_hidden()
                    .child(body),
            )
    }
}

/// 打开关系图弹窗:有可用的当前连接则直接打开该库,否则弹窗内选库。
pub fn open(
    conn_id: Option<String>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) {
    let view = cx.new(|cx| ErdView::new(conn_id, workspace, window, cx));
    #[cfg(feature = "snapshot")]
    BENCH.with(|b| *b.borrow_mut() = Some(view.downgrade()));
    let width = (f32::from(window.viewport_size().width) * 0.92).min(1400.);
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.w(px(width)).child(view.clone())
    });
}

/// 截图基准用:平移当前打开的关系图一点,触发重绘。
#[cfg(feature = "snapshot")]
pub fn bench_pan(cx: &mut gpui_kit::App) {
    if let Some(view) = BENCH.with(|b| b.borrow().as_ref().and_then(|w| w.upgrade())) {
        view.update(cx, |v, cx| {
            v.pan.x += 3.;
            cx.notify();
        });
    }
}

/// 截图用:切换关系图视图(struct / er)。
#[cfg(feature = "snapshot")]
pub fn bench_view(mode: &str, cx: &mut gpui_kit::App) {
    if let Some(view) = BENCH.with(|b| b.borrow().as_ref().and_then(|w| w.upgrade())) {
        let mode = if mode == "er" {
            ViewMode::Er
        } else {
            ViewMode::Struct
        };
        view.update(cx, |v, cx| v.set_view(mode, cx));
    }
}

#[cfg(feature = "snapshot")]
thread_local! {
    static BENCH: std::cell::RefCell<Option<gpui_kit::WeakEntity<ErdView>>> = const { std::cell::RefCell::new(None) };
}
