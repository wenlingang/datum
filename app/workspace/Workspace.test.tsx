// 工作区外壳的交互测试(无头,按 macOS 桌面跑)。
// 运行前把 DATUM_CONFIG_DIR 指向临时配置目录,其中 connections.json 的第一个连接是
// 含 products / orders 等表的本地 SQLite(见报告里的 mkcfg.sh);绝不要指向真实配置。

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
