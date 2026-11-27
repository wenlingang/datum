// 结果网格的列宽(DataTable 的 autoFit + grow):数字列按内容适配不伸展,文本列分掉剩余宽度铺满结果区;
// 很长的值最多适配到一屏宽(maxWidth = max(600, 结果区宽度)),超出结果区时横向滚动;新结果的列重新适配。
import { expect, open, test, type App } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1200, height: 800, locale: "en-US" };
const hasText = (app: App, prefix: string) => app.texts().some((s) => s.startsWith(prefix));

/** 点编辑器里现有的一段文字聚焦它,全选后换成 `sql` 运行。 */
async function runSql(app: App, focusText: string, sql: string, rows: string) {
  await app.press(`text:${focusText}`);
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", sql);
  await app.keyboard.press("mod+enter");
  await app.waitUntil(() => hasText(app, `Query succeeded · ${rows}`), { timeout: 15000 });
}

const width = (app: App, col: string) => app.layout(`id:th:${col}`)?.width ?? 0;

test("数字列按内容、文本列铺满结果区;长值适配到上限;新结果重新适配", async () => {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });

  await runSql(app, "-- Top products by revenue", "SELECT id, name FROM categories", "4 rows");
  await app.waitUntil(() => width(app, "name") > 0);
  const grid = app.layout("id:result-grid")!;
  expect(width(app, "id")).toBeLessThan(150);
  // 只有一个文本列也铺满(不被 600 卡住)
  expect(Math.abs(width(app, "id") + width(app, "name") - grid.width)).toBeLessThanOrEqual(2);

  // 同一位置的列换成很长的值:新结果重新适配,宽度停在一屏宽,横向滚动
  const long = "x".repeat(400);
  await runSql(app, "SELECT id, name FROM categories", `SELECT id, '${long}' AS name, '${long}' AS other FROM categories`, "4 rows");
  const cap = Math.max(600, Math.floor(grid.width));
  // name 与上一个结果的 name 同位置同名:也按新值重新适配(other 被挤到屏幕外,表头不渲染,不读它)
  await app.waitUntil(() => width(app, "name") === cap, { timeout: 3000 });
  expect(width(app, "id")).toBeLessThan(150);
});
