// 工作区外壳的交互测试(无头,按 macOS 桌面跑)。
// 运行前把 DATUM_CONFIG_DIR 指向临时配置目录,其中 connections.json 的第一个连接是
// 含 products / orders 等表的本地 SQLite(见报告里的 mkcfg.sh);绝不要指向真实配置。

import { Journal } from "datum/native";
import { expect, open, test } from "nsh/test";

// 界面语言随测试默认的 en-US
const desktop = { platform: "macos", width: 1200, height: 800, locale: "en-US" } as const;

test("空工作区:提示选连接,竖轨列出连接", async () => {
  const app = await open("../App.tsx", desktop);
  await app.waitFor("text:Ready");
  expect(app.count("id:empty-workspace")).toBe(1);
  expect(app.shows("id:empty-new-query")).toBe(true);
  expect(app.count("id:rail-c-roastery")).toBe(1);
});

test("点竖轨连接:加载表列表,搜索按名过滤", async () => {
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-roastery");
  await app.waitFor("label:products");
  expect(app.shows("label:orders")).toBe(true);
  await app.type("id:table-search", "prod");
  await app.waitUntil(() => !app.shows("label:orders"));
  expect(app.shows("label:products")).toBe(true);
  // 切到另一个连接时搜索清空
  await app.press("id:rail-c-shop");
  await app.waitFor("label:items");
  expect(app.value("id:table-search")).toBe("");
});

test("侧栏开关 ⌘B 与 AI 面板开关", async () => {
  const app = await open("./fixtures/Seeded.tsx", desktop);
  await app.waitFor("label:products");
  await app.keyboard.press("mod+b");
  await app.waitUntil(() => !app.shows("id:table-search"));
  await app.keyboard.press("mod+b");
  await app.waitFor("id:table-search");
  expect(app.shows("id:ai-panel")).toBe(false);
  await app.press("id:toggle-ai");
  await app.waitFor("id:ai-panel");
});

test("标签条:当前表进面包屑,关闭 tab 回落到邻居", async () => {
  const app = await open("./fixtures/Seeded.tsx", desktop);
  await app.waitFor("id:conn-path");
  // 预置了 SQL Query 与 products 两个 tab,激活的是后加的 products
  expect(app.count("role:tab")).toBe(2);
  expect(app.shows("id:export-menu")).toBe(true);
  expect(app.texts()).toContain("products");
  await app.keyboard.press("mod+w");
  await app.waitUntil(() => app.count("role:tab") === 1);
  // 回落到 SQL Query:没有表,过滤 / 导出隐藏
  expect(app.shows("id:export-menu")).toBe(false);
  await app.keyboard.press("mod+w");
  await app.waitFor("id:empty-workspace");
});

test("tab 右键:重命名与关闭其他", async () => {
  const app = await open("./fixtures/Seeded.tsx", desktop);
  await app.waitFor("label:SQL Query");
  await app.rightClick("label:SQL Query");
  await app.press("label:Rename");
  await app.waitFor("id:tab-rename");
  await app.clear("id:tab-rename");
  await app.type("id:tab-rename", "Top coffees\n");
  await app.waitFor("label:Top coffees");
  await app.rightClick("label:Top coffees");
  await app.press("label:Close Other Tabs");
  await app.waitUntil(() => app.count("role:tab") === 1);
  expect(app.shows("label:Top coffees")).toBe(true);
});

test("表右键:查看建表语句弹窗", async () => {
  const app = await open("./fixtures/Seeded.tsx", desktop);
  await app.waitFor("label:products");
  await app.rightClick("id:table-list >> label:products");
  await app.press("label:View CREATE Statement");
  await app.waitFor("id:ddl-dialog");
  await app.press("id:ddl-copy");
  await app.waitUntil(() => (app.clipboard() ?? "").includes("CREATE TABLE"));
});

test("AI 对话工作台:对话铺满主区域、侧栏保留;再按一次或 ⌘J 回到编辑布局", async () => {
  const app = await open("./fixtures/Seeded.tsx", desktop);
  await app.waitFor("label:products");
  await app.press("id:toggle-ai");
  await app.waitFor("id:ai-panel");
  await app.press("id:ai-fullscreen");
  await app.waitFor("id:ai-workbench");
  // 标签页区域与右侧面板收起,侧栏还在
  expect(app.shows("id:ai-panel")).toBe(false);
  expect(app.shows("id:table-search")).toBe(true);
  expect(app.shows("text:Exit Full Screen")).toBe(true);
  await app.press("id:ai-fullscreen");
  await app.waitFor("id:ai-workbench", { state: "gone" });
  // 退出后 AI 面板仍在右侧
  expect(app.shows("id:ai-panel")).toBe(true);
  // ⌘J 切换
  await app.keyboard.press("mod+j");
  await app.waitFor("id:ai-workbench");
  await app.keyboard.press("mod+j");
  await app.waitFor("id:ai-workbench", { state: "gone" });
});

test("结构对比:roastery 对比 shop,列出差异并生成迁移脚本", async () => {
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-roastery");
  await app.waitFor("label:products");
  await app.press("label:Connection Actions");
  await app.press("label:Compare Schema");
  await app.waitFor("id:schema-diff");
  expect(app.enabled("id:sdiff-run")).toBe(false);
  await app.press("id:sdiff-target:shop");
  await app.press("id:sdiff-run");
  await app.waitFor("id:sdiff-summary", { timeout: 10_000 });
  // roastery 的表在 shop 里都没有;shop 独有的 items 只会生成注释掉的 DROP
  expect(app.shows("id:sdiff-table:products")).toBe(true);
  expect(app.shows("id:sdiff-table:items")).toBe(true);
  expect(app.texts().some((s) => s.startsWith("11 new"))).toBe(true);
  // 视图与触发器也参与对比;SQLite 的触发器体有多条语句,整段注释等人工执行
  expect(app.shows("id:sdiff-view:v_kenya")).toBe(true);
  expect(app.shows("id:sdiff-trigger:trg_review")).toBe(true);
  app.dispose();
});

// 放在最后:它会留下会话文件,之后打开的应用都会恢复这些标签
test("重启后恢复上次打开的标签与内容(不自动执行)", async () => {
  const first = await open("../App.tsx", desktop);
  await first.press("id:rail-c-roastery");
  await first.waitFor("label:products");
  await first.press("label:products");
  await first.waitFor("text:Boquete", { timeout: 10_000 });
  await first.keyboard.press("mod+t");
  await first.waitFor("id:sql-editor");
  await first.type("id:sql-editor", "SELECT 42 AS answer");
  // 等防抖写盘
  await first.wait(1500);
  first.dispose();

  const second = await open("../App.tsx", desktop);
  await second.waitUntil(() => second.texts().includes("SELECT 42 AS answer"), { timeout: 10_000 });
  // 查询 tab 是当前 tab,内容恢复但没有执行
  expect(second.texts()).not.toContain("answer");
  expect(second.shows("id:empty-workspace")).toBe(false);
  // 表浏览 tab 还在:切过去后自动加载数据
  await second.press("id:tab-strip >> text:products");
  await second.waitFor("text:Boquete", { timeout: 10_000 });
});

// 放最后:会删掉 c-shop 连接
test("连接菜单打开写操作记录;删除连接时可连同写操作记录一起删", async () => {
  await Journal.record({ connId: "c-shop", connName: "shop", kind: "sqlite", source: "editor", sql: "UPDATE items SET title = 'b' WHERE id = 1", ok: true, affected: 1 });
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-shop");
  await app.waitFor("label:items");
  await app.rightClick("id:rail-c-shop");
  await app.press("label:Write Journal");
  await app.waitFor("id:journal-view", { timeout: 5000 });
  await app.waitUntil(() => app.texts().includes("UPDATE items SET title = 'b' WHERE id = 1"), { timeout: 5000 });
  expect(app.texts()).toContain("Write Journal · shop");

  app.onDialog(() => "Delete with Journal");
  await app.rightClick("id:rail-c-shop");
  await app.press("label:Delete");
  await app.waitUntil(() => app.count("id:rail-c-shop") === 0, { timeout: 5000 });
  const d = app.dialogs();
  expect(d[d.length - 1]?.buttons).toEqual(["Cancel", "Delete, Keep Journal", "Delete with Journal"]);
  let left: unknown[] = [{}];
  for (let i = 0; i < 30 && left.length > 0; i++) {
    left = (await Journal.list("c-shop", null)) as unknown[];
    if (left.length > 0) await app.wait(100);
  }
  expect(left.length).toBe(0);
  app.dispose();
});

test("查看表详情:弹窗显示字段与索引,不打开表的 tab,关掉不留面板", async () => {
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-roastery");
  await app.waitFor("id:table-list >> label:products");
  const tabs = app.count("role:tab");
  await app.rightClick("id:table-list >> label:products");
  await app.press("label:View Table Details");
  await app.waitFor("id:table-detail-dialog");
  expect(app.shows("id:detail-columns")).toBe(true);
  expect(app.shows("id:detail-indexes")).toBe(true);
  expect(app.shows("id:table-detail-dialog >> text:country")).toBe(true);
  expect(app.count("role:tab")).toBe(tabs);
  // 右上角关闭
  await app.press("id:table-detail-dialog >> label:Close");
  await app.waitUntil(() => !app.shows("id:table-detail-dialog"));
  // 再开一次,Esc 关闭;打开 AI 只有 AI 面板
  await app.rightClick("id:table-list >> label:products");
  await app.press("label:View Table Details");
  await app.waitFor("id:table-detail-dialog");
  await app.keyboard.press("escape");
  await app.waitUntil(() => !app.shows("id:table-detail-dialog"));
  await app.press("id:toggle-ai");
  await app.waitFor("id:ai-panel");
  expect(app.shows("id:detail-columns")).toBe(false);
});

test("表右键「导出整表」是二级菜单:展开后列出五种格式", async () => {
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-roastery");
  await app.waitFor("id:table-list >> label:products");
  await app.rightClick("id:table-list >> label:products");
  await app.waitFor("text:Export Table");
  // 主菜单里不再平铺各格式
  expect(app.shows("label:CSV")).toBe(false);
  await app.press("text:Export Table");
  for (const f of ["CSV", "Excel", "JSON", "SQL", "Markdown"]) await app.waitFor(`label:${f}`);
});

test("只有查询页能改名:表的 tab 双击不进入重命名,右键也没有「重命名」", async () => {
  const app = await open("./fixtures/Seeded.tsx", desktop);
  await app.waitFor("id:tab-strip >> label:products");
  // 表的 tab:双击只激活,右键菜单没有重命名
  await app.press("id:tab-strip >> label:products");
  await app.press("id:tab-strip >> label:products");
  await app.wait(300);
  expect(app.shows("id:tab-rename")).toBe(false);
  await app.rightClick("id:tab-strip >> label:products");
  await app.waitFor("label:Close Other Tabs");
  expect(app.shows("label:Rename")).toBe(false);
  await app.keyboard.press("escape");
  // 查询页:双击进入重命名(对照)
  await app.press("id:tab-strip >> label:SQL Query");
  await app.press("id:tab-strip >> label:SQL Query");
  await app.waitFor("id:tab-rename");
});

test("改名:点到别处退出输入并保存当前名字(Esc 见 workspace.native.test.tsx)", async () => {
  const app = await open("./fixtures/Seeded.tsx", desktop);
  await app.waitFor("id:tab-strip >> label:SQL Query");
  await app.press("id:tab-strip >> label:SQL Query");
  await app.press("id:tab-strip >> label:SQL Query");
  await app.waitFor("id:tab-rename");
  await app.clear("id:tab-rename");
  await app.type("id:tab-rename", "Daily report");
  // 按在不能获得焦点的地方(顶栏的连接路径,测试只能用 drag 按下):输入框不会失焦,由工作区根节点收尾
  await app.drag("id:conn-path", { x: 1 });
  await app.waitUntil(() => !app.shows("id:tab-rename"));
  expect(app.shows("id:tab-strip >> label:Daily report")).toBe(true);
});

test("改名时点另一个 tab:一次就切过去(按下即激活,不怕改名收尾带来的挪位)", async () => {
  const app = await open("./fixtures/Seeded.tsx", desktop);
  await app.waitFor("id:tab-strip >> label:SQL Query");
  await app.press("id:tab-strip >> label:SQL Query");
  await app.press("id:tab-strip >> label:SQL Query");
  await app.waitFor("id:tab-rename");
  expect(app.shows("id:export-menu")).toBe(false);
  // drag:按下后指针移开、点击不成立——就像改名收尾让 tab 挪了位、抬起时已不在指针下
  await app.drag("id:tab-strip >> label:products", { x: 40 });
  await app.waitUntil(() => !app.shows("id:tab-rename"));
  // products(表)成了当前 tab:标签条右侧出现导出
  await app.waitFor("id:export-menu");
});

test("点 tab 的图标也能切换(整个 tab 都可点)", async () => {
  const app = await open("./fixtures/Seeded.tsx", desktop);
  await app.waitFor("id:tab-strip >> label:SQL Query");
  // 预置激活的是 products:点 SQL Query 的图标切到查询页,再点 products 的图标切回
  await app.press("id:tab-icon:SQL Query");
  await app.waitUntil(() => !app.shows("id:export-menu"));
  await app.press("id:tab-icon:products");
  await app.waitFor("id:export-menu");
});

test("标签栏末尾的「+」新建查询页", async () => {
  const app = await open("./fixtures/Seeded.tsx", desktop);
  await app.waitFor("id:tab-strip >> label:SQL Query");
  const n = app.count("role:tab");
  await app.press("id:new-query-tab");
  await app.waitUntil(() => app.count("role:tab") === n + 1);
});

test("tab 右键:关闭左侧全部 / 关闭右侧全部", async () => {
  const app = await open("./fixtures/Seeded.tsx", desktop);
  await app.waitFor("id:tab-strip >> label:products");
  // 预置 SQL Query、products;再开一个查询页:SQL Query | products | 新查询
  await app.press("id:new-query-tab");
  await app.waitUntil(() => app.count("role:tab") === 3);
  // 最左边的没有可关的左侧
  await app.rightClick("id:tab-strip >> label:SQL Query >> nth=0");
  await app.waitFor("label:Close Tabs to the Left");
  expect(app.enabled("label:Close Tabs to the Left")).toBe(false);
  await app.keyboard.press("escape");
  // 中间的 products:先关右侧,再关左侧,只剩它自己
  await app.rightClick("id:tab-strip >> label:products");
  await app.press("label:Close Tabs to the Right");
  await app.waitUntil(() => app.count("role:tab") === 2);
  await app.rightClick("id:tab-strip >> label:products");
  await app.press("label:Close Tabs to the Left");
  await app.waitUntil(() => app.count("role:tab") === 1);
  expect(app.shows("id:tab-strip >> label:products")).toBe(true);
});
