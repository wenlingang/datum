// ER 图的视口几何:世界坐标(布局结果,单位点) ↔ 屏幕坐标(画布内)。
// 屏幕 = 世界 × zoom + pan。纯函数,模型与视图共用。

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}
export type Rects = Record<string, Rect>;
export interface Pt {
  x: number;
  y: number;
}
export interface Viewport {
  zoom: number;
  pan: Pt;
}
export interface Size {
  width: number;
  height: number;
}

/** 节点几何,与 `datum_core::erd` 的估算一致。 */
export const NODE_HEADER_H = 34;
export const NODE_ROW_H = 22;
export const NODE_PAD_Y = 6;

export const MIN_ZOOM = 0.1;
export const MAX_ZOOM = 2;
/** 低于该缩放只画表名色块 */
export const LOD_SIMPLE = 0.45;
/** 打开 / 搜索时自动适配的最小缩放:保证字段文字可读 */
export const FIT_READABLE = 0.5;

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** 直角折线:源表右侧表头中点 → 目标表左侧表头中点;目标在左侧时绕行。同 core `route_edge`。 */
export function routeEdge(src: Rect, dst: Rect): Pt[] {
  const sx = src.x + src.w;
  const sy = src.y + NODE_HEADER_H / 2;
  const tx = dst.x;
  const ty = dst.y + NODE_HEADER_H / 2;
  if (tx >= sx + 24) {
    const mid = (sx + tx) / 2;
    return [
      { x: sx, y: sy },
      { x: mid, y: sy },
      { x: mid, y: ty },
      { x: tx, y: ty },
    ];
  }
  const outX = sx + 20;
  const inX = tx - 20;
  const bendY = Math.abs(sy - ty) < 1 ? Math.min(src.y, dst.y) - 24 : Math.max(src.y + src.h, dst.y + dst.h) + 24;
  return [
    { x: sx, y: sy },
    { x: outX, y: sy },
    { x: outX, y: bendY },
    { x: inX, y: bendY },
    { x: inX, y: ty },
    { x: tx, y: ty },
  ];
}

/** 以屏幕点 `anchor` 为中心缩放 `factor` 倍(限制在 MIN_ZOOM..MAX_ZOOM)。 */
export function zoomAt(v: Viewport, factor: number, anchor: Pt): Viewport {
  const zoom = clamp(v.zoom * factor, MIN_ZOOM, MAX_ZOOM);
  const k = zoom / v.zoom;
  return { zoom, pan: { x: anchor.x - (anchor.x - v.pan.x) * k, y: anchor.y - (anchor.y - v.pan.y) * k } };
}

/** 包围盒;没有矩形时为 null。 */
export function bounds(rects: Rect[]): { x0: number; y0: number; x1: number; y1: number } | null {
  if (!rects.length) return null;
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (const r of rects) {
    x0 = Math.min(x0, r.x);
    y0 = Math.min(y0, r.y);
    x1 = Math.max(x1, r.x + r.w);
    y1 = Math.max(y1, r.y + r.h);
  }
  return { x0, y0, x1, y1 };
}

/**
 * 把视野适配到给定矩形,最大缩放 1。`minZoom`:放不下时的下限(打开 / 搜索用可读下限,
 * 此时放不下的方向从起始边开始看;工具栏「适配视野」用 MIN_ZOOM 看全貌)。
 */
export function fit(rects: Rect[], size: Size, minZoom: number): Viewport | null {
  const b = bounds(rects);
  if (!b || size.width <= 0 || size.height <= 0) return null;
  const pad = 0.15;
  const fitted = Math.min((size.width * (1 - pad)) / (b.x1 - b.x0 || 1), (size.height * (1 - pad)) / (b.y1 - b.y0 || 1));
  const zoom = Math.max(clamp(fitted, MIN_ZOOM, 1), Math.min(minZoom, 1));
  const gw = (b.x1 - b.x0) * zoom;
  const gh = (b.y1 - b.y0) * zoom;
  const INSET = 24;
  return {
    zoom,
    pan: {
      x: gw <= size.width - INSET * 2 ? size.width / 2 - ((b.x0 + b.x1) / 2) * zoom : INSET - b.x0 * zoom,
      y: gh <= size.height - INSET * 2 ? size.height / 2 - ((b.y0 + b.y1) / 2) * zoom : INSET - b.y0 * zoom,
    },
  };
}

/** 视野在世界坐标里的范围(带 `margin` 屏幕点的边距),用于裁剪。 */
export function worldView(v: Viewport, size: Size, margin = 40) {
  const m = margin / v.zoom;
  const x0 = -v.pan.x / v.zoom - m;
  const y0 = -v.pan.y / v.zoom - m;
  return { x0, y0, x1: x0 + size.width / v.zoom + m * 2, y1: y0 + size.height / v.zoom + m * 2 };
}

export function intersects(r: { x0: number; y0: number; x1: number; y1: number }, x0: number, y0: number, x1: number, y1: number) {
  return x1 >= r.x0 && x0 <= r.x1 && y1 >= r.y0 && y0 <= r.y1;
}

/** 滚轮增量换算成点(鼠标滚轮按行计)。 */
export function wheelPoints(delta: Pt, mode: "lines" | "points"): Pt {
  const unit = mode === "lines" ? 16 : 1;
  return { x: delta.x * unit, y: delta.y * unit };
}
