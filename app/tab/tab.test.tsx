// 查询 / 表浏览标签页的流程测试(headless,用户的操作步骤)。
// 只连临时目录里的 SQLite:运行前先重建库,并设 DATUM_CONFIG_DIR 指向临时配置目录
// (其中 connections.json 只有一条指向该库的 sqlite 连接)。库里有 products 250 行、orders 400 行。
// 测试按顺序执行,后面的会改库(提交编辑、删除 orders)。

import { Store } from "datum/native";
import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1200, height: 800, locale: "en-US" };

const hasText = (app: App, prefix: string) => app.texts().some((s) => s.startsWith(prefix));

async function start(): Promise<App> {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  return app;
}

test("运行查询:显示行数、网格与历史", async () => {
  const app = await start();
  await app.press("id:run");
  // 语句以注释开头,core 的 is_query 判为非查询(与 GPUI 版一致:状态行显示「执行成功」),结果照常出网格
  await app.waitUntil(() => app.texts().includes("Coffee 133"), { timeout: 15000 });
  expect(hasText(app, "Executed successfully")).toBe(true);
  expect(app.shows("id:result-grid")).toBe(true);
  // 点自定义表头做客户端排序(升序):收入最低的排到最前
  const firstCoffee = () => app.texts().find((x) => x.startsWith("Coffee "));
  expect(firstCoffee()).toBe("Coffee 133");
  await app.press("text:revenue_usd");
  await app.waitUntil(() => firstCoffee() !== "Coffee 133", { timeout: 5000 });
  // 成功执行进历史
  await app.press("id:history");
  await app.waitUntil(() => hasText(app, "-- Top products by revenue"), { timeout: 5000 });
});

async function replaceSql(app: App, sql: string) {
  await app.press("text:-- Top products by revenue");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", sql);
}

test("SELECT 语句:状态行报告返回行数", async () => {
  const app = await start();
  await replaceSql(app, "SELECT id, name FROM categories");
  await app.press("id:run");
  await app.waitUntil(() => hasText(app, "Query succeeded · 4 rows returned"), { timeout: 15000 });
  expect(app.texts()).toContain("Single Origin");
});

test("表浏览分页:每页 100 行,末页禁用下一页", async () => {
  const app = await start();
  await app.press("id:tab:products");
  await app.waitUntil(() => app.texts().includes("100") && hasText(app, "Page"), { timeout: 15000 });
  expect(app.texts()).toContain("1");
  await app.press("label:Next Page");
  await app.waitUntil(() => app.texts().includes("2"), { timeout: 10000 });
  await app.press("label:Next Page");
  await app.waitUntil(() => app.texts().includes("50"), { timeout: 10000 });
  expect(app.enabled("label:Next Page")).toBe(false);
  await app.press("label:Previous Page");
  await app.waitUntil(() => app.texts().includes("100"), { timeout: 10000 });
});

test("编辑暂存后 ⌘S 一次提交", async () => {
  const app = await start();
  await app.press("id:tab:products");
  await app.waitUntil(() => app.texts().includes("Coffee 1"), { timeout: 15000 });
  await app.rightClick("text:Coffee 1");
  expect(app.menu()?.map((e) => e.label)).toContain("Edit");
  await app.chooseMenuItem("Edit");
  await app.waitFor("id:cell-editor");
  await app.type("id:cell-editor", "Edited ");
  await app.press("id:cell-stage");
  await app.waitFor("text:Staged");
  await app.press("label:x");
  await app.waitFor("text:Edit Cell", { state: "gone" });
  await app.keyboard.press("mod+s");
  await app.waitUntil(() => app.texts().includes("Edited Coffee 1"), { timeout: 15000 });
});

test("报错可交给 AI 修复;Explain 显示执行计划并可交给 AI 解读", async () => {
  const app = await start();
  await replaceSql(app, "SELECT nme FROM products");
  await app.press("id:run");
  await app.waitFor("id:query-error", { timeout: 15000 });
  expect(app.shows("id:ai-fix-error")).toBe(true);
  // 没配置 AI:SQL 引用到面板并提示,不报错
  await app.press("id:ai-fix-error");
  await app.waitUntil(() => hasText(app, "The SQL is quoted in the AI panel"), { timeout: 5000 });
  // Explain:SQLite 走 EXPLAIN QUERY PLAN,输出查询计划而不是字节码
  await app.press("text:SELECT nme FROM products");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "SELECT * FROM products WHERE origin = 'Kenya'");
  await app.press("text:Explain");
  await app.waitFor("text:Execution Plan", { timeout: 15000 });
  expect(app.shows("id:ai-explain-plan")).toBe(true);
  expect(app.texts().some((x) => x.startsWith("SCAN") || x.startsWith("SEARCH"))).toBe(true);
  expect(app.shows("id:ai-fix-error")).toBe(false);
  // 普通查询的结果不显示执行计划栏
  await app.press("id:run");
  await app.waitFor("text:Execution Plan", { state: "gone", timeout: 15000 });
  app.dispose();
});

test("写操作可撤销:执行后撤销把数据恢复原样;不能撤销时说明原因", async () => {
  const app = await start();
  await replaceSql(app, "UPDATE products SET origin = 'Undo test' WHERE id <= 3");
  await app.press("id:run");
  await app.waitUntil(() => hasText(app, "✓ Executed successfully · 3 rows affected"), { timeout: 15000 });
  await app.waitFor("text:Can be undone: restores the 3 rows as they were before");
  await app.press("id:undo");
  await app.waitFor("id:undo-dialog");
  expect(app.texts().some((x) => x.includes("These 3 statements restore the 3 rows"))).toBe(true);
  await app.press("id:undo-confirm");
  await app.waitFor("text:Undone: 3 rows restored", { timeout: 15000 });
  expect(app.shows("id:undo")).toBe(false);
  // 数据回到原样:没有一行还是 'Undo test'
  await app.press("text:UPDATE products SET origin = 'Undo test' WHERE id <= 3");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "SELECT COUNT(*) AS n FROM products WHERE origin = 'Undo test'");
  await app.press("id:run");
  await app.waitFor("id:result-grid", { timeout: 15000 });
  expect(app.texts()).toContain("0");
  // 没有改到任何行:不显示撤销,也不说「不能撤销」
  await app.press("text:SELECT COUNT(*) AS n FROM products WHERE origin = 'Undo test'");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "UPDATE products SET origin = 'x' WHERE id < 0");
  await app.press("id:run");
  await app.waitUntil(() => hasText(app, "✓ Executed successfully"), { timeout: 15000 });
  expect(app.shows("id:undo")).toBe(false);
  expect(app.shows("id:undo-unavailable")).toBe(false);
  // DDL 不能撤销:执行照常,并说明原因(用一张临时表,不动其他测试用的数据)
  await app.press("text:UPDATE products SET origin = 'x' WHERE id < 0");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "CREATE TABLE undo_scratch (id INTEGER)");
  await app.press("id:run");
  await app.waitUntil(() => hasText(app, "✓ Executed successfully"), { timeout: 15000 });
  await app.waitFor("id:undo-unavailable");
  expect(hasText(app, "Can't be undone: Undo is only available for single-table UPDATE and DELETE")).toBe(true);
  app.dispose();
});

test("破坏性语句先审查:取消不执行,确认才执行", async () => {
  const app = await start();
  await replaceSql(app, "DELETE FROM orders");
  await app.press("id:run");
  await app.waitFor("text:Review Destructive Statements");
  expect(app.texts()).toContain("Danger");
  await app.press("id:risk-cancel");
  await app.waitFor("text:Review Destructive Statements", { state: "gone" });
  expect(hasText(app, "Executed successfully")).toBe(false);
  await app.press("id:run");
  await app.waitFor("text:Review Destructive Statements");
  // 影响范围预估:行数 + 前 10 行预览,数据未动
  await app.waitFor("text:Will affect 400 rows", { timeout: 15000 });
  await app.waitFor("text:Showing the first 10 of 400 rows");
  await app.press("id:risk-confirm");
  await app.waitUntil(() => hasText(app, "✓ Executed successfully · 400 rows affected"), { timeout: 15000 });
});

test("prod 连接:带 WHERE 的写语句也先确认并预估影响范围", async () => {
  const sql = "UPDATE products SET price_cents = price_cents WHERE id <= 5";
  // local:带 WHERE 的 UPDATE 直接执行
  let app = await start();
  await replaceSql(app, sql);
  await app.press("id:run");
  await app.waitUntil(() => hasText(app, "✓ Executed successfully · 5 rows affected"), { timeout: 15000 });
  expect(app.shows("id:risk-dialog")).toBe(false);
  app.dispose();
  // 换成 prod(SQLite 不读密码,改环境不影响连接)
  const original = (Store.connections() as any[])[0];
  Store.saveConnection({ ...original, env: "prod" }, null);
  try {
    app = await start();
    await replaceSql(app, sql);
    await app.press("id:run");
    await app.waitFor("text:Review Destructive Statements");
    expect(app.texts()).toContain("UPDATE on production");
    expect(hasText(app, "Running on database")).toBe(true);
    expect(app.texts().some((x) => x.includes("Review the following 1 statements"))).toBe(true);
    await app.waitFor("text:Will affect 5 rows", { timeout: 15000 });
    // 预览表头带出列名,行数不超过 10 时不显示「共 N 行」
    expect(app.texts()).toContain("sku");
    expect(app.texts()).toContain("+5 columns");
    expect(hasText(app, "Showing the first")).toBe(false);
    await app.press("id:risk-cancel");
    await app.waitFor("id:risk-dialog", { state: "gone" });
    // 改写不了的写法:标明无法预估,不猜
    await app.press(`text:${sql}`);
    await app.keyboard.press("mod+a");
    await app.type("id:sql-editor", "WITH x AS (SELECT 1) UPDATE products SET price_cents = price_cents WHERE id IN (SELECT 1 FROM x)");
    await app.press("id:run");
    await app.waitFor("text:Review Destructive Statements");
    await app.waitFor("text:/^Can't estimate the affected rows/", { timeout: 15000 });
    await app.press("id:risk-cancel");
    app.dispose();
  } finally {
    Store.saveConnection(original, null);
  }
});
