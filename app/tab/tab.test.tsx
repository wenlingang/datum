// 查询 / 表浏览标签页的流程测试(headless,用户的操作步骤)。
// 只连临时目录里的 SQLite:运行前先重建库,并设 DATUM_CONFIG_DIR 指向临时配置目录
// (其中 connections.json 只有一条指向该库的 sqlite 连接)。库里有 products 250 行、orders 400 行。
// 测试按顺序执行,后面的会改库(提交编辑、删除 orders)。

import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1200, height: 800, locale: "en-US" };

const hasText = (app: App, prefix: string) => app.texts().some((s) => s.startsWith(prefix));

async function start(): Promise<App> {
  const app = await open("../TabDev.tsx", OPTS);
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
  // 列头是自定义表头
  expect(app.shows("id:th:revenue_usd")).toBe(true);
  // 成功执行进历史
  await app.press("id:history");
  await app.waitUntil(() => hasText(app, "-- Top products by revenue"), { timeout: 5000 });
});

async function replaceSql(app: App, sql: string) {
  await app.press("id:sql-editor");
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
  await app.press("id:risk-confirm");
  await app.waitUntil(() => hasText(app, "✓ Executed successfully · 400 rows affected"), { timeout: 15000 });
});
