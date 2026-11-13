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
  await app.waitFor("text:Ask about this database");
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
  await app.press("id:ai-new");
  await app.waitFor("text:Ask about this database");
  app.dispose();
});

test("流式中途出错:错误追加在已收到的文本之后", async () => {
  const app = await open("./testing/error.tsx", SIZE);
  await app.type("id:ai-input", "hi");
  await app.submit("id:ai-input");
  await app.waitFor("text:network down", { timeout: 10_000 });
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

  // 直接执行:先弹确认框(含破坏性警告与影响范围预估),取消不执行
  await app.press("id:sql-run-2:1");
  await app.waitFor("id:sql-confirm");
  expect(app.shows("text:/Contains destructive statements/")).toBe(true);
  await app.waitFor("id:sql-confirm-impact >> text:The table currently has 7 rows; all of them will be removed");
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
  // DROP 不能撤销:卡片上说明原因
  expect(app.shows("id:sql-card-2:1 >> text:/Can't be undone: TRUNCATE and DROP TABLE/")).toBe(true);

  // 查询直接执行:结果在对话里成表
  await app.press("id:sql-run-2:3");
  await app.waitFor("id:sql-confirm");
  // 只读语句没有影响范围一栏
  expect(app.shows("id:sql-confirm-impact")).toBe(false);
  await app.press("id:sql-confirm-ok");
  await app.waitFor("id:ai-result");
  expect(app.shows("id:ai-result >> text:12")).toBe(true);

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
  await app.waitFor("text:Ask about this database");
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

test("AI 自己查数据:工具步骤按顺序显示,可展开看 SQL 与结果;写语句仍是待确认卡片", async () => {
  const app = await open("./testing/agent.tsx", SIZE);
  await app.type("id:ai-input", "订单状态分布怎样?");
  await app.press("id:ai-send");
  await app.waitFor("text:/已支付 42 单/", { timeout: 10_000 });
  expect(app.shows("text:我先查一下订单状态分布。")).toBe(true);
  // 两个工具步骤:成功的显示摘要,失败的标红
  expect(app.shows("id:ai-tool-1")).toBe(true);
  expect(app.shows("id:ai-tool-1 >> text:2 行")).toBe(true);
  expect(app.shows("id:ai-tool-2 >> text:Failed")).toBe(true);
  // 默认收起;点开看完整 SQL 与结果表
  expect(app.shows("text:cancelled")).toBe(false);
  await app.press("id:ai-tool-1 >> label:Query");
  await app.waitFor("text:cancelled");
  expect(app.shows("text:42")).toBe(true);
  // 结果表可复制为 CSV、导出成文件(导出的是卡片保存的全部行)
  await app.press("id:tool-result-actions >> role:button[Copy as CSV]");
  await app.waitFor("text:copyCsv status|n 2");
  await app.press("id:tool-result-actions >> role:button[Export]");
  await app.press("label:Excel");
  await app.waitFor("text:export xlsx 2");
  // 写语句不会被自动执行:以 SQL 卡片出现,等用户点「Run」
  expect(app.texts()).toContain("DELETE FROM orders WHERE status = ");
  expect(app.texts()).toContain("Run");
  app.dispose();
});

test("模型不支持工具:自动改用普通对话", async () => {
  const app = await open("./testing/notools.tsx", SIZE);
  await app.type("id:ai-input", "建一张 users 表");
  await app.press("id:ai-send");
  await app.waitFor("text:/然后初始化几条数据/", { timeout: 10_000 });
  expect(app.texts().some((x) => x.includes("tools are not supported"))).toBe(false);
  app.dispose();
});

test("停止、重新生成:停止后保留已收到的内容;重新生成替换最后一轮", async () => {
  // 手动时钟:回复只在测试推进时间时流出,才能在中途点停止
  const app = await open("./testing/slow.tsx", { ...SIZE, clock: {} });
  const clock = app.clock!;
  await app.type("id:ai-input", "讲讲这个库");
  await app.press("id:ai-send");
  await clock.runFor(300);
  // 回复进行中:发送按钮换成停止,「思考中」已被文字取代
  expect(app.shows("id:ai-stop")).toBe(true);
  await app.press("id:ai-stop");
  await clock.runFor(500);
  await app.waitFor("id:ai-stopped");
  expect(app.shows("id:ai-send")).toBe(true);
  // 完整回复是「第 1 次回答:」+ 40 个「内容」;停止后只剩一部分
  const partial = app.texts().find((x) => x.startsWith("第 1 次")) ?? "";
  expect(partial.length).toBeGreaterThan(0);
  expect(partial.length).toBeLessThan("第 1 次回答:".length + 80);
  // 重新生成:同一个问题再问一次,旧回复被替换,用户消息不重复
  await app.press("label:Regenerate");
  await clock.runFor(5_000);
  await app.waitFor("text:/第 2 次回答/");
  expect(app.texts().some((x) => x.startsWith("第 1 次"))).toBe(false);
  expect(app.texts().filter((x) => x === "讲讲这个库").length).toBe(1);
  app.dispose();
});

test("出错可重试;空会话的建议点一下就发出", async () => {
  const app = await open("./testing/error.tsx", SIZE);
  await app.press("id:ai-suggestion-0");
  await app.waitFor("text:What tables does this database have, and what does each one store?", { timeout: 5_000 });
  await app.waitFor("id:ai-error", { timeout: 10_000 });
  expect(app.shows("id:ai-retry")).toBe(true);
  await app.press("id:ai-retry");
  await app.waitFor("id:ai-error", { timeout: 10_000 });
  // 重试没有重复堆出用户消息
  expect(app.texts().filter((x) => x.startsWith("What tables does this database have")).length).toBe(1);
  app.dispose();
});

test("对话工作台里查询结果默认展开,可在编辑器中打开", async () => {
  const app = await open("./testing/agent-wide.tsx", SIZE);
  await app.type("id:ai-input", "订单状态分布怎样?");
  await app.press("id:ai-send");
  await app.waitFor("text:/已支付 42 单/", { timeout: 10_000 });
  // 成功的查询默认展开,失败的不展开
  expect(app.shows("text:cancelled")).toBe(true);
  expect(app.shows("text:no such table: nope")).toBe(false);
  await app.press("role:button[Open in Editor]");
  await app.waitFor("text:/openQuery c1 SELECT status/");
  app.dispose();
});

test("AI 卡片上执行的写操作可撤销,撤销结果告知 AI", async () => {
  const app = await open("./testing/agent.tsx", SIZE);
  await app.type("id:ai-input", "清理取消的订单");
  await app.press("id:ai-send");
  await app.waitFor("text:/已支付 42 单/", { timeout: 10_000 });
  expect(app.texts()).toContain("DELETE FROM orders WHERE status = ");
  await app.press("role:button[Run]");
  await app.waitFor("id:sql-confirm");
  await app.press("id:sql-confirm-ok");
  await app.waitFor("text:Can be undone: restores the 2 rows as they were before", { timeout: 10_000 });
  await app.press("role:button[Undo]");
  await app.waitFor("text:/Restore the 2 rows as they were before\\?/");
  await app.press("role:button[Undo Changes]");
  await app.waitFor("text:✓ Undone: 2 rows restored", { timeout: 10_000 });
  // 撤销以隐藏消息记进对话
  expect(app.texts().some((x) => x.startsWith("[Undone]"))).toBe(true);
  app.dispose();
});

test("新对话保留历史:可从历史菜单切回;对话跨重启保留", async () => {
  const first = await open("./testing/persist.tsx", SIZE);
  await first.waitFor("text:Ask about this database");
  await first.type("id:ai-input", "第一段对话的问题");
  await first.press("id:ai-send");
  await first.waitFor("text:/然后初始化几条数据/", { timeout: 10_000 });
  // 新对话:当前对话进历史,面板回到空状态
  await first.press("id:ai-new");
  await first.waitFor("text:Ask about this database");
  await first.type("id:ai-input", "第二段对话的问题");
  await first.press("id:ai-send");
  await first.waitFor("text:第二段对话的问题");
  await first.waitUntil(() => first.texts().filter((s) => /然后初始化几条数据/.test(s)).length > 0, { timeout: 10_000 });
  // 从历史菜单切回第一段
  await first.press("label:History");
  await first.press("label:/^第一段对话的问题/");
  await first.waitFor("text:第一段对话的问题");
  expect(first.texts()).not.toContain("第二段对话的问题");
  // 等防抖写盘后「重启」
  await first.wait(1600);
  first.dispose();

  const second = await open("./testing/persist.tsx", SIZE);
  await second.waitFor("text:第一段对话的问题", { timeout: 10_000 });
  await second.press("label:History");
  await second.waitFor("label:/^第二段对话的问题/");
  second.dispose();
});
