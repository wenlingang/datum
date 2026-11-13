// Redis 分析(run-tests.sh 的 redis 组,需要测试用的 Redis):多级前缀展开、删除大 key 先确认;
// 集群逐个主节点扫描,大 key 标出所在节点,在那个节点上查看 / 删除。

import { Db, Store } from "datum/native";
import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const desktop = { platform: "macos", width: 1400, height: 900, locale: "en-US" } as const;
const has = (app: App, part: string) => app.texts().some((s) => s.includes(part));
const conns = () => (Store.connections() as { id: string }[]).map((c) => c.id);

async function analyse(app: App, rail: string) {
  await app.press(`id:rail-${rail}`);
  await app.press("label:Redis Analysis");
  await app.waitFor("id:redis-insight", { timeout: 10000 });
  await app.press("id:ri-start");
  await app.waitUntil(() => has(app, "Scanned") && !has(app, "Scanning"), { timeout: 20000 });
}

test("单机:前缀逐级展开;删除大 key 先确认,删后从排行榜消失", async () => {
  if (!conns().includes("rs")) return;
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-rs");
  await app.waitFor("label:Redis Analysis", { timeout: 10000 });
  await Db.redisExec("rs", "FLUSHDB");
  for (let i = 0; i < 40; i++) await Db.redisExec("rs", `SET user:profile:${i} v${i}`);
  for (let i = 0; i < 5; i++) await Db.redisExec("rs", `SET user:session:${i} s`);
  await Db.redisExec("rs", `SET bigkey ${"x".repeat(20000)}`);
  await analyse(app, "rs");
  await app.waitFor("id:ri-biggest", { timeout: 5000 });
  expect(app.shows("label:bigkey")).toBe(true);
  // 前缀:user:* 展开出 user:profile:* 与 user:session:*
  expect(has(app, "user:profile:*")).toBe(false);
  await app.press("id:ri-prefix-toggle:user");
  await app.waitUntil(() => has(app, "user:profile:*") && has(app, "user:session:*"), { timeout: 5000 });
  // 删除:先确认,取消不删
  await app.press("label:Delete bigkey");
  await app.waitFor("id:ri-delete", { timeout: 5000 });
  expect(has(app, "Delete key bigkey?")).toBe(true);
  await app.press("id:ri-delete-cancel");
  expect(await Db.redisExec("rs", "EXISTS bigkey")).toBe("1");
  await app.press("label:Delete bigkey");
  await app.press("id:ri-delete-ok");
  await app.waitUntil(() => has(app, "Key deleted"), { timeout: 5000 });
  expect(app.shows("label:bigkey")).toBe(false);
  expect(await Db.redisExec("rs", "EXISTS bigkey")).toBe("0");
  app.dispose();
});

test("集群:逐个主节点扫描;大 key 标出节点,在所在节点上查看与删除", async () => {
  if (!conns().includes("rc0")) return;
  const app = await open("../App.tsx", desktop);
  // bigkey 的槽位(7676)在第二个主节点上:连它写入
  await app.press("id:rail-rc1");
  await app.waitFor("label:Redis Analysis", { timeout: 10000 });
  await Db.redisExec("rc1", `SET bigkey ${"y".repeat(30000)}`);
  // 从第一个节点发起分析
  await analyse(app, "rc0");
  await app.waitFor("id:ri-nodes", { timeout: 5000 });
  expect(app.texts().filter((s) => /^127\.0\.0\.1:\d+$/.test(s)).length).toBeGreaterThanOrEqual(3);
  await app.press("id:ri-biggest >> label:bigkey");
  await app.waitFor("id:ri-value", { timeout: 5000 });
  expect(has(app, "yyyyyyyy")).toBe(true);
  await app.press("id:ri-value-close");
  await app.waitUntil(() => !app.shows("id:ri-value"), { timeout: 5000 });
  await app.press("id:ri-biggest >> label:Delete bigkey");
  await app.press("id:ri-delete-ok");
  await app.waitUntil(() => has(app, "Key deleted"), { timeout: 5000 });
  expect(await Db.redisExec("rc1", "EXISTS bigkey")).toBe("0");
  app.dispose();
});
