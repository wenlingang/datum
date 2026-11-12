// AI 面板:未配置引导、发送后流式拼接、错误追加、SQL 卡片渲染与风险确认。
// 入口在 testing/ 下,装的是假的 AI 后端与连接依赖,不碰真实配置与服务。

import { expect, open, test } from "nsh/test";

const SIZE = { width: 1000, height: 760, platform: "macos" as const };

test("未配置 AI 时引导去设置", async () => {
  const app = await open("./testing/unconfigured.tsx", SIZE);
  await app.waitFor("text:AI provider not configured. Add one in Settings.");
  expect(app.shows("id:ai-open-settings")).toBe(true);
  expect(app.shows("text:AI isn't ready. Check Settings.")).toBe(true);
  expect(app.enabled("id:ai-input")).toBe(false);
  expect(app.enabled("id:ai-send")).toBe(false);
  expect(app.shows("text:Select Model")).toBe(true);
  app.dispose();
});

test("发送后流式拼接回复,并渲染 SQL 卡片", async () => {
  const app = await open("./testing/streaming.tsx", SIZE);
  await app.waitFor("text:Generate SQL with natural language, for example:");
  expect(app.shows("text:gpt-4o")).toBe(true);
  expect(app.enabled("id:ai-send")).toBe(false);
  await app.type("id:ai-input", "建一张 users 表");
  expect(app.enabled("id:ai-send")).toBe(true);
  await app.press("id:ai-send");
  expect(app.value("id:ai-input")).toBe("");
  await app.waitFor("text:建一张 users 表");
  // Done 没带全文:最终文本是 Delta 的拼接
  await app.waitFor("text:/然后初始化几条数据/", { timeout: 10_000 });
  expect(app.shows("text:可以这样建表:")).toBe(true);
  expect(app.shows("id:sql-card-2:1")).toBe(true);
  expect(app.shows("id:sql-card-2:1 >> text:AI Generated")).toBe(true);
  expect(app.enabled("id:sql-run-2:1")).toBe(true);
  // 绑定连接显示在标题栏,清空按钮出现
  expect(app.shows("text:local / shop")).toBe(true);
  await app.press("id:ai-clear");
  await app.waitFor("text:Generate SQL with natural language, for example:");
  app.dispose();
});

test("流式中途出错:错误追加在已收到的文本之后", async () => {
  const app = await open("./testing/error.tsx", SIZE);
  await app.type("id:ai-input", "hi");
  await app.submit("id:ai-input");
  await app.waitFor("text:⚠️ network down", { timeout: 10_000 });
  expect(app.shows("text:部分回复")).toBe(true);
  // 流结束后可以继续发送
  await app.type("id:ai-input", "again");
  expect(app.enabled("id:ai-send")).toBe(true);
  app.dispose();
});

test("SQL 卡片:风险标记、执行确认、结果回灌、编辑器运行", async () => {
  const app = await open("./testing/risky.tsx", SIZE);
  await app.type("id:ai-input", "删掉 users");
  await app.press("id:ai-send");
  await app.waitFor("id:sql-card-2:3", { timeout: 10_000 });
  // 破坏性语句标红,只读语句不标
  expect(app.shows("id:sql-card-2:1 >> text:Destructive")).toBe(true);
  expect(app.shows("id:sql-card-2:3 >> text:Destructive")).toBe(false);

  // 直接执行:先弹确认框(含破坏性警告),取消不执行
  await app.press("id:sql-run-2:1");
  await app.waitFor("id:sql-confirm");
  expect(app.shows("text:/Contains destructive statements/")).toBe(true);
  expect(app.shows("text:The following statement will run on local / shop:")).toBe(true);
  await app.press("id:sql-confirm-cancel");
  await app.waitFor("id:sql-confirm", { state: "gone" });
  expect(app.shows("text:/\\[Executed\\]/")).toBe(false);

  // 确认后执行,结果以隐藏消息回灌对话
  await app.press("id:sql-run-2:1");
  await app.waitFor("id:sql-confirm");
  await app.press("id:sql-confirm-ok");
  await app.waitFor("text:/\\[Executed\\] DROP TABLE users;/");
  await app.waitFor("id:sql-card-2:1 >> text:Executed");
  expect(app.enabled("id:sql-run-2:1")).toBe(false);

  // 只读语句「编辑器运行」免确认
  await app.press("id:sql-card-2:3 >> role:button[Run in Editor]");
  await app.waitFor("text:insertAndRun c1 confirmed=false");
  expect(app.shows("id:sql-confirm")).toBe(false);

  // 破坏性语句「编辑器运行」要确认,确认后带 confirmed
  await app.press("id:sql-card-2:1 >> role:button[Run in Editor]");
  await app.waitFor("id:sql-confirm");
  expect(app.shows("text:/replaces all content in the query editor/")).toBe(true);
  await app.press("id:sql-confirm-ok");
  await app.waitFor("text:insertAndRun c1 confirmed=true");
  app.dispose();
});

test("编辑器报错 / Explain 交给 AI:引用 SQL、附报错或执行计划,自动发出", async () => {
  const app = await open("./testing/ask.tsx", SIZE);
  await app.waitFor("text:Generate SQL with natural language, for example:");
  await app.press("id:trigger-fix");
  // 发出的消息:提问 + 报错原文 + 引用的 SQL
  await app.waitUntil(() => app.texts().some((x) => x.includes('column "nme" does not exist')), { timeout: 10_000 });
  expect(app.texts().some((x) => x.includes("This SQL failed to run"))).toBe(true);
  expect(app.texts().some((x) => x.includes("SELECT nme FROM users"))).toBe(true);
  // 等回复结束再发下一条
  await app.waitFor("text:/然后初始化几条数据/", { timeout: 10_000 });
  await app.press("id:trigger-plan");
  await app.waitUntil(() => app.texts().some((x) => x.includes("Seq Scan on orders")), { timeout: 10_000 });
  expect(app.texts().some((x) => x.includes("CREATE INDEX"))).toBe(true);
  expect(app.texts().some((x) => x.includes("SELECT * FROM orders WHERE total > 10"))).toBe(true);
  app.dispose();
});
