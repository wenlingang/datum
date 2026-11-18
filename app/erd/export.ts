// ER 图导出 PNG:按整张图的包围盒(与当前视口、缩放、选中 / 搜索高亮无关)画进 nsh 的
// OffscreenCanvas,原生渲染成 PNG 字节。几何与屏幕上的节点一致(geom.ts),颜色取当前主题。

import { OffscreenCanvas, type CanvasRenderingContext2D } from "nsh";
import { t } from "nsh/i18n";
import { color } from "../ui/kit";
import { bounds, NODE_HEADER_H, NODE_PAD_Y, NODE_ROW_H, routeEdge, type Rects } from "./geom";
import type { ErdGraph } from "../lib/types";

/** 四周留白(点)。 */
const PAD = 40;
/** 像素密度上限;大图按 nsh 的上限(单边 16384 像素、共 6400 万像素)等比缩小。 */
const MAX_SCALE = 2;
const MAX_SIDE = 16384;
const MAX_PIXELS = 64 * 1024 * 1024;

/** 整张图画进 `cx`:原点平移到包围盒左上角减留白。 */
export function drawDiagram(cx: CanvasRenderingContext2D, graph: ErdGraph, rects: Rects, width: number, height: number, ox: number, oy: number) {
  const fg = color("foreground");
  const muted = color("muted-foreground");
  const faint = color("fg-faint");
  const border = color("border");
  cx.fillStyle = color("editor");
  cx.fillRect(0, 0, width, height);
  cx.translate(ox, oy);

  // 连线在下,节点在上
  cx.strokeStyle = faint;
  cx.fillStyle = faint;
  cx.lineWidth = 1.4;
  for (const e of graph.edges) {
    const s = rects[e.source];
    const d = rects[e.target];
    if (!s || !d) continue;
    const pts = routeEdge(s, d);
    cx.setLineDash(e.inferred ? [6, 4] : []);
    cx.beginPath();
    cx.moveTo(pts[0].x, pts[0].y);
    for (const p of pts.slice(1)) cx.lineTo(p.x, p.y);
    cx.stroke();
    const tip = pts[pts.length - 1];
    cx.setLineDash([]);
    cx.beginPath();
    cx.moveTo(tip.x, tip.y);
    cx.lineTo(tip.x - 7, tip.y - 4.2);
    cx.lineTo(tip.x - 7, tip.y + 4.2);
    cx.closePath();
    cx.fill();
  }

  cx.textBaseline = "middle";
  for (const n of graph.nodes) {
    const r = rects[n.table];
    if (!r) continue;
    // 卡片 + 表头(裁在圆角内)
    cx.save();
    cx.beginPath();
    cx.roundRect(r.x, r.y, r.w, r.h, 8);
    cx.fillStyle = color("panel");
    cx.fill();
    cx.clip();
    cx.fillStyle = color("raised");
    cx.fillRect(r.x, r.y, r.w, NODE_HEADER_H);
    cx.restore();
    cx.strokeStyle = border;
    cx.lineWidth = 1;
    cx.beginPath();
    cx.roundRect(r.x + 0.5, r.y + 0.5, r.w - 1, r.h - 1, 8);
    cx.stroke();
    cx.beginPath();
    cx.moveTo(r.x, r.y + NODE_HEADER_H);
    cx.lineTo(r.x + r.w, r.y + NODE_HEADER_H);
    cx.stroke();
    if (n.isFocus) {
      cx.fillStyle = color("primary");
      cx.fillRect(r.x + 8, r.y, r.w - 16, 2);
    }

    cx.textAlign = "left";
    cx.fillStyle = fg;
    cx.font = "600 13px monospace";
    cx.fillText(n.table, r.x + 10, r.y + NODE_HEADER_H / 2, r.w - 20);

    let y = r.y + NODE_HEADER_H + NODE_PAD_Y + NODE_ROW_H / 2;
    for (const c of n.columns) {
      if (c.isPk || c.isFk) {
        cx.textAlign = "left";
        cx.font = "700 9px sans-serif";
        cx.fillStyle = c.isPk ? color("data") : faint;
        cx.fillText(c.isPk ? "PK" : "FK", r.x + 10, y);
      }
      cx.textAlign = "right";
      cx.font = "11px sans-serif";
      cx.fillStyle = muted;
      cx.fillText(c.dataType, r.x + r.w - 10, y);
      const typeW = cx.measureText(c.dataType).width;
      cx.textAlign = "left";
      cx.font = "12px monospace";
      cx.fillStyle = fg;
      cx.fillText(c.name, r.x + 32, y, Math.max(20, r.w - 52 - typeW));
      y += NODE_ROW_H;
    }
    cx.textAlign = "left";
    if (!n.columns.length) {
      cx.font = "italic 12px sans-serif";
      cx.fillStyle = faint;
      cx.fillText(t("erd.no_key_columns"), r.x + 10, y);
      y += NODE_ROW_H;
    }
    if (n.hiddenCount > 0) {
      cx.font = "11px sans-serif";
      cx.fillStyle = faint;
      cx.fillText(t("erd.more_columns", { n: n.hiddenCount }), r.x + 10, y);
    }
  }
}

/** 像素尺寸:每点 `scale` 像素,向上取整(与 nsh 的取整一致)。 */
export function pixelSize(width: number, height: number, scale: number) {
  return { w: Math.ceil(width * scale), h: Math.ceil(height * scale) };
}

/** 不超过 nsh 上限(单边 16384 像素、共 6400 万像素)的最大密度,至多 2 倍。
 *  nsh 用 f32 算像素,这里按取整后的尺寸校验,并留一点余量。 */
export function exportScale(width: number, height: number) {
  let scale = Math.min(MAX_SCALE, MAX_SIDE / width, MAX_SIDE / height, Math.sqrt(MAX_PIXELS / (width * height))) * 0.999;
  const fits = (k: number) => {
    const { w, h } = pixelSize(width, height, k);
    return w <= MAX_SIDE && h <= MAX_SIDE && w * h <= MAX_PIXELS;
  };
  while (!fits(scale)) scale *= 0.995;
  return scale;
}

/** 整张图渲染成 PNG,返回字节与像素尺寸。空图时报错。 */
export async function renderPng(graph: ErdGraph, rects: Rects) {
  const b = bounds(Object.values(rects));
  if (!b) throw new Error("empty diagram");
  const width = Math.ceil(b.x1 - b.x0 + PAD * 2);
  const height = Math.ceil(b.y1 - b.y0 + PAD * 2);
  const scale = exportScale(width, height);
  const canvas = new OffscreenCanvas(width, height);
  drawDiagram(canvas.getContext("2d"), graph, rects, width, height, PAD - b.x0, PAD - b.y0);
  const blob = await canvas.convertToBlob({ type: "image/png", scale });
  const bytes = new Uint8Array(await blob.arrayBuffer());
  const { w, h } = pixelSize(width, height, scale);
  return { bytes, pixels: `${w}×${h}` };
}
