// ER 图的纯函数:导出 PNG 的空图与尺寸上限(出图本身要真实窗口,见 erd.native.test.tsx)。

import { expect, test } from "nsh/test";
import { exportScale, pixelSize, renderPng } from "./export";

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

