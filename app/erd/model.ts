// ER 图 tab 的模型:整库外键 / 主键、构图选项、节点坐标、视口与交互状态。
// 构图与布局在原生(datum-core erd);这里只管状态流转。挂在 WorkTab.state 上,换 tab 不丢。

import { batch, signal, type Signal } from "nsh";
import { conns, loadTables } from "../store/conns";
import { erd, errorText } from "../lib/native";
import type { DbKind, ErdGraph, ErdSchema } from "../lib/types";
import { FIT_READABLE, fit, MIN_ZOOM, zoomAt, type Pt, type Rects, type Size, type Viewport } from "./geom";

/** 整库表数超过它时默认用「工程 ER 图」(全部折叠,只显键列),同 core COLLAPSE_THRESHOLD。 */
const COLLAPSE_THRESHOLD = 30;

export type ViewMode = "struct" | "er";

export interface ErdModel {
  readonly kind: "erd";
  readonly connId: string;
  readonly dbKind: DbKind;
  readonly schema: Signal<ErdSchema | null>;
  readonly loading: Signal<boolean>;
  readonly error: Signal<string | null>;
  readonly graph: Signal<ErdGraph>;
  /** 节点坐标(世界坐标) */
  readonly rects: Signal<Rects>;
  /** 聚焦表(只看它与直接邻居);null = 整库 */
  readonly focus: Signal<string | null>;
  readonly collapsed: Signal<Record<string, boolean>>;
  readonly full: Signal<Record<string, boolean>>;
  readonly inferred: Signal<boolean>;
  readonly viewMode: Signal<ViewMode | null>;
  /** 点选的表:相连的边亮起并显示列配对 */
  readonly selected: Signal<string | null>;
  readonly search: Signal<string>;
  /** 搜索命中的表;没有搜索时为 null */
  readonly hits: Signal<Set<string> | null>;
  readonly viewport: Signal<Viewport>;
  /** 画布尺寸(onLayout) */
  readonly size: Signal<Size>;
  /** 尺寸确定后要适配的视野:undefined 不适配,null 全图,数组为指定表 */
  pendingFit: string[] | null | undefined;
  searchTimer: number | undefined;
}

const EMPTY: ErdGraph = { nodes: [], edges: [], fks: [] };

export function erdModelOf(state: unknown): ErdModel | undefined {
  const s = state as ErdModel | undefined;
  return s && typeof s === "object" && s.kind === "erd" ? s : undefined;
}

export function createErdModel(connId: string, dbKind: DbKind): ErdModel {
  return {
    kind: "erd",
    connId,
    dbKind,
    schema: signal<ErdSchema | null>(null),
    loading: signal(false),
    error: signal<string | null>(null),
    graph: signal(EMPTY),
    rects: signal<Rects>({}),
    focus: signal<string | null>(null),
    collapsed: signal<Record<string, boolean>>({}),
    full: signal<Record<string, boolean>>({}),
    inferred: signal(true),
    viewMode: signal<ViewMode | null>(null),
    selected: signal<string | null>(null),
    search: signal(""),
    hits: signal<Set<string> | null>(null),
    viewport: signal<Viewport>({ zoom: 1, pan: { x: 0, y: 0 } }),
    size: signal<Size>({ width: 0, height: 0 }),
    pendingFit: undefined,
    searchTimer: undefined,
  };
}

const tablesOf = (m: ErdModel) => conns.tables(m.connId) ?? [];
const columnsOf = (m: ErdModel) => conns.columns(m.connId) ?? {};

function buildGraph(m: ErdModel): ErdGraph | null {
  const schema = m.schema();
  if (!schema) return null;
  return erd.build(tablesOf(m), columnsOf(m), schema, {
    focus: m.focus(),
    collapsed: m.collapsed(),
    full: m.full(),
    inferred: m.inferred(),
  });
}

/** 新尺寸写回坐标;有变化时就地推开重叠(`fixed` 不动)。 */
function applySizes(m: ErdModel, g: ErdGraph, fixed: string | null) {
  const rects = { ...m.rects() };
  let changed = false;
  for (const n of g.nodes) {
    const r = rects[n.table];
    if (r && (r.w !== n.w || r.h !== n.h)) {
      rects[n.table] = { ...r, w: n.w, h: n.h };
      changed = true;
    }
  }
  m.rects.set(changed ? erd.resolve(rects, fixed) : rects);
}

/**
 * 重建图。`keep`:节点集合没变时保留坐标(列数据晚到只更新尺寸并推开);
 * 否则整图重新布局,并在下一次有尺寸时适配视野。
 */
export function rebuild(m: ErdModel, keep: boolean) {
  const g = buildGraph(m);
  if (!g) return;
  batch(() => {
    const rects = m.rects();
    const same = keep && g.nodes.length === Object.keys(rects).length && g.nodes.every((n) => n.table in rects);
    if (same) applySizes(m, g, null);
    else {
      m.rects.set(erd.layout(g));
      m.pendingFit = null;
      applyPendingFit(m);
    }
    if (m.viewMode() === null && m.focus() === null && tablesOf(m).length > COLLAPSE_THRESHOLD && !Object.keys(m.collapsed()).length) {
      m.viewMode.set("er");
    }
    m.graph.set(g);
    refreshHits(m);
  });
}

/** 拉取整库外键 + 主键(表与列走连接缓存,未连接时先连接)。 */
export async function load(m: ErdModel) {
  m.loading.set(true);
  m.error.set(null);
  try {
    if (!conns.tables(m.connId)) await loadTables(m.connId);
    const schema = await erd.schema(m.dbKind, m.connId);
    batch(() => {
      m.schema.set(schema);
      m.rects.set({});
      rebuild(m, false);
    });
  } catch (e) {
    m.error.set(errorText(e));
  } finally {
    m.loading.set(false);
  }
}

/** 表结构图(全部字段)/ 工程 ER 图(只显键列)。 */
export function setView(m: ErdModel, mode: ViewMode) {
  const tables = tablesOf(m);
  batch(() => {
    m.viewMode.set(mode);
    m.collapsed.set(Object.fromEntries(tables.map((t) => [t, mode === "er"])));
    m.full.set(mode === "struct" ? Object.fromEntries(tables.map((t) => [t, true])) : {});
    m.rects.set({});
    rebuild(m, false);
  });
}

/** 「+N 个字段」:该表直达完整字段,就地长高并推开压住的邻居(不整图重排)。 */
export function expand(m: ErdModel, table: string) {
  batch(() => {
    m.collapsed.update((c) => ({ ...c, [table]: false }));
    m.full.update((f) => ({ ...f, [table]: true }));
    const g = buildGraph(m);
    if (!g) return;
    applySizes(m, g, table);
    m.graph.set(g);
  });
}

/** 聚焦某表(只看它与直接邻居);null 回到整库。 */
export function setFocus(m: ErdModel, table: string | null) {
  batch(() => {
    m.focus.set(table);
    m.selected.set(null);
    m.collapsed.set({});
    m.full.set({});
    m.viewMode.set(null);
    m.rects.set({});
    rebuild(m, false);
  });
}

export function toggleInferred(m: ErdModel) {
  batch(() => {
    m.inferred.update((v) => !v);
    m.rects.set({});
    rebuild(m, false);
  });
}

function refreshHits(m: ErdModel) {
  const q = m.search().trim();
  m.hits.set(q ? new Set(erd.matchTables(q, tablesOf(m), columnsOf(m), m.graph().fks)) : null);
}

/** 搜索:立即高亮命中;停止输入 300ms 后视野移到命中的表。 */
export function setSearch(m: ErdModel, q: string) {
  batch(() => {
    m.search.set(q);
    refreshHits(m);
  });
  clearTimeout(m.searchTimer);
  m.searchTimer = setTimeout(() => {
    const hits = m.hits();
    if (hits && hits.size) fitTo(m, [...hits], FIT_READABLE);
  }, 300) as unknown as number;
}

/** 把视野适配到给定表(null = 全部)。 */
export function fitTo(m: ErdModel, only: string[] | null, minZoom = MIN_ZOOM) {
  const rects = m.rects();
  const list = only ? only.map((t) => rects[t]).filter(Boolean) : Object.values(rects);
  const v = fit(list, m.size(), minZoom);
  if (v) m.viewport.set(v);
  return !!v;
}

/** 有待适配且画布已有尺寸时执行(打开、重新布局、尺寸首次确定)。 */
export function applyPendingFit(m: ErdModel) {
  if (m.pendingFit === undefined) return;
  if (fitTo(m, m.pendingFit, FIT_READABLE)) m.pendingFit = undefined;
}

export function zoomBy(m: ErdModel, factor: number, anchor?: Pt) {
  const s = m.size();
  m.viewport.update((v) => zoomAt(v, factor, anchor ?? { x: s.width / 2, y: s.height / 2 }));
}

export function panBy(m: ErdModel, dx: number, dy: number) {
  m.viewport.update((v) => ({ zoom: v.zoom, pan: { x: v.pan.x + dx, y: v.pan.y + dy } }));
}

/** 拖动节点:屏幕位移 → 世界坐标。 */
export function moveNode(m: ErdModel, table: string, dx: number, dy: number) {
  const z = m.viewport().zoom;
  m.rects.update((rs) => {
    const r = rs[table];
    return r ? { ...rs, [table]: { ...r, x: r.x + dx / z, y: r.y + dy / z } } : rs;
  });
}

/** 节点的(高亮, 退后):搜索优先于点选。 */
export function nodeState(m: ErdModel, table: string): { hit: boolean; dim: boolean } {
  const hits = m.hits();
  if (hits) return { hit: hits.has(table), dim: !hits.has(table) };
  const sel = m.selected();
  if (!sel) return { hit: false, dim: false };
  if (sel === table) return { hit: false, dim: false };
  const linked = m.graph().edges.some((e) => (e.source === sel && e.target === table) || (e.target === sel && e.source === table));
  return { hit: false, dim: !linked };
}
