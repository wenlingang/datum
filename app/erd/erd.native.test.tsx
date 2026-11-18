// ER 图导出 PNG(真实窗口:无头平台不渲染离屏画布)。保存不弹框:run-tests.sh 设了
// DATUM_TEST_SAVE_DIR,跑完由脚本检查目录里的文件是 PNG。
// 运行:app/testing/run-tests.sh native(需要能开窗口的桌面)

import { expect, open, test } from "nsh/test";

test("导出 PNG:整张图渲染并保存,提示像素尺寸", async () => {
  const app = await open("../tab/testing/Harness.tsx", { platform: "macos", width: 1200, height: 800, locale: "en-US" });
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  await app.press("label:erd");
  await app.waitFor("id:erd-node:orders", { timeout: 15000 });
  await app.waitUntil(() => app.enabled("id:erd-export"));
  await app.press("id:erd-export");
  await app.waitUntil(() => app.texts().some((s) => s.includes("diagram exported")), { timeout: 15000 });
  const toast = app.texts().find((s) => s.includes("diagram exported")) ?? "";
  // 5 张表的图:2 倍密度,两边都远超 1000 像素
  const m = /(\d+)×(\d+)/.exec(toast);
  expect(!!m).toBe(true);
  expect(Number(m?.[1])).toBeGreaterThan(1000);
  expect(Number(m?.[2])).toBeGreaterThan(400);
  app.dispose();
});

test("工具条里的搜索框:方向键移动光标,不被工具条接走", async () => {
  const app = await open("../tab/testing/Harness.tsx", { platform: "macos", width: 1200, height: 800, locale: "en-US" });
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  await app.press("label:erd");
  await app.waitFor("id:erd-node:orders", { timeout: 15000 });
  await app.type("id:erd-search", "ordr");
  await app.keyboard.press("left");
  await app.keyboard.press("e");
  await app.waitUntil(() => app.value("id:erd-search") === "order", { timeout: 5000 });
  app.dispose();
});
