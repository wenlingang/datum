// 索引建议的流程测试(headless):临时目录里的 SQLite(roastery-seed.sql,products 250 行)。
// 运行脚本在测试前后比较库里的索引:验证在回滚的事务里进行,测试结束后不应多出任何索引。

import { expect, open, test, type App } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1400, height: 900, locale: "en-US" };
const has = (app: App, part: string) => app.texts().some((s) => s.includes(part));

const SQL = "SELECT name, price_cents FROM products WHERE origin = 'Kenya' AND roast_level = 'medium' ORDER BY created_at DESC";
const DDL = "CREATE INDEX idx_products_origin_roast_level_created_at ON products (origin, roast_level, created_at);";

async function start(sql: string): Promise<App> {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  await app.press("text:-- Top products by revenue");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", sql);
  return app;
}

test("编辑器运行菜单 → 索引建议:给出候选并在回滚的事务里验证,计划前后对比", async () => {
  const app = await start(SQL);
  await app.press("id:run-limit");
  await app.press("id:suggest-indexes");
  await app.waitFor("id:index-advisor", { timeout: 10000 });
  await app.waitUntil(() => has(app, DDL), { timeout: 15000 });
  expect(app.texts()).toContain("Verified");
  expect(app.texts()).toContain("Helps");
  expect(app.texts()).toContain("Rule");
  expect(has(app, "Plan uses index: yes")).toBe(true);
  expect(has(app, "Full scans 1 → 0")).toBe(true);
  expect(has(app, "rolled back")).toBe(true);
  // 计划对比:左边全表扫描,右边走建议的索引(临时名已换成建议名)
  expect(app.shows("id:advisor-plan-before")).toBe(true);
  expect(has(app, "USING INDEX idx_products_origin_roast_level_created_at")).toBe(true);
  expect(app.texts()).toContain("Full scan");
  // 放进编辑器:新开查询 tab,不执行
  await app.press("id:advisor-open:0");
  await app.waitUntil(() => has(app, "-- Suggested by the index advisor"), { timeout: 5000 });
  expect(has(app, DDL)).toBe(true);
  app.dispose();
});

test("Explain 之后从计划视图打开索引建议;已有索引覆盖的列不再建议", async () => {
  const app = await start("SELECT * FROM products WHERE category_id = 2");
  await app.press("id:explain");
  await app.waitFor("id:plan-suggest-indexes", { timeout: 10000 });
  await app.press("id:plan-suggest-indexes");
  await app.waitFor("id:index-advisor", { timeout: 10000 });
  await app.waitFor("id:advisor-empty", { timeout: 15000 });
  expect(has(app, "No index suggestions")).toBe(true);
  app.dispose();
});

test("带命名参数的语句先弹框填值,再给建议", async () => {
  const app = await start("SELECT * FROM orders WHERE product_id = :pid AND quantity > 3");
  await app.press("id:run-limit");
  await app.press("id:suggest-indexes");
  await app.waitFor("id:params-dialog", { timeout: 5000 });
  await app.type("id:param:pid", "7");
  await app.press("id:params-run");
  await app.waitFor("id:index-advisor", { timeout: 10000 });
  await app.waitUntil(() => has(app, "CREATE INDEX idx_orders_product_id_quantity ON orders (product_id, quantity);"), { timeout: 15000 });
  expect(has(app, "product_id = 7")).toBe(true);
  app.dispose();
});
