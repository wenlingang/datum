// ER 图的纯函数:导出 PNG 的空图与尺寸上限(出图本身要真实窗口,见 erd.native.test.tsx)、小地图的坐标映射。

import { expect, test } from "nsh/test";
import { exportScale, pixelSize, renderPng } from "./export";
import { MINIMAP_H, MINIMAP_W, minimapFrame, minimapToWorld, union } from "./geom";

test("空图报错", async () => {
  let err = "";
  try {
    await renderPng({ nodes: [], edges: [], fks: [] }, {});
  } catch (e) {
    err = String(e);
  }
  expect(err).toContain("empty");
});

test("大图按 nsh 上限降低密度", () => {
  for (const [w, h] of [[20000, 300], [9000, 9000], [16000, 16000], [100, 100]]) {
    const s = exportScale(w, h);
    const p = pixelSize(w, h, s);
    expect(s).toBeLessThan(2.0001);
    expect(p.w).toBeLessThan(16385);
    expect(p.h).toBeLessThan(16385);
    expect(p.w * p.h).toBeLessThan(64 * 1024 * 1024 + 1);
  }
});

test("小地图:范围等比放进框内并居中,来回换算一致", () => {
  const f = minimapFrame({ x0: 100, y0: 50, x1: 1100, y1: 550 });
  // 1000×500 放进 168×108:宽度决定比例
  expect(Math.abs(f.scale - 0.168)).toBeLessThan(1e-9);
  const left = f.ox + 100 * f.scale;
  const right = f.ox + 1100 * f.scale;
  expect(Math.abs(left - (MINIMAP_W - right))).toBeLessThan(1e-9);
  const top = f.oy + 50 * f.scale;
  const bottom = f.oy + 550 * f.scale;
  expect(Math.abs(top - (MINIMAP_H - bottom))).toBeLessThan(1e-9);
  const w = minimapToWorld(f, { x: f.ox + 600 * f.scale, y: f.oy + 300 * f.scale });
  expect(Math.round(w.x)).toBe(600);
  expect(Math.round(w.y)).toBe(300);
});

test("小地图:范围含视野(视野在图外时框也画得出来)", () => {
  const u = union({ x0: 0, y0: 0, x1: 100, y1: 100 }, { x0: -500, y0: 20, x1: -300, y1: 60 });
  expect(u).toEqual({ x0: -500, y0: 0, x1: 100, y1: 100 });
});
