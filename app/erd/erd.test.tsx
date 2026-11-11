// ER 图 tab 的交互测试(无头,macOS 桌面)。配置目录见 testing/run-tests.sh 的 erd 段:
// customers ← orders ← order_items(物理外键),order_items.product_id → products(按命名推断),
// products 有 15 列(超出展开上限 12,尾行「+N 个字段」),notes 无关系。

import { expect, open, test, type App } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1200, height: 800, locale: "en-US" };
const hasText = (app: App, s: string) => app.texts().some((x) => x.includes(s));

async function openErd() {
  const app = await open("../tab/testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  await app.press("label:erd");
  await app.waitFor("id:erd-node:orders", { timeout: 15000 });
  return app;
}

test("整库:表节点、外键与推断外键的统计", async () => {
  const app = await openErd();
  for (const t of ["customers", "orders", "order_items", "products", "notes"]) expect(app.shows(`id:erd-node:${t}`)).toBe(true);
  expect(hasText(app, "5 tables · 3 relationships")).toBe(true);
  expect(hasText(app, "1 inferred from naming")).toBe(true);
});

test("关闭推断外键:只剩物理外键", async () => {
  const app = await openErd();
  await app.press("id:erd-inferred");
  await app.waitUntil(() => hasText(app, "5 tables · 2 relationships"));
  expect(hasText(app, "inferred from naming")).toBe(false);
});

test("「+N 个字段」就地展开全部字段", async () => {
  const app = await openErd();
  await app.waitFor("id:erd-more:products");
  expect(hasText(app, "c15_last")).toBe(false);
  await app.press("id:erd-more:products");
  await app.waitUntil(() => hasText(app, "c15_last"));
  expect(app.shows("id:erd-more:products")).toBe(false);
});

test("聚焦某表只看邻居,返回整库", async () => {
  const app = await openErd();
  await app.press("id:erd-focus:customers");
  await app.waitFor("id:erd-back");
  expect(app.shows("id:erd-node:notes")).toBe(false);
  expect(app.shows("id:erd-node:orders")).toBe(true);
  expect(hasText(app, "2 tables · 1 relationships")).toBe(true);
  await app.press("id:erd-back");
  await app.waitFor("id:erd-node:notes");
});

test("工程 ER 图只显示键列,表结构图显示全部字段", async () => {
  const app = await openErd();
  await app.press("id:erd-view-er");
  await app.waitUntil(() => !hasText(app, "total"));
  expect(hasText(app, "customer_id")).toBe(true);
  await app.press("id:erd-view-struct");
  await app.waitUntil(() => hasText(app, "c15_last"));
  expect(hasText(app, "total")).toBe(true);
});

test("缩放按钮与适配视野", async () => {
  const app = await openErd();
  const zoom = () => app.value("id:erd-zoom") ?? app.texts().find((s) => /^\d+%$/.test(s)) ?? "";
  const before = zoom();
  await app.press("label:Zoom In");
  await app.waitUntil(() => zoom() !== before);
  await app.press("label:Fit to View");
  await app.waitUntil(() => /^\d+%$/.test(zoom()));
});

test("双击表节点打开表浏览", async () => {
  const app = await openErd();
  await app.press("id:erd-node:notes");
  await app.press("id:erd-node:notes");
  await app.waitFor("id:tab:notes");
});
