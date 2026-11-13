// 真实 PostgreSQL 上的 JSONB(run-tests.sh 的 pg 组,表 events 250 行,payload 为 jsonb):
// 行详情 / 单元格弹窗把 JSON 展开成树,复制路径与值;树上点字段生成筛选,表浏览下推到服务端;
// 查询结果在本地按路径筛;编辑时校验 JSON、格式化 / 压缩;编辑器里 `payload->'` 补全键名并逐级提示。

import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1400, height: 900, locale: "en-US" };
const has = (app: App, s: string) => app.texts().includes(s);

async function start(): Promise<App> {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  return app;
}

async function openEvents(app: App) {
  await app.waitFor("id:open:events", { timeout: 15000 });
  await app.press("id:open:events");
  await app.waitUntil(() => has(app, "n1") && has(app, "JSONB"), { timeout: 15000 });
}

test("表浏览:行详情把 jsonb 展开成树,点字段生成筛选并下推到服务端(对整张表生效)", async () => {
  const app = await start();
  await openEvents(app);
  await app.press("label:detail");
  // 详情栏打开后网格变窄,note 列在可视区外:点 payload 单元格选中第 1 行
  await app.press(`text:/"zipcode": 10001\\}/`);
  await app.waitFor("id:row-json:payload", { timeout: 5000 });
  // 默认只展开第一层:customer 收起
  expect(app.shows("id:json-node:customer.zipcode")).toBe(false);
  await app.press("id:json-node:customer");
  await app.waitFor("id:json-node:customer.zipcode", { timeout: 5000 });
  await app.press("id:json-node:customer.zipcode");
  await app.press("id:json-filter");
  // 条件填进筛选条:列后标出 PG 取值表达式,按数值包含查询下推,只剩第 1 行
  await app.waitFor("id:filter-server-hint", { timeout: 5000 });
  await app.waitUntil(() => has(app, `->'customer'->>'zipcode'`), { timeout: 5000 });
  await app.waitUntil(() => has(app, "n1") && !has(app, "n2"), { timeout: 10000 });
  // 改成第 250 行的值:它在第 3 页,服务端筛选才找得到
  expect(app.value("id:filter-value:1")).toBe("10001");
  await app.clear("id:filter-value:1");
  await app.type("id:filter-value:1", "10250");
  await app.press("id:filter-apply");
  await app.waitUntil(() => has(app, "n250") && !has(app, "n1"), { timeout: 10000 });
  app.dispose();
});

test("单元格弹窗:默认树形,复制路径与值;切到文本可格式化 / 压缩,不合法的 JSON 不能暂存", async () => {
  const app = await start();
  await openEvents(app);
  await app.rightClick(`text:/"zipcode": 10001\\}/`);
  expect(app.menu()?.map((e) => e.label)).toContain("Expand JSON");
  await app.chooseMenuItem("Expand JSON");
  await app.waitFor("id:cell-json-tree", { timeout: 5000 });
  // 弹窗默认展开两层
  await app.press("id:cell-json-tree >> id:json-node:customer.tier");
  await app.press("id:json-copy-value");
  await app.waitUntil(() => app.clipboard() === "gold", { timeout: 3000 });
  await app.press("id:json-copy-path");
  await app.waitUntil(() => app.clipboard() === `"payload"->'customer'->>'tier'`, { timeout: 3000 });
  // 文本:格式化 / 压缩(键顺序与数字写法不变)
  await app.press("id:cell-view-text");
  await app.waitFor("id:cell-json-valid", { timeout: 3000 });
  await app.press("id:cell-json-compact");
  const compact = `{"kind":"view","flags":[false],"amount":10,"customer":{"tier":"gold","zipcode":10001}}`;
  await app.waitUntil(() => app.texts().some((s) => s.includes(compact)), { timeout: 3000 });
  await app.press("id:cell-json-format");
  await app.waitUntil(() => !app.texts().some((s) => s.includes(compact)), { timeout: 3000 });
  // 改成不合法的 JSON:标出位置与原因,暂存按钮不可用
  await app.clear("id:cell-editor");
  await app.type("id:cell-editor", "nope");
  await app.waitFor("id:cell-json-error", { timeout: 3000 });
  expect(app.texts().some((s) => s.startsWith("Line 1, column 1"))).toBe(true);
  expect(app.enabled("id:cell-stage")).toBe(false);
  app.dispose();
});

test("查询结果:树上点字段在本地按路径筛选", async () => {
  const app = await start();
  await app.press("text:-- Top products by revenue");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "SELECT note, payload FROM events ORDER BY id LIMIT 6");
  await app.press("id:run");
  await app.waitUntil(() => has(app, "n6"), { timeout: 15000 });
  await app.press("label:detail");
  await app.press("text:n2");
  await app.waitFor("id:row-json:payload", { timeout: 5000 });
  await app.press("id:json-node:kind");
  await app.press("id:json-filter");
  // kind = "click" 的是偶数行;查询结果不下推,本地筛
  await app.waitUntil(() => has(app, "n4") && !has(app, "n1") && !has(app, "n3"), { timeout: 5000 });
  expect(app.shows("id:filter-server-hint")).toBe(false);
  expect(has(app, "n6")).toBe(true);
  app.dispose();
});

test("补全:payload->' 之后提示取样推断出的键名,逐级提示嵌套的键", async () => {
  const app = await start();
  await app.press("text:-- Top products by revenue");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "SELECT * FROM events WHERE payload->'");
  await app.waitUntil(() => has(app, "customer") && has(app, "amount"), { timeout: 10000 });
  await app.press("text:customer");
  await app.waitUntil(() => app.texts().some((s) => s.includes("payload->'customer'")), { timeout: 5000 });
  await app.type("id:sql-editor", "->'");
  await app.waitUntil(() => has(app, "zipcode") && has(app, "tier"), { timeout: 10000 });
  expect(has(app, "amount")).toBe(false);
  await app.press("text:zipcode");
  await app.waitUntil(() => app.texts().some((s) => s.includes("payload->'customer'->'zipcode'")), { timeout: 5000 });
  app.dispose();
});
