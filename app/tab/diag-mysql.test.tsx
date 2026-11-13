// 真实 MySQL 上的诊断操作(run-tests.sh 的 mysql 组,需要本机 MySQL):
// 会话 SQL 在编辑器打开、慢 SQL 一键 Explain(占位符换成参数填值)、未使用索引生成 DROP、OPTIMIZE TABLE 先确认。

import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1400, height: 900, locale: "en-US" };
const has = (app: App, part: string) => app.texts().some((s) => s.includes(part));

async function diag(): Promise<App> {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  await app.press("id:open-diag");
  await app.waitFor("id:diag-cards", { timeout: 15000 });
  return app;
}

test("会话的 SQL 可在编辑器打开", async () => {
  const app = await diag();
  await app.press("id:diag-tab-sessions");
  await app.waitUntil(() => has(app, "datum_sleeper"), { timeout: 10000 });
  await app.press("id:diag-sql-menu >> nth=0");
  await app.press("label:Open in Editor");
  await app.waitFor("id:tab:SQL Query 2", { timeout: 5000 });
  await app.waitUntil(() => has(app, "SELECT SLEEP(40) AS datum_sleeper"), { timeout: 5000 });
  app.dispose();
});

test("慢 SQL:Explain 用带真实值的样例语句,出执行计划", async () => {
  const app = await diag();
  await app.press("id:diag-tab-slow");
  await app.waitUntil(() => has(app, "FROM `big` `a` JOIN `big` `b`"), { timeout: 10000 });
  await app.press("id:diag-sql-menu >> nth=0");
  await app.press("label:Explain");
  // 执行计划画成树:MySQL 的 FORMAT=TREE 里有聚合与连接
  await app.waitUntil(() => has(app, "Aggregate"), { timeout: 10000 });
  app.dispose();
});

test("未使用的索引生成 DROP INDEX(放进编辑器不执行);OPTIMIZE TABLE 先确认", async () => {
  const app = await diag();
  await app.press("id:diag-tab-unused_indexes");
  await app.waitUntil(() => has(app, "idx_unused_name"), { timeout: 10000 });
  await app.press("id:diag-sql-menu >> nth=0");
  await app.press("label:Draft DROP INDEX");
  await app.waitUntil(() => has(app, "ALTER TABLE `products` DROP INDEX `idx_unused_name`;"), { timeout: 5000 });
  // 回到诊断 tab 做 OPTIMIZE
  await app.press("id:tab:Diagnostics");
  await app.press("id:diag-tab-tables");
  await app.waitUntil(() => app.shows("id:diag-optimize_table"), { timeout: 10000 });
  await app.press("id:diag-optimize_table >> nth=0");
  await app.waitFor("id:diag-confirm", { timeout: 5000 });
  expect(has(app, "Optimize table big?")).toBe(true);
  await app.press("id:diag-confirm-ok");
  await app.waitUntil(() => has(app, "Table optimized"), { timeout: 15000 });
  app.dispose();
});
