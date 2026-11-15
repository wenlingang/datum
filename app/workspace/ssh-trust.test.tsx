// 首次连接 SSH 跳板机:弹出主机指纹请用户确认;取消则不连接(下次仍会问),信任后连上并记住。
// 需要 sshd + 跳板机上的 Redis(run-tests.sh 的 ssh 组,DATUM_TEST_SSH 等环境变量)。

import { expect, open, test } from "nsh/test";

const desktop = { platform: "macos", width: 1200, height: 800, locale: "en-US" } as const;
const has = (app: Awaited<ReturnType<typeof open>>, part: string) => app.texts().some((s) => s.includes(part));

test("首次连接跳板机:确认指纹后才连接", async () => {
  // 取消:不连接
  const app = await open("../App.tsx", desktop);
  app.onDialog(() => "Cancel");
  await app.press("id:rail-ssh-redis");
  await app.waitUntil(() => has(app, "Failed to load ssh-redis"), { timeout: 15000 });
  const d = app.dialogs();
  expect(d.length).toBe(1);
  expect(d[0]).toMatchObject({ title: "Trust this SSH server?", buttons: ["Cancel", "Trust and Connect"] });
  expect(d[0].message ?? "").toContain("SHA256:");
  expect(has(app, "ssh-ok")).toBe(false);
  app.dispose();

  // 再开一次:仍然要确认;信任后连上,列出 key
  const again = await open("../App.tsx", desktop);
  again.onDialog(() => "Trust and Connect");
  await again.press("id:rail-ssh-redis");
  await again.waitUntil(() => has(again, "ssh-ok"), { timeout: 15000 });
  expect(again.dialogs().length).toBe(1);
  again.dispose();

  // 记住了:之后不再询问
  const later = await open("../App.tsx", desktop);
  later.onDialog(() => "Cancel");
  await later.press("id:rail-ssh-redis");
  await later.waitUntil(() => has(later, "ssh-ok"), { timeout: 15000 });
  expect(later.dialogs().length).toBe(0);
  later.dispose();
});
