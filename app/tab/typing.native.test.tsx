// 真实窗口(原生平台)里的输入测试:无头运行时事件同步处理,测不出「打字快于 JS 处理 onChange」的问题。
// 运行:app/testing/run-tests.sh native(需要能开窗口的 macOS / Windows / Linux 桌面)
import { expect, open, test } from "nsh/test";
const SQL = "DELETE FROM orders WHERE quantity >= 3 AND note IS NULL";
test("查询编辑器:快速连续输入,字符顺序不乱", async () => {
  const app = await open("./testing/Harness.tsx", { platform: "macos", width: 1200, height: 800, locale: "en-US" });
  await app.waitFor("id:sql-editor", { timeout: 15000 });
  await app.press("text:-- Top products by revenue");
  await app.keyboard.press("mod+a");
  await app.keyboard.press("backspace");
  await app.type("id:sql-editor", SQL);
  await new Promise((r) => setTimeout(r, 800));
  const shown = app.texts().filter((x) => x.includes("quantity")).join(" | ");
  expect(shown).toBe(SQL);
  app.dispose();
});
