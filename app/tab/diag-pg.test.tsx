// 真实 PostgreSQL 上的诊断操作(run-tests.sh 的 pg 组,需要开了 pg_stat_statements 的 PG):
// 慢 SQL 的 $1 换成参数、填值后 Explain;未使用的索引生成 DROP INDEX CONCURRENTLY,执行前过危险语句审查。

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

test("慢 SQL:$1 换成 :p1,填值后出执行计划", async () => {
  const app = await diag();
  await app.press("id:diag-tab-slow");
  await app.waitUntil(() => has(app, "JOIN big b ON a.v < b.v WHERE a.id > $1"), { timeout: 10000 });
  await app.press("id:diag-sql-menu >> nth=0");
  await app.press("label:Explain");
  await app.waitFor("id:params-dialog", { timeout: 5000 });
  await app.type("id:param:p1", "10");
  await app.press("id:params-run");
  await app.waitUntil(() => has(app, "Nested Loop") || has(app, "Hash Join") || has(app, "Aggregate"), { timeout: 10000 });
  app.dispose();
});

test("未使用的索引:生成 DROP INDEX CONCURRENTLY,执行前先确认,执行后索引消失", async () => {
  const app = await diag();
  await app.press("id:diag-tab-unused_indexes");
  await app.waitUntil(() => has(app, "idx_unused_name"), { timeout: 10000 });
  await app.press("id:diag-sql-menu >> nth=0");
  await app.press("label:Draft DROP INDEX");
  await app.waitUntil(() => has(app, 'DROP INDEX CONCURRENTLY "public"."idx_unused_name";'), { timeout: 5000 });
  await app.press("id:run");
  await app.waitFor("id:risk-dialog", { timeout: 5000 });
  await app.press("id:risk-confirm");
  await app.waitUntil(() => has(app, "Executed successfully") || has(app, "Query failed"), { timeout: 10000 });
  expect(has(app, "Query failed")).toBe(false);
  await app.press("id:tab:Diagnostics");
  await app.press("id:diag-refresh");
  await app.waitUntil(() => !has(app, "idx_unused_name"), { timeout: 10000 });
  app.dispose();
});
