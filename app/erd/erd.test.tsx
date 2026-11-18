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

test("导出按钮:图加载后可用", async () => {
  const app = await openErd();
  await app.waitUntil(() => app.enabled("id:erd-export"));
});

test("小地图:开关显示", async () => {
  const app = await openErd();
  await app.waitFor("id:erd-minimap");
  await app.press("id:erd-minimap-toggle");
  await app.waitUntil(() => !app.shows("id:erd-minimap"));
  await app.press("id:erd-minimap-toggle");
  await app.waitFor("id:erd-minimap");
});

test("小地图:拖到左上角,视野跟过去(画布不反向平移),选中不被清掉", async () => {
  const app = await openErd();
  await app.press("id:erd-node:order_items");
  const dimmed = () => (app.viewStyle("id:erd-node:notes")?.opacity ?? 1) < 0.5;
  await app.waitUntil(dimmed);
  for (let i = 0; i < 8; i++) await app.press("label:Zoom In");
  await app.waitUntil(() => app.value("id:erd-zoom") === "200%" || app.texts().includes("200%"));
  expect(app.shows("id:erd-node:customers")).toBe(true);
  // 从小地图中间拖到左上角:视野中心移到图的左上,最右边的 customers 出了视野
  await app.drag("id:erd-minimap", { x: -80, y: -50 });
  await app.waitUntil(() => !app.shows("id:erd-node:customers"));
  expect(app.shows("id:erd-node:order_items")).toBe(true);
  // 单击小地图也不算点空白:选中还在(notes 已出视野,回到全图再看)
  await app.press("id:erd-minimap");
  await app.press("label:Fit to View");
  await app.waitFor("id:erd-node:notes");
  expect(dimmed()).toBe(true);
});

test("工具条放不下时不丢控件:窄宽度下搜索框、开关与按钮都还在", async () => {
  const app = await openErd();
  await app.setViewport(480, 800);
  for (const id of ["erd-search", "erd-inferred", "erd-view-struct", "erd-view-er", "erd-export"]) {
    expect(app.shows(`id:${id}`)).toBe(true);
  }
  expect(app.shows("label:Reload foreign keys / primary keys")).toBe(true);
  await app.press("id:erd-view-er");
  await app.waitUntil(() => !hasText(app, "total"));
});
