// 敏感数据遮罩(headless)。run-tests.sh 的 tab-mask 夹具:roastery 库外加 customers 表
// (name / email / phone / password / tier),mask_settings.json 打开了 dev-sqlite 的遮罩。
// 测试按顺序执行;改了手动覆盖、演示模式的测试结束前改回去。

import { Mask } from "datum/native";
import { expect, open, test, type App } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1200, height: 800, locale: "en-US" };
const CONN = "dev-sqlite";

async function start(): Promise<App> {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  return app;
}

async function openCustomers(app: App) {
  await app.waitFor("id:open:customers", { timeout: 15000 });
  await app.press("id:open:customers");
  await app.waitUntil(() => app.texts().includes("Alice"), { timeout: 15000 });
}

test("表浏览:敏感列遮罩显示,普通列原样,表头带标记", async () => {
  const app = await start();
  await openCustomers(app);
  const texts = app.texts();
  expect(texts).toContain("a***@example.com");
  expect(texts).toContain("138****5678");
  expect(texts).toContain("••••••");
  for (const raw of ["alice@example.com", "13812345678", "hunter2"]) expect(texts).not.toContain(raw);
  // name 不按姓名识别(太常见),tier 不敏感
  expect(texts).toContain("Bob");
  expect(texts).toContain("gold");
  expect(app.shows("id:th-masked:email")).toBe(true);
  expect(app.shows("id:th-masked:password")).toBe(true);
  expect(app.shows("id:th-masked:tier")).toBe(false);
  app.dispose();
});

test("复制值 / 复制行 / INSERT 都是遮罩后的值", async () => {
  const app = await start();
  await openCustomers(app);
  await app.rightClick("text:a***@example.com");
  const labels = app.menu()?.map((e) => e.label) ?? [];
  expect(labels).toContain("Reveal Value");
  await app.chooseMenuItem("Copy Value");
  await app.waitUntil(() => app.clipboard() === "a***@example.com", { timeout: 3000 });
  await app.rightClick("text:a***@example.com");
  await app.chooseMenuItem("Copy Row as CSV");
  await app.waitUntil(() => (app.clipboard() ?? "").includes("a***@example.com"), { timeout: 3000 });
  expect(app.clipboard()).not.toContain("alice@example.com");
  expect(app.clipboard()).not.toContain("hunter2");
  await app.rightClick("text:a***@example.com");
  await app.chooseMenuItem("Copy INSERT Statement");
  await app.waitUntil(() => (app.clipboard() ?? "").startsWith("INSERT"), { timeout: 3000 });
  expect(app.clipboard()).toContain("a***@example.com");
  expect(app.clipboard()).not.toContain("13812345678");
  app.dispose();
});

test("显示原值:右键只揭开这一格;弹窗里遮罩值只读,显示原值后才能修改", async () => {
  const app = await start();
  await openCustomers(app);
  await app.rightClick("text:b***@example.org");
  await app.chooseMenuItem("Reveal Value");
  await app.waitUntil(() => app.texts().includes("bob@example.org"), { timeout: 3000 });
  // 别的格子仍是遮罩
  expect(app.texts()).toContain("a***@example.com");
  // 弹窗:先是遮罩值、只读,没有「暂存」
  await app.rightClick("text:c***@example.net");
  await app.chooseMenuItem("Edit");
  await app.waitFor("id:cell-masked", { timeout: 5000 });
  expect(app.shows("id:cell-stage")).toBe(false);
  // 编辑器里也是遮罩值(网格一份 + 编辑器一份)
  expect(app.texts().filter((x) => x === "c***@example.net").length).toBe(2);
  expect(app.texts()).not.toContain("carol@example.net");
  await app.press("id:cell-reveal");
  await app.waitFor("id:cell-masked", { state: "gone", timeout: 5000 });
  await app.waitUntil(() => app.texts().includes("carol@example.net"), { timeout: 3000 });
  expect(app.shows("id:cell-stage")).toBe(true);
  app.dispose();
});

test("行详情:敏感字段遮罩", async () => {
  const app = await start();
  await openCustomers(app);
  await app.press("text:Alice");
  await app.press("label:detail");
  await app.waitFor("id:row-detail", { timeout: 5000 });
  expect(app.shows("id:row-masked:email")).toBe(true);
  expect(app.texts().filter((s) => s === "alice@example.com")).toEqual([]);
  app.dispose();
});

test("表头右键:取消遮罩 / 遮罩此列(写进连接的手动覆盖)", async () => {
  const app = await start();
  await openCustomers(app);
  await app.rightClick("id:th:phone");
  expect(app.menu()?.map((e) => e.label)).toContain("Unmask This Column");
  await app.chooseMenuItem("Unmask This Column");
  await app.waitUntil(() => app.texts().includes("13812345678"), { timeout: 5000 });
  await app.rightClick("id:th:tier");
  await app.chooseMenuItem("Mask This Column");
  await app.waitUntil(() => !app.texts().includes("gold"), { timeout: 5000 });
  expect(app.shows("id:th-masked:tier")).toBe(true);
  expect(Mask.settings(CONN)).toMatchObject({ overrides: { "customers.phone": "plain", "customers.tier": "mask" } });
  Mask.setOverride(CONN, "customers.phone", null);
  Mask.setOverride(CONN, "customers.tier", null);
  app.dispose();
});

test("查询结果:没有可认的列名时按取值识别(表达式列)", async () => {
  const app = await start();
  await app.press("text:-- Top products by revenue");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "SELECT name, email || '' AS contact FROM customers ORDER BY id");
  await app.press("id:run");
  await app.waitUntil(() => app.texts().includes("Alice"), { timeout: 15000 });
  expect(app.texts()).toContain("a***@example.com");
  expect(app.texts()).not.toContain("alice@example.com");
  app.dispose();
});

test("演示模式:不能显示原值", async () => {
  Mask.setDemoMode(true);
  try {
    const app = await start();
    await openCustomers(app);
    await app.rightClick("text:a***@example.com");
    const reveal = app.menu()?.find((e) => e.label === "Reveal Value");
    expect(reveal?.disabled).toBe(true);
    await app.closeMenu();
    await app.rightClick("text:a***@example.com");
    await app.chooseMenuItem("Edit");
    await app.waitFor("id:cell-masked", { timeout: 5000 });
    expect(app.enabled("id:cell-reveal")).toBe(false);
    app.dispose();
  } finally {
    Mask.setDemoMode(false);
  }
});

test("遮罩关掉:原值显示", async () => {
  Mask.setEnabled(CONN, false);
  try {
    const app = await start();
    await openCustomers(app);
    expect(app.texts()).toContain("alice@example.com");
    expect(app.shows("id:th-masked:email")).toBe(false);
    app.dispose();
  } finally {
    Mask.setEnabled(CONN, true);
  }
});
