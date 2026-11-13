// 真实 MySQL 上的改表结构风险评估(run-tests.sh 的 mysql 组,需要本机 MySQL;连接是 local 环境):
// 低风险的改表直接执行;高风险(COPY / 锁写)先弹确认框,列出判定、表大小与保护写法;
// 本身就算破坏性的 ALTER … DROP 照常确认,并附上评估。

import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1400, height: 900, locale: "en-US" };
const has = (app: App, part: string) => app.texts().some((s) => s.includes(part));

async function start(sql: string): Promise<App> {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  await app.press("text:-- Top products by revenue");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", sql);
  await app.press("id:run");
  return app;
}

test("非 prod:能 INSTANT 的加列直接执行,不弹确认框", async () => {
  const app = await start("ALTER TABLE alter_demo ADD COLUMN c INT");
  await app.waitUntil(() => has(app, "Executed successfully"), { timeout: 15000 });
  expect(app.shows("id:risk-dialog")).toBe(false);
  app.dispose();
});

test("非 prod:改列类型只能 COPY,先弹确认框并给出保护写法;取消不执行", async () => {
  const sql = "ALTER TABLE alter_demo MODIFY v BIGINT";
  const app = await start(sql);
  await app.waitFor("id:risk-dialog", { timeout: 15000 });
  await app.waitFor("id:risk-alter-risk", { timeout: 15000 });
  expect(app.texts()).toContain("High risk");
  expect(app.texts()).toContain("COPY · rebuilds the table · blocks writes while running");
  expect(app.texts()).toContain("Modify column v — changing the data type needs a full table copy");
  expect(has(app, "gh-ost or pt-online-schema-change")).toBe(true);
  expect(app.texts()).toContain(`${sql}, ALGORITHM=INPLACE, LOCK=NONE`);
  expect(has(app, "MySQL ")).toBe(true);
  await app.press("id:risk-cancel");
  await app.waitFor("id:risk-dialog", { state: "gone" });
  expect(has(app, "Executed successfully")).toBe(false);
  app.dispose();
});

test("ALTER … DROP 照常确认,附上评估(删未建索引的列在 8.0.29+ 是 INSTANT)", async () => {
  const app = await start("ALTER TABLE alter_demo DROP COLUMN note");
  await app.waitFor("text:Review Destructive Statements", { timeout: 15000 });
  await app.waitFor("id:risk-alter-risk", { timeout: 15000 });
  expect(app.texts()).toContain("Low risk");
  expect(app.texts()).toContain("INSTANT · no table rebuild · reads and writes keep working");
  expect(has(app, "Metadata only")).toBe(true);
  await app.press("id:risk-confirm");
  await app.waitUntil(() => has(app, "Executed successfully"), { timeout: 15000 });
  app.dispose();
});
