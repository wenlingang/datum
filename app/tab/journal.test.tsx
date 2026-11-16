// 写操作记录(headless):编辑器里执行 UPDATE → 写操作记录标签页里看到这一条与改动前的行 →
// 从记录撤销(核对现在的行)→ 标为已撤销。另留一条没撤销的写操作给 journal-restart.test.tsx(同一配置目录、新进程)。
// 夹具同 tab.test.tsx:临时目录里的 roastery SQLite(products 250 行)。

import { Journal } from "datum/native";
import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1280, height: 820, locale: "en-US" };
const hasText = (app: App, prefix: string) => app.texts().some((s) => s.startsWith(prefix));

async function start(): Promise<App> {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  return app;
}

let current = "-- Top products by revenue";
async function runSql(app: App, sql: string, want: string) {
  await app.press(`text:${current}`);
  current = sql;
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", sql);
  await app.press("id:run");
  await app.waitUntil(() => app.texts().some((x) => x.includes(want)), { timeout: 15000 });
}

test("编辑器的 UPDATE 记进写操作记录,可从记录撤销并标为已撤销", async () => {
  const app = await start();
  const update = "UPDATE products SET origin = 'Journal test' WHERE id <= 2";
  await runSql(app, update, "✓ Executed successfully · 2 rows affected");
  // 只读语句不记(开头带注释的查询也不记)
  await runSql(app, "-- 说明\nSELECT 'no' || 'ted' AS c", "noted");
  current = "SELECT 'no' || 'ted' AS c";
  await runSql(app, "SELECT COUNT(*) || ' journal' AS n FROM products WHERE origin = 'Journal test'", "2 journal");

  await app.press("id:open-journal");
  await app.waitFor("id:journal-view", { timeout: 5000 });
  await app.waitUntil(() => app.texts().includes(update), { timeout: 5000 });
  expect(app.texts().some((x) => x.startsWith("SELECT COUNT(*) || ' journal'"))).toBe(false);
  expect(app.texts().some((x) => x.includes("SELECT 'no' || 'ted'"))).toBe(false);
  expect(app.texts()).toContain("Today");
  expect(app.texts()).toContain("Can Undo");

  // 选中:完整语句与改动前的行
  await app.press(`id:journal-list >> text:${update}`);
  await app.waitFor("id:journal-snapshot", { timeout: 5000 });
  expect(app.texts()).toContain("Rows before the change · products");
  expect(app.shows("id:journal-copy")).toBe(true);
  expect(app.shows("id:journal-open")).toBe(true);
  await app.press("id:journal-copy");
  expect(app.clipboard()).toBe(update);

  // 筛选:失败的没有;搜索不到的没有
  await app.press("id:journal-status-error");
  await app.waitFor("id:journal-empty", { timeout: 5000 });
  expect(app.texts()).toContain("No writes match the filters");
  await app.press("id:journal-status-all");
  await app.waitUntil(() => app.texts().includes(update), { timeout: 5000 });

  // 撤销:核对现在的行 → 执行 → 标为已撤销
  await app.press(`id:journal-list >> text:${update}`);
  await app.waitFor("id:journal-undo", { timeout: 5000 });
  await app.press("id:journal-undo");
  await app.waitFor("id:undo-dialog", { timeout: 5000 });
  await app.waitFor("id:undo-check-ok", { timeout: 10000 });
  expect(app.texts()).toContain("Undo a Recorded Change");
  await app.press("id:undo-confirm");
  await app.waitFor("text:Undone: 2 rows restored", { timeout: 15000 });
  await app.waitUntil(() => app.texts().includes("Undone"), { timeout: 5000 });
  await app.waitFor("id:journal-undone-note", { timeout: 5000 });
  expect(app.shows("id:journal-undo")).toBe(false);
  // 撤销本身也是一条记录(来源:撤销)
  await app.waitUntil(() => app.texts().some((x) => x.startsWith("UPDATE products SET \"origin\" = 'Kenya' WHERE \"id\" = 1")), { timeout: 5000 });

  // 数据回到原样
  await app.press("id:tab:SQL Query");
  await runSql(app, "SELECT COUNT(*) || ' left' AS n FROM products WHERE origin = 'Journal test'", "0 left");

  // 留一条不撤销的,给重启测试
  await runSql(app, "UPDATE products SET origin = 'Restart test' WHERE id = 5", "✓ Executed successfully · 1 rows affected");
  // 失败的写操作也留痕
  await runSql(app, "UPDATE no_such_table SET x = 1 WHERE id = 1", "no such table");
  const list = (await Journal.list("dev-sqlite", null)) as { sql: string; ok: boolean; undoneBy: string | null }[];
  expect(list.find((e) => e.sql === update)?.undoneBy).not.toBe(null);
  expect(list.find((e) => e.sql.startsWith("UPDATE no_such_table"))?.ok).toBe(false);
  app.dispose();
});

test("表格编辑提交也留痕,带撤销", async () => {
  const app = await start();
  await app.press("id:tab:products");
  await app.waitUntil(() => app.texts().includes("Coffee 3"), { timeout: 15000 });
  await app.rightClick("text:Coffee 3");
  await app.chooseMenuItem("Edit");
  await app.waitFor("id:cell-editor");
  await app.type("id:cell-editor", "Grid ");
  await app.press("id:cell-stage");
  await app.waitFor("text:Staged");
  await app.press("label:x");
  await app.waitFor("text:Edit Cell", { state: "gone" });
  await app.keyboard.press("mod+s");
  await app.waitUntil(() => app.texts().includes("Grid Coffee 3"), { timeout: 15000 });
  // 提交后表格标签页里就能撤销
  await app.waitFor("text:Can be undone: restores the 1 rows as they were before", { timeout: 5000 });
  await app.press("id:open-journal");
  await app.waitFor("id:journal-view", { timeout: 5000 });
  await app.press("label:All Sources");
  await app.press("text:Grid Edit");
  await app.waitUntil(() => !app.texts().includes("UPDATE products SET origin = 'Journal test' WHERE id <= 2"), { timeout: 5000 });
  await app.waitUntil(() => app.texts().some((x) => x.startsWith("UPDATE \"products\" SET \"name\" = 'Grid Coffee 3'")), { timeout: 5000 });
  expect(app.texts()).toContain("Can Undo");
  app.dispose();
});
