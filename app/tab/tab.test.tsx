// 查询 / 表浏览标签页的流程测试(headless,用户的操作步骤)。
// 只连临时目录里的 SQLite:运行前先重建库,并设 DATUM_CONFIG_DIR 指向临时配置目录
// (其中 connections.json 只有一条指向该库的 sqlite 连接)。库里有 products 250 行、orders 400 行。
// 测试按顺序执行,后面的会改库(提交编辑、删除 orders)。

import { Store } from "datum/native";
import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1200, height: 800, locale: "en-US" };

const hasText = (app: App, prefix: string) => app.texts().some((s) => s.startsWith(prefix));

async function start(): Promise<App> {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  return app;
}

test("运行查询:显示行数、网格与历史", async () => {
  const app = await start();
  await app.press("id:run");
  // 语句以注释开头,core 的 is_query 判为非查询(与 GPUI 版一致:状态行显示「执行成功」),结果照常出网格
  await app.waitUntil(() => app.texts().includes("Coffee 133"), { timeout: 15000 });
  expect(hasText(app, "Executed successfully")).toBe(true);
  expect(app.shows("id:result-grid")).toBe(true);
  // 点自定义表头做客户端排序(升序):收入最低的排到最前
  const firstCoffee = () => app.texts().find((x) => x.startsWith("Coffee "));
  expect(firstCoffee()).toBe("Coffee 133");
  await app.press("text:revenue_usd");
  await app.waitUntil(() => firstCoffee() !== "Coffee 133", { timeout: 5000 });
  // 成功执行进历史
  await app.press("id:history");
  await app.waitUntil(() => hasText(app, "-- Top products by revenue"), { timeout: 5000 });
});

async function replaceSql(app: App, sql: string) {
  await app.press("text:-- Top products by revenue");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", sql);
}

test("格式化替换编辑器全文(编辑器不受控,程序修改经 setValue 推给编辑器)", async () => {
  const app = await start();
  await replaceSql(app, "select id, name from categories where id > 1");
  await app.keyboard.press("mod+shift+f");
  await app.waitUntil(() => app.texts().some((x) => x.startsWith("SELECT")), { timeout: 5000 });
  expect(app.texts().some((x) => x.includes("select id"))).toBe(false);
  // 格式化后的文本就是要执行的文本
  await app.press("id:run");
  await app.waitUntil(() => hasText(app, "Query succeeded · 3 rows returned"), { timeout: 15000 });
  app.dispose();
});

test("SELECT 语句:状态行报告返回行数", async () => {
  const app = await start();
  await replaceSql(app, "SELECT id, name FROM categories");
  await app.press("id:run");
  await app.waitUntil(() => hasText(app, "Query succeeded · 4 rows returned"), { timeout: 15000 });
  expect(app.texts()).toContain("Single Origin");
});

test("表浏览分页:每页 100 行,末页禁用下一页", async () => {
  const app = await start();
  await app.press("id:tab:products");
  await app.waitUntil(() => app.texts().includes("100") && hasText(app, "Page"), { timeout: 15000 });
  expect(app.texts()).toContain("1");
  await app.press("label:Next Page");
  await app.waitUntil(() => app.texts().includes("2"), { timeout: 10000 });
  await app.press("label:Next Page");
  await app.waitUntil(() => app.texts().includes("50"), { timeout: 10000 });
  expect(app.enabled("label:Next Page")).toBe(false);
  await app.press("label:Previous Page");
  await app.waitUntil(() => app.texts().includes("100"), { timeout: 10000 });
});

test("编辑暂存后 ⌘S 一次提交", async () => {
  const app = await start();
  await app.press("id:tab:products");
  await app.waitUntil(() => app.texts().includes("Coffee 1"), { timeout: 15000 });
  await app.rightClick("text:Coffee 1");
  expect(app.menu()?.map((e) => e.label)).toContain("Edit");
  await app.chooseMenuItem("Edit");
  await app.waitFor("id:cell-editor");
  await app.type("id:cell-editor", "Edited ");
  await app.press("id:cell-stage");
  await app.waitFor("text:Staged");
  await app.press("label:x");
  await app.waitFor("text:Edit Cell", { state: "gone" });
  await app.keyboard.press("mod+s");
  await app.waitUntil(() => app.texts().includes("Edited Coffee 1"), { timeout: 15000 });
});

test("报错可交给 AI 修复;Explain 显示执行计划并可交给 AI 解读", async () => {
  const app = await start();
  await replaceSql(app, "SELECT nme FROM products");
  await app.press("id:run");
  await app.waitFor("id:query-error", { timeout: 15000 });
  expect(app.shows("id:ai-fix-error")).toBe(true);
  // 没配置 AI:SQL 引用到面板并提示,不报错
  await app.press("id:ai-fix-error");
  await app.waitUntil(() => hasText(app, "The SQL is quoted in the AI panel"), { timeout: 5000 });
  // Explain:SQLite 走 EXPLAIN QUERY PLAN,输出查询计划而不是字节码
  await app.press("text:SELECT nme FROM products");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "SELECT * FROM products WHERE origin = 'Kenya'");
  await app.press("text:Explain");
  await app.waitFor("text:Execution Plan", { timeout: 15000 });
  expect(app.shows("id:ai-explain-plan")).toBe(true);
  expect(app.texts().some((x) => x.startsWith("SCAN") || x.startsWith("SEARCH"))).toBe(true);
  expect(app.shows("id:ai-fix-error")).toBe(false);
  // 默认画成树:没有索引的 origin 条件是全表扫描;可切回原始输出;SQLite 没有 ANALYZE
  expect(app.shows("id:plan-tree")).toBe(true);
  expect(app.texts()).toContain("Full scan");
  expect(app.shows("id:plan-analyze")).toBe(false);
  await app.press("id:plan-view-raw");
  await app.waitUntil(() => !app.shows("id:plan-tree") && app.shows("id:result-grid"), { timeout: 5000 });
  await app.press("id:plan-view-tree");
  await app.waitFor("id:plan-tree", { timeout: 5000 });
  // 普通查询的结果不显示执行计划栏
  await app.press("id:run");
  await app.waitFor("text:Execution Plan", { state: "gone", timeout: 15000 });
  app.dispose();
});

test("写操作可撤销:执行后撤销把数据恢复原样;不能撤销时说明原因", async () => {
  const app = await start();
  await replaceSql(app, "UPDATE products SET origin = 'Undo test' WHERE id <= 3");
  await app.press("id:run");
  await app.waitUntil(() => hasText(app, "✓ Executed successfully · 3 rows affected"), { timeout: 15000 });
  await app.waitFor("text:Can be undone: restores the 3 rows as they were before");
  await app.press("id:undo");
  await app.waitFor("id:undo-dialog");
  expect(app.texts().some((x) => x.includes("These 3 statements restore the 3 rows"))).toBe(true);
  await app.press("id:undo-confirm");
  await app.waitFor("text:Undone: 3 rows restored", { timeout: 15000 });
  expect(app.shows("id:undo")).toBe(false);
  // 数据回到原样:没有一行还是 'Undo test'
  await app.press("text:UPDATE products SET origin = 'Undo test' WHERE id <= 3");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "SELECT COUNT(*) AS n FROM products WHERE origin = 'Undo test'");
  await app.press("id:run");
  await app.waitFor("id:result-grid", { timeout: 15000 });
  expect(app.texts()).toContain("0");
  // 没有改到任何行:不显示撤销,也不说「不能撤销」
  await app.press("text:SELECT COUNT(*) AS n FROM products WHERE origin = 'Undo test'");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "UPDATE products SET origin = 'x' WHERE id < 0");
  await app.press("id:run");
  await app.waitUntil(() => hasText(app, "✓ Executed successfully"), { timeout: 15000 });
  expect(app.shows("id:undo")).toBe(false);
  expect(app.shows("id:undo-unavailable")).toBe(false);
  // DDL 不能撤销:执行照常,并说明原因(用一张临时表,不动其他测试用的数据)
  await app.press("text:UPDATE products SET origin = 'x' WHERE id < 0");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "CREATE TABLE undo_scratch (id INTEGER)");
  await app.press("id:run");
  await app.waitUntil(() => hasText(app, "✓ Executed successfully"), { timeout: 15000 });
  await app.waitFor("id:undo-unavailable");
  expect(hasText(app, "Can't be undone: Undo is only available for single-table UPDATE and DELETE")).toBe(true);
  app.dispose();
});

test("破坏性语句先审查:取消不执行,确认才执行", async () => {
  const app = await start();
  await replaceSql(app, "DELETE FROM orders");
  await app.press("id:run");
  await app.waitFor("text:Review Destructive Statements");
  expect(app.texts()).toContain("Danger");
  await app.press("id:risk-cancel");
  await app.waitFor("text:Review Destructive Statements", { state: "gone" });
  expect(hasText(app, "Executed successfully")).toBe(false);
  await app.press("id:run");
  await app.waitFor("text:Review Destructive Statements");
  // 影响范围预估:行数 + 前 10 行预览,数据未动
  await app.waitFor("text:Will affect 400 rows", { timeout: 15000 });
  await app.waitFor("text:Showing the first 10 of 400 rows");
  await app.press("id:risk-confirm");
  await app.waitUntil(() => hasText(app, "✓ Executed successfully · 400 rows affected"), { timeout: 15000 });
});

test("prod 连接:带 WHERE 的写语句也先确认并预估影响范围", async () => {
  const sql = "UPDATE products SET price_cents = price_cents WHERE id <= 5";
  // local:带 WHERE 的 UPDATE 直接执行
  let app = await start();
  await replaceSql(app, sql);
  await app.press("id:run");
  await app.waitUntil(() => hasText(app, "✓ Executed successfully · 5 rows affected"), { timeout: 15000 });
  expect(app.shows("id:risk-dialog")).toBe(false);
  app.dispose();
  // 换成 prod(SQLite 不读密码,改环境不影响连接)
  const original = (Store.connections() as any[])[0];
  Store.saveConnection({ ...original, env: "prod" }, null);
  try {
    app = await start();
    await replaceSql(app, sql);
    await app.press("id:run");
    await app.waitFor("text:Review Destructive Statements");
    expect(app.texts()).toContain("UPDATE on production");
    expect(hasText(app, "Running on database")).toBe(true);
    expect(app.texts().some((x) => x.includes("Review the following 1 statements"))).toBe(true);
    await app.waitFor("text:Will affect 5 rows", { timeout: 15000 });
    // 预览表头带出列名,行数不超过 10 时不显示「共 N 行」
    expect(app.texts()).toContain("sku");
    expect(app.texts()).toContain("+5 columns");
    expect(hasText(app, "Showing the first")).toBe(false);
    await app.press("id:risk-cancel");
    await app.waitFor("id:risk-dialog", { state: "gone" });
    // 改写不了的写法:标明无法预估,不猜
    await app.press(`text:${sql}`);
    await app.keyboard.press("mod+a");
    await app.type("id:sql-editor", "WITH x AS (SELECT 1) INSERT INTO products (category_id, name) SELECT 1, 'z' FROM x");
    await app.press("id:run");
    await app.waitFor("text:Review Destructive Statements");
    await app.waitFor("text:/^Can't estimate the affected rows/", { timeout: 15000 });
    await app.press("id:risk-cancel");
    app.dispose();
  } finally {
    Store.saveConnection(original, null);
  }
});

test("数据库诊断:概览卡片、完整性检查手动运行、VACUUM 先确认", async () => {
  const app = await start();
  await app.press("id:open-diag");
  await app.waitFor("id:diagnostics", { timeout: 10000 });
  await app.waitFor("id:diag-cards", { timeout: 10000 });
  expect(app.texts()).toContain("File Size");
  expect(app.texts()).toContain("Journal Mode");
  // 表空间:dbstat 可用时列出表,否则说明原因
  await app.press("id:diag-tab-tables");
  await app.waitUntil(() => app.shows("id:diag-table") || app.shows("id:diag-unavailable"), { timeout: 10000 });
  // 完整性检查开销大:切过去不自动跑,点「运行」才查
  await app.press("id:diag-tab-integrity");
  await app.waitUntil(() => hasText(app, "This check can take a while"), { timeout: 5000 });
  await app.press("id:diag-refresh");
  await app.waitUntil(() => app.texts().includes("ok"), { timeout: 10000 });
  // 整库 VACUUM:先弹确认框,取消不执行,确认后提示完成
  await app.press("id:diag-tab-overview");
  await app.press("id:diag-vacuum");
  await app.waitFor("id:diag-confirm", { timeout: 5000 });
  expect(app.texts()).toContain("VACUUM this database?");
  await app.press("id:diag-confirm-cancel");
  await app.waitUntil(() => !app.texts().includes("VACUUM this database?"), { timeout: 5000 });
  await app.press("id:diag-vacuum");
  await app.press("id:diag-confirm-ok");
  await app.waitUntil(() => app.texts().includes("VACUUM finished"), { timeout: 10000 });
});

test("自然语言筛选:没配置 AI 时提示去设置", async () => {
  const app = await start();
  await app.press("id:tab:products");
  await app.waitUntil(() => app.texts().includes("Coffee 2"), { timeout: 15000 });
  await app.keyboard.press("mod+f");
  await app.waitFor("id:filter-nl", { timeout: 5000 });
  await app.type("id:filter-nl", "status failed");
  await app.press("id:filter-nl-go");
  await app.waitUntil(() => app.texts().includes("AI provider not configured. Add one in Settings."), { timeout: 5000 });
});

test("系统智能引号:编辑器里敲出的弯引号改回直引号,SQL 照常执行;报错可一键复制", async () => {
  const app = await start();
  // 模拟 macOS 智能引号把 ' 换成 ’ ‘
  await replaceSql(app, "SELECT name FROM categories WHERE name = \u2018Single Origin\u2019");
  // \u6539\u56de\u76f4\u5f15\u53f7\u8d70 editor.edit,\u6539\u52a8\u8981\u7ecf onChange \u56de\u4f20\u624d\u8fdb\u5230\u8981\u6267\u884c\u7684\u6587\u672c;
  // \u6162\u673a\u5668\u4e0a(CI)\u8f93\u5b8c\u7acb\u523b\u8fd0\u884c\u4f1a\u8d76\u5728\u56de\u4f20\u4e4b\u524d,\u5148\u7b49\u7f16\u8f91\u5668\u6539\u5b8c\u3001\u518d\u7ed9\u56de\u4f20\u7559\u51fa\u65f6\u95f4
  await app.waitUntil(() => !app.texts().some((s) => s.includes("\u2018") || s.includes("\u2019")), { timeout: 5000 });
  await app.wait(500);
  await app.press("id:run");
  await app.waitUntil(() => hasText(app, "Query succeeded · 1 rows returned") || hasText(app, "Query succeeded · 1 row"), { timeout: 15000 });
  expect(app.texts().some((s) => s.includes("\u2018") || s.includes("\u2019"))).toBe(false);
  // 报错区:可选中的报错原文 + 复制按钮
  await app.press("text:SELECT name FROM categories WHERE name = 'Single Origin'");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "SELECT nope FROM categories");
  await app.press("id:run");
  await app.waitFor("id:query-error", { timeout: 15000 });
  expect(app.shows("id:query-error-copy")).toBe(true);
});

test("一次执行多条语句:每条一个结果标签,出错即停,后面的标为未执行", async () => {
  const app = await start();
  await replaceSql(app, "SELECT id FROM categories;\nSELECT name FROM categories WHERE id < 3");
  await app.keyboard.press("mod+shift+enter");
  await app.waitFor("id:stmt-tabs", { timeout: 15000 });
  // 默认显示最后一条
  expect(app.texts()).toContain("Result 1 · 4 rows");
  expect(app.texts()).toContain("Result 2 · 2 rows");
  await app.waitUntil(() => hasText(app, "Query succeeded · 2 rows returned"), { timeout: 5000 });
  await app.press("id:stmt-tab:1");
  await app.waitUntil(() => hasText(app, "Query succeeded · 4 rows returned"), { timeout: 5000 });
  // 中间一条出错:停在出错的那条,第三条未执行
  await app.press("text:SELECT id FROM categories;");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "SELECT 1;\nSELECT nope FROM categories;\nSELECT 2");
  await app.keyboard.press("mod+shift+enter");
  await app.waitUntil(() => app.texts().includes("Result 3 · Not run"), { timeout: 15000 });
  expect(app.texts()).toContain("Result 2 · Error");
  expect(app.shows("id:query-error")).toBe(true);
  await app.press("id:stmt-tab:3");
  await app.waitUntil(() => hasText(app, "Not run: an earlier statement failed."), { timeout: 5000 });
});

test("表浏览筛选在服务端执行:对整张表生效,不只是当前这一页", async () => {
  const app = await start();
  await app.press("id:tab:products");
  await app.waitUntil(() => app.texts().includes("Coffee 2"), { timeout: 15000 });
  await app.keyboard.press("mod+f");
  await app.waitFor("id:filter-server-hint", { timeout: 5000 });
  // 第一页只有前 100 行;Coffee 250 在第 3 页
  expect(app.texts()).not.toContain("Coffee 250");
  // 每个标签的第一个草稿条件 id 是 1
  const draft = "1";
  await app.press(`id:filter-col:${draft}`);
  await app.press("text:name >> nth=1");
  await app.type(`id:filter-value:${draft}`, "Coffee 250");
  await app.press("id:filter-apply");
  await app.waitUntil(() => app.texts().includes("Coffee 250"), { timeout: 10000 });
  expect(app.texts()).not.toContain("Coffee 2");
  expect(app.texts()).not.toContain("SO-001");
  // 清除条件:回到不带条件的第一页
  await app.press("text:Clear");
  await app.waitUntil(() => app.texts().includes("Coffee 2"), { timeout: 10000 });
  expect(app.texts()).not.toContain("Coffee 250");
});

test("查询参数:执行前弹框填值,预览代入结果;同一标签记住上次的值", async () => {
  const app = await start();
  await replaceSql(app, "SELECT name FROM categories WHERE id >= :min AND name <> :skip");
  await app.press("id:run");
  await app.waitFor("id:params-dialog", { timeout: 5000 });
  await app.type("id:param:min", "2");
  await app.type("id:param:skip", "Blend");
  await app.waitUntil(() => app.texts().some((s) => s.includes("id >= 2 AND name <> 'Blend'")), { timeout: 5000 });
  await app.press("id:params-run");
  await app.waitUntil(() => hasText(app, "Query succeeded · 2 rows returned"), { timeout: 15000 });
  expect(app.texts()).not.toContain("Single Origin");
  expect(app.texts()).not.toContain("Blend");
  // 再执行一次:弹框预填上次的值,取消则不执行
  await app.press("id:run");
  await app.waitFor("id:params-dialog", { timeout: 5000 });
  expect(app.value("id:param:min")).toBe("2");
  await app.press("id:params-cancel");
  await app.waitUntil(() => !app.shows("id:params-dialog"), { timeout: 5000 });
});

test("SQL 片段:收藏当前语句,在光标处插入,可删除;重启后仍在", async () => {
  const app = await start();
  await replaceSql(app, "SELECT name FROM categories ORDER BY id");
  await app.press("id:snippets");
  await app.waitFor("text:No snippets yet. Save a statement you use often.", { timeout: 5000 });
  await app.press("id:snippet-add");
  await app.waitFor("id:snippet-name");
  expect(app.value("id:snippet-name")).toBe("SELECT name FROM categories ORDER BY id");
  await app.clear("id:snippet-name");
  await app.type("id:snippet-name", "all categories");
  await app.press("id:snippet-save");
  await app.waitFor("id:snippet:all categories", { timeout: 5000 });
  const saved = Store.loadSnippets() as { snippets: { name: string; sql: string; lang: string }[] };
  expect(saved.snippets.map((s) => [s.name, s.sql, s.lang])).toEqual([
    ["all categories", "SELECT name FROM categories ORDER BY id", "sql"],
  ]);
  // 换一段语句后插入片段:接在后面另起一行,不自动执行
  await app.keyboard.press("Escape");
  await app.waitUntil(() => !app.shows("id:snippet-add"), { timeout: 5000 });
  await app.press("text:SELECT name FROM categories ORDER BY id");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "SELECT 1;");
  await app.press("id:snippets");
  await app.press("id:snippet:all categories");
  await app.waitUntil(() => app.texts().includes("SELECT 1;") && app.texts().includes("2"), { timeout: 5000 });
  expect(app.texts()).toContain("SELECT name FROM categories ORDER BY id");
  expect(hasText(app, "Ready")).toBe(true);
  app.dispose();

  // 重启后片段还在;删除后为空
  const again = await start();
  await again.press("id:snippets");
  await again.waitFor("id:snippet:all categories", { timeout: 5000 });
  await again.press("id:snippet:all categories >> label:Delete Snippet");
  await again.waitFor("text:No snippets yet. Save a statement you use often.", { timeout: 5000 });
  expect((Store.loadSnippets() as { snippets: unknown[] }).snippets).toEqual([]);
  again.dispose();
});

test("多条写语句一起执行也能撤销;过后还能从历史里撤销", async () => {
  const app = await start();
  // 两条写语句一起执行:合成一份倒序的撤销
  await replaceSql(app, "UPDATE products SET origin = 'Multi' WHERE id BETWEEN 20 AND 21; DELETE FROM products WHERE id = 22");
  await app.keyboard.press("mod+shift+enter");
  await app.waitFor("text:Can be undone: restores the 3 rows as they were before", { timeout: 15000 });
  await app.press("id:undo");
  await app.waitFor("id:undo-dialog");
  await app.press("id:undo-confirm");
  await app.waitFor("text:Undone: 3 rows restored", { timeout: 15000 });
  let current = "UPDATE products SET origin = 'Multi' WHERE id BETWEEN 20 AND 21; DELETE FROM products WHERE id = 22";
  const check = async (sql: string, want: string) => {
    await app.press(`text:${current}`);
    current = sql;
    await app.keyboard.press("mod+a");
    await app.type("id:sql-editor", sql);
    await app.press("id:run");
    await app.waitUntil(() => app.texts().includes(want), { timeout: 15000 });
  };
  await check("SELECT COUNT(*) || ' left' AS n FROM products WHERE id BETWEEN 20 AND 22 AND COALESCE(origin, '') <> 'Multi'", "3 left");

  // 单条写语句执行后又跑了别的:从历史里还能撤销这一条
  await check("UPDATE products SET origin = 'Later' WHERE id = 30", "✓ Executed successfully · 1 rows affected");
  await check("SELECT 'other' AS x", "other");
  await app.press("id:history");
  await app.waitUntil(() => app.texts().some((x) => x.startsWith("Undo (1 rows)")), { timeout: 5000 });
  // 整体撤销过的那两条不再提供撤销,只剩这一条
  expect(app.texts().filter((x) => x.startsWith("Undo (")).length).toBe(1);
  await app.press("text:Undo (1 rows)");
  await app.waitFor("id:undo-dialog", { timeout: 5000 });
  await app.press("id:undo-confirm");
  await app.waitFor("text:Undone: 1 rows restored", { timeout: 15000 });
  await check("SELECT COUNT(*) || ' later' AS n FROM products WHERE origin = 'Later'", "0 later");
  // 撤销过的不能再撤销
  await app.press("id:history");
  await app.waitUntil(() => app.texts().some((x) => x.includes("UPDATE products SET origin = 'Later'")), { timeout: 5000 });
  expect(app.texts().some((x) => x.startsWith("Undo ("))).toBe(false);
  app.dispose();
});

test("SQLite 的 JSON 文本也展开成树(只是显示):可复制 $ 路径,不提供按字段筛选", async () => {
  const app = await start();
  await replaceSql(app, `SELECT 'tags' AS k, '{"origin": {"farm": "Kiambu", "lots": [12, 13]}}' AS doc`);
  await app.press("id:run");
  await app.waitUntil(() => app.texts().includes("tags"), { timeout: 15000 });
  await app.rightClick(`id:result-grid >> text:/"farm": "Kiambu"/`);
  await app.chooseMenuItem("Expand JSON");
  await app.waitFor("id:cell-json-tree", { timeout: 5000 });
  await app.press("id:json-node:origin.lots");
  await app.waitFor("id:json-node:origin.lots.1", { timeout: 3000 });
  await app.press("id:json-node:origin.lots.1");
  await app.press("id:json-copy-path");
  await app.waitUntil(() => app.clipboard() === "$.origin.lots[1]", { timeout: 3000 });
  expect(app.shows("id:json-filter")).toBe(false);
  app.dispose();
});
