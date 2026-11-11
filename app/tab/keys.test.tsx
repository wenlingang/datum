// 编辑器内快捷键:⌘↵ 运行(有选区时运行选中部分),不能被编辑器自带的「下方插入新行」吃掉。
import { expect, open, test, type App } from "nsh/test";

const OPTS = { platform: "macos" as const, width: 1200, height: 800, locale: "en-US" };
const hasText = (app: App, prefix: string) => app.texts().some((s) => s.startsWith(prefix));

test("选中 SQL 后 ⌘↵ 运行,不插入换行", async () => {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  await app.press("text:-- Top products by revenue");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "SELECT id, name FROM categories");
  await app.keyboard.press("mod+a");
  await app.keyboard.press("mod+enter");
  await app.waitUntil(() => hasText(app, "Query succeeded · 4 rows returned"), { timeout: 15000 });
  // 有选区时 value() 读不到编辑器文本:看屏幕上的原文仍在,且没有出现第 2 行的行号(没插入换行)
  const texts = app.texts();
  expect(texts).toContain("SELECT id, name FROM categories");
  expect(texts.slice(0, texts.indexOf("SELECT id, name FROM categories"))).not.toContain("2");
});

test("无选区时 ⌘↵ 运行光标所在语句", async () => {
  const app = await open("./testing/Harness.tsx", OPTS);
  await app.waitFor("id:tab:SQL Query", { timeout: 15000 });
  await app.press("text:-- Top products by revenue");
  await app.keyboard.press("mod+a");
  await app.type("id:sql-editor", "SELECT id FROM categories");
  await app.keyboard.press("mod+enter");
  await app.waitUntil(() => hasText(app, "Query succeeded · 4 rows returned"), { timeout: 15000 });
  expect(app.value("id:sql-editor")).toBe("SELECT id FROM categories");
});
