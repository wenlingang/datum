// 表浏览的自然语言筛选(headless)。run-tests.sh 起一个 OpenAI 兼容的假服务,固定回:
// origin = Kenya 且 roast_level = medium,外加一个不存在的列 rating(应被忽略并说明)。

import { expect, open, test, type App } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1200, height: 800, locale: "en-US" };

async function start(): Promise<App> {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  return app;
}

test("一句话 → 且/或条件:填进筛选条并应用,不存在的列忽略并说明", async () => {
  const app = await start();
  await app.press("id:tab:products");
  await app.waitUntil(() => app.texts().includes("Coffee 1"), { timeout: 15000 });
  await app.keyboard.press("mod+f");
  await app.waitFor("id:filter-nl", { timeout: 5000 });
  await app.type("id:filter-nl", "Kenya medium roast with rating above 4");
  await app.press("id:filter-nl-go");
  await app.waitFor("id:filter-nl-note", { timeout: 15000 });
  const texts = app.texts();
  // 列名大小写已纠正成真实列名,条件可见可改
  expect(texts).toContain("origin");
  expect(texts).toContain("roast_level");
  expect(texts.some((s) => s.includes("rating 列不存在"))).toBe(true);
  expect(texts.some((s) => s.includes("Skipped") && s.includes("rating gt 4"))).toBe(true);
  // 已应用:第一页里 Kenya(i % 6 = 1)且 medium(i % 3 = 1)的是 1、7、13……
  await app.waitUntil(() => !app.texts().includes("Coffee 2"), { timeout: 5000 });
  expect(app.texts()).toContain("Coffee 1");
  expect(app.texts()).toContain("Coffee 7");
  expect(app.texts()).not.toContain("Coffee 4");
});
