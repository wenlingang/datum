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

// 系统设置开着「智能引号与破折号」时(macOS 默认开),编辑器以前会把 ' " 换成弯引号、-- 换成破折号,
// SQL 因此报错。nsh 0.5.1 起 Editor(autoCorrect 关闭)不再替换;这条要在真实窗口里跑才测得到。
const QUOTED = `SELECT 'a' AS x, "b" AS y -- c`;
test("查询编辑器:系统智能引号开着时,敲入的引号与 -- 原样保留", async () => {
  const app = await open("./testing/Harness.tsx", { platform: "macos", width: 1200, height: 800, locale: "en-US" });
  await app.waitFor("id:sql-editor", { timeout: 15000 });
  await app.press("text:-- Top products by revenue");
  await app.keyboard.press("mod+a");
  await app.keyboard.press("backspace");
  await app.type("id:sql-editor", QUOTED);
  await new Promise((r) => setTimeout(r, 800));
  const shown = app.texts().filter((x) => x.includes("AS x")).join(" | ");
  expect(shown).toBe(QUOTED);
  app.dispose();
});
