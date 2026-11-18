// 演示模式与连接的「数据与隐私」(无头,按 macOS 桌面跑)。夹具同 Workspace.test.tsx:
// 第一个连接是本地 SQLite 的 roastery.db(customers 表有 email 列)。改过的设置在测试结束前改回去。

import { Mask } from "datum/native";
import { expect, open, test } from "nsh/test";

const desktop = { platform: "macos", width: 1200, height: 800, locale: "en-US" } as const;

/** 原生侧返回 JSON(类型为 unknown),测试里按遮罩设置的形状读取。 */
const maskSettings = (connId: string) =>
  Mask.settings(connId) as { enabled: boolean; overrides: Record<string, "mask" | "plain" | undefined> };

test("演示模式:顶栏 DEMO 徽章,面包屑不露出 SQLite 文件名;⌘⇧D 退出", async () => {
  try {
    const app = await open("./fixtures/Seeded.tsx", desktop);
    await app.waitFor("id:conn-path");
    await app.waitUntil(() => app.texts().includes("roastery.db"), { timeout: 5000 });
    expect(app.shows("id:demo-badge")).toBe(false);
    await app.press("id:demo-toggle");
    await app.waitFor("id:demo-badge");
    expect(app.texts()).toContain("DEMO");
    expect(app.texts()).not.toContain("roastery.db");
    expect(app.texts()).toContain("•••");
    expect(Mask.demoMode()).toBe(true);
    // 演示模式下所有连接强制遮罩
    expect(maskSettings("c-roastery")).toMatchObject({ enabled: true, demo: true });
    await app.keyboard.press("mod+shift+d");
    await app.waitFor("id:demo-badge", { state: "gone" });
    await app.waitUntil(() => app.texts().includes("roastery.db"), { timeout: 5000 });
    expect(Mask.demoMode()).toBe(false);
    app.dispose();
  } finally {
    Mask.setDemoMode(false);
  }
});

test("演示模式:新建连接的连接串预览隐藏主机与用户名", async () => {
  Mask.setDemoMode(true);
  try {
    const app = await open("../App.tsx", desktop);
    await app.waitFor("id:conn-rail");
    await app.press("label:New Connection");
    await app.waitFor("id:conn-host");
    await app.type("id:conn-host", "db.internal.example");
    await app.type("id:conn-user", "admin_bob");
    await app.wait(200);
    const preview = app.texts().find((s) => s.includes("://")) ?? "";
    expect(preview).toContain("•••");
    expect(app.texts().some((s) => s.includes("://") && s.includes("db.internal.example"))).toBe(false);
    expect(app.texts().some((s) => s.includes("://") && s.includes("admin_bob"))).toBe(false);
    app.dispose();
  } finally {
    Mask.setDemoMode(false);
  }
});

test("连接菜单「数据与隐私」:列出按列名认出的敏感列,可设为不遮罩", async () => {
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-roastery");
  await app.waitFor("label:products");
  await app.press("label:Connection Actions");
  await app.press("text:Data & Privacy");
  await app.waitFor("id:privacy-dialog");
  // 列信息在后台加载
  await app.waitFor("id:privacy-col:customers.email", { timeout: 10_000 });
  expect(app.shows("id:privacy-col:customers.name")).toBe(false);
  await app.press("id:privacy-choice:customers.email >> label:Don't Mask");
  await app.waitUntil(() => maskSettings("c-roastery").overrides["customers.email"] === "plain", { timeout: 3000 });
  await app.press("id:privacy-choice:customers.email >> label:Auto");
  await app.waitUntil(() => maskSettings("c-roastery").overrides["customers.email"] === undefined, { timeout: 3000 });
  // 本地连接默认不遮罩;打开后生效
  expect(maskSettings("c-roastery").enabled).toBe(false);
  await app.press("id:privacy-enabled");
  await app.waitUntil(() => maskSettings("c-roastery").enabled === true, { timeout: 3000 });
  Mask.setEnabled("c-roastery", null);
  app.dispose();
});
