// 真实窗口(原生平台)里的输入测试:无头运行时事件同步处理,测不出「打字快于 JS 处理 onChange」的问题。
// 运行:app/testing/run-tests.sh native
import { expect, open, test } from "nsh/test";
test("AI SQL 卡片:快速连续输入字符顺序不乱,格式化仍能推给编辑器", async () => {
  const app = await open("./testing/risky.tsx", { platform: "macos", width: 1000, height: 760, locale: "en-US" });
  await app.waitFor("id:ai-input", { timeout: 15000 });
  await app.type("id:ai-input", "x");
  await app.press("id:ai-send");
  await app.waitFor("id:sql-editor-2:3", { timeout: 15000 });
  await app.press("text:SELECT * FROM orders;");
  await app.keyboard.press("mod+a");
  await app.keyboard.press("backspace");
  await app.type("id:sql-editor-2:3", "select id, total from orders where total > 10 and id < 99");
  await new Promise((r) => setTimeout(r, 800));
  // 软换行会把一行拆成几段文本:取卡片里「1」之后、「执行」之前的各段拼起来
  const all = app.texts();
  const start = all.lastIndexOf("1") + 1;
  const typed = all.slice(start, all.indexOf("Run", start)).join("");
  expect(typed).toContain("select id, total from orders where total > 10 and id < 99");
  await app.press("id:sql-card-2:3 >> role:button[Format]");
  await new Promise((r) => setTimeout(r, 500));
  const formatted = app.texts().filter((x) => /SELECT|FROM/.test(x)).join(" | ");
  expect(formatted).toContain("SELECT");
  app.dispose();
});
