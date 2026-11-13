// 真实 MySQL 上的字符集体检(run-tests.sh 的 mysql 组,需要本机 MySQL):
// utf8mb3 的表与列、外键两侧不一致被列出;行内修复语句与整库转换脚本放进编辑器,
// 执行时经过改表结构风险评估(CONVERT 是 COPY、高风险)。

import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1400, height: 900, locale: "en-US" };
const has = (app: App, part: string) => app.texts().some((s) => s.includes(part));
const CONVERT =
  "ALTER TABLE `datum_ui_test`.`legacy_users` CONVERT TO CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci;";

async function charset(): Promise<App> {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  await app.press("id:open-diag");
  await app.waitFor("id:diag-cards", { timeout: 15000 });
  await app.press("id:diag-tab-charset");
  // 体检要扫 information_schema:不自动查,点「运行」
  expect(app.texts()).toContain("Run");
  await app.press("id:diag-refresh");
  await app.waitUntil(() => has(app, "Table uses utf8mb3"), { timeout: 15000 });
  return app;
}

test("列出 utf8mb3 的表与列、外键两侧不一致,按严重程度排序,行内生成修复语句", async () => {
  const app = await charset();
  expect(has(app, "legacy_users.name")).toBe(true);
  expect(has(app, "Column uses utf8mb3")).toBe(true);
  expect(has(app, "legacy_users.user_name → members.user_name")).toBe(true);
  // 外键两侧不一致(高)排在 utf8mb3(中)之前
  const texts = app.texts();
  expect(texts.indexOf("Foreign key mismatch")).toBeGreaterThanOrEqual(0);
  expect(texts.indexOf("Foreign key mismatch")).toBeLessThan(texts.indexOf("Table uses utf8mb3"));
  await app.press("id:diag-sql-menu >> nth=0");
  await app.press("label:Draft Fix");
  await app.waitFor("id:tab:SQL Query 2", { timeout: 5000 });
  await app.waitUntil(() => has(app, "MODIFY `user_name` varchar(40) CHARACTER SET utf8mb4"), { timeout: 5000 });
  app.dispose();
});

test("整库脚本放进编辑器;执行前弹出改表风险评估:COPY、高风险,取消不执行", async () => {
  const app = await charset();
  await app.press("id:diag-script");
  await app.waitFor("id:tab:SQL Query 2", { timeout: 5000 });
  await app.waitUntil(() => has(app, CONVERT), { timeout: 5000 });
  await app.press("id:run");
  await app.waitFor("id:risk-alter-risk", { timeout: 15000 });
  expect(app.texts()).toContain("High risk");
  expect(has(app, "COPY")).toBe(true);
  await app.press("id:risk-cancel");
  await app.waitFor("id:risk-dialog", { state: "gone" });
  expect(has(app, "Executed successfully")).toBe(false);
  app.dispose();
});
