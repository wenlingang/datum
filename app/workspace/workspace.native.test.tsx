// 工作区外壳在真实窗口里的交互(无头运行时不派发 onKeyDown,Esc 这类按键要在这里测)。
// 运行:app/testing/run-tests.sh native(需要能开窗口的桌面)

import { expect, open, test } from "nsh/test";

const desktop = { platform: "macos", width: 1200, height: 800, locale: "en-US" } as const;

test("改名:按 Esc 退出输入并保存当前名字", async () => {
  const app = await open("./fixtures/Seeded.tsx", desktop);
  await app.waitFor("id:tab-strip >> label:SQL Query");
  await app.rightClick("id:tab-strip >> label:SQL Query");
  await app.press("label:Rename");
  await app.waitFor("id:tab-rename");
  await app.clear("id:tab-rename");
  await app.type("id:tab-rename", "Top coffees");
  await app.keyboard.press("escape");
  await app.waitUntil(() => !app.shows("id:tab-rename"));
  expect(app.shows("id:tab-strip >> label:Top coffees")).toBe(true);
  app.dispose();
});
