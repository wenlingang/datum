// 停止正在执行的查询(真实 PG / MySQL,run-tests.sh 的 pg 与 mysql 组都跑这一份,表 big 3000 行):
// 执行中工具条出现「停止」,点了之后服务器端中止语句,结果区报「已取消」,连接照常可用。

import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1400, height: 900, locale: "en-US" };
const has = (app: App, part: string) => app.texts().some((s) => s.includes(part));
// PG:canceling statement due to user request;MySQL:Query execution was interrupted
const stopped = (app: App) => has(app, "canceling statement") || has(app, "interrupted");

async function start(): Promise<App> {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  return app;
}

async function setSql(app: App, sql: string, current: string) {
  await app.press(`text:${current}`);
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", sql);
}

test("执行中可停止:服务器端中止,之后照常执行", async () => {
  const app = await start();
  expect(app.shows("id:stop")).toBe(false);
  // 三表笛卡尔积(270 亿行)逐行求和要跑很久(COUNT(*) 会被 MySQL 直接算出来)
  await setSql(app, "SELECT SUM(a.v + b.v + c.v) FROM big a, big b, big c", "-- Top products by revenue");
  await app.press("id:run");
  await app.waitFor("id:stop", { timeout: 10000 });
  const started = Date.now();
  await app.press("id:stop");
  await app.waitUntil(() => stopped(app), { timeout: 10000 });
  expect(Date.now() - started).toBeLessThan(10000);
  await app.waitUntil(() => !app.shows("id:stop"), { timeout: 5000 });
  // 连接照常可用
  await setSql(app, "SELECT COUNT(*) AS n FROM products", "SELECT SUM(a.v + b.v + c.v) FROM big a, big b, big c");
  await app.press("id:run");
  await app.waitUntil(() => has(app, "1 rows returned") || has(app, "1 row returned"), { timeout: 10000 });
  app.dispose();
});
