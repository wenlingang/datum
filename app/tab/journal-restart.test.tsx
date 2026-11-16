// 写操作记录跨重启(headless):在 journal.test.tsx 之后、同一配置目录里用新进程跑。
// 上次留下的 UPDATE(没撤销)重启后仍在写操作记录里,查询历史里照样能撤销(撤销数据来自写操作记录),
// 确认框提醒「发生在上次重启之前」,撤销后记录标为已撤销。

import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1280, height: 820, locale: "en-US" };

async function start(): Promise<App> {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  return app;
}

test("重启后从查询历史撤销上次的写操作(撤销数据来自写操作记录)", async () => {
  const app = await start();
  const update = "UPDATE products SET origin = 'Restart test' WHERE id = 5";
  await app.press("id:history");
  await app.waitUntil(() => app.texts().some((x) => x.startsWith("Undo (1 rows)")), { timeout: 5000 });
  await app.press("text:Undo (1 rows)");
  await app.waitFor("id:undo-dialog", { timeout: 5000 });
  await app.waitFor("id:undo-check-previous", { timeout: 10000 });
  expect(app.texts()).toContain("This change was made before Datum last restarted. The rows may have changed since.");
  expect(app.texts()).toContain("Checked by primary key: every row to restore is in place.");
  await app.press("id:undo-confirm");
  await app.waitFor("text:Undone: 1 rows restored", { timeout: 15000 });

  // 写操作记录里标为已撤销,也看得到上次运行的其他记录
  await app.press("id:open-journal");
  await app.waitFor("id:journal-view", { timeout: 5000 });
  await app.waitUntil(() => app.texts().includes(update), { timeout: 5000 });
  expect(app.texts()).toContain("UPDATE products SET origin = 'Journal test' WHERE id <= 2");
  await app.press(`text:${update}`);
  await app.waitFor("id:journal-undone-note", { timeout: 5000 });

  // 历史里不再提供这条的撤销
  await app.press("id:tab:SQL Query");
  await app.press("id:history");
  await app.waitUntil(() => app.texts().some((x) => x.includes(update)), { timeout: 5000 });
  expect(app.texts().some((x) => x.startsWith("Undo ("))).toBe(false);
  app.dispose();
});
