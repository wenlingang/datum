// Redis 实时(run-tests.sh 的 redis 组,需要测试用的 Redis):订阅频道收到消息、界面发布、暂停时不刷新而继续缓冲、
// 详情走格式识别、停止后退订;MONITOR 先警告确认、本地过滤;prod 连接发布先确认、开 MONITOR 要勾选二次确认;
// 关 tab 时关掉专用连接。

import { Db, Store } from "datum/native";
import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const desktop = { platform: "macos", width: 1400, height: 900, locale: "en-US" } as const;
const has = (app: App, part: string) => app.texts().some((s) => s.includes(part));
const conns = () => (Store.connections() as { id: string }[]).map((c) => c.id);
const uniq = (p: string) => `${p}:${Math.random().toString(36).slice(2, 10)}`;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function openLive(app: App, rail: string) {
  await app.press(`id:rail-${rail}`);
  await app.press("label:Redis Live (Pub/Sub, MONITOR)");
  await app.waitFor("id:redis-live", { timeout: 10000 });
}

/** 服务器上还有没有 Datum 的专用连接(CLIENT SETNAME 为 datum-pubsub-… / datum-monitor-…)。 */
async function liveClients(conn: string): Promise<number> {
  const list = (await Db.redisExec(conn, "CLIENT LIST")) as string;
  return list.split("\n").filter((l) => /name=datum-(pubsub|monitor)-/.test(l)).length;
}

test("Pub/Sub:订阅收到消息,界面发布,暂停不刷新,详情识别格式,停止后退订,关 tab 关掉专用连接", async () => {
  if (!conns().includes("rs")) return;
  const app = await open("../App.tsx", desktop);
  await openLive(app, "rs");
  const ch = uniq("datum:ui:ch");
  await app.type("id:rlive-sub-name", ch);
  await app.press("id:rlive-subscribe");
  await app.waitFor("id:rlive-subs", { timeout: 5000 });
  // 等服务器确认订阅
  for (let i = 0; i < 50 && (await Db.redisPublish("rs", ch, "probe")) === 0; i++) await sleep(100);
  await app.waitUntil(() => has(app, "probe"), { timeout: 5000 });

  await Db.redisPublish("rs", ch, '{"order":42,"state":"paid"}');
  await app.waitUntil(() => has(app, '{"order":42,"state":"paid"}'), { timeout: 5000 });
  expect(has(app, "JSON")).toBe(true);

  // 界面发布
  await app.type("id:rlive-pub-channel", ch);
  await app.type("id:rlive-pub-message", "from-ui");
  await app.press("id:rlive-publish");
  await app.waitUntil(() => has(app, "from-ui") && has(app, "subscribers that received it: 1"), { timeout: 5000 });

  // 暂停:后台继续收,界面不刷新;继续后补上
  await app.press("id:rlive-pause");
  await app.waitUntil(() => has(app, "Paused"), { timeout: 3000 });
  await Db.redisPublish("rs", ch, "while-paused");
  await sleep(900);
  expect(has(app, "while-paused")).toBe(false);
  await app.press("id:rlive-pause");
  await app.waitUntil(() => has(app, "while-paused"), { timeout: 5000 });

  // 详情:完整内容 + 格式识别
  await app.press("label:View message 2");
  await app.waitFor("id:rlive-detail", { timeout: 5000 });
  await app.waitFor("id:rlive-detail >> id:rkey-format-detected", { timeout: 5000 });
  expect(has(app, "JSON")).toBe(true);
  await app.press("id:rlive-detail >> label:Close");
  await app.waitUntil(() => !app.shows("id:rlive-detail"), { timeout: 5000 });

  // 清空
  await app.press("id:rlive-clear");
  await app.waitUntil(() => !has(app, "while-paused"), { timeout: 3000 });

  // 停止:退订,连接关掉
  expect(await liveClients("rs")).toBe(1);
  await app.press("id:rlive-stop");
  await app.waitUntil(() => has(app, "Stopped"), { timeout: 5000 });
  for (let i = 0; i < 50 && (await liveClients("rs")) > 0; i++) await sleep(100);
  expect(await liveClients("rs")).toBe(0);
  expect(await Db.redisPublish("rs", ch, "x")).toBe(0);

  // 再订阅一次,关 tab 后专用连接也关掉
  await app.type("id:rlive-sub-name", ch);
  await app.press("id:rlive-subscribe");
  await app.waitFor("id:rlive-subs", { timeout: 5000 });
  for (let i = 0; i < 50 && (await liveClients("rs")) === 0; i++) await sleep(100);
  expect(await liveClients("rs")).toBe(1);
  await app.press("label:Close Redis Live");
  await app.waitUntil(() => !app.shows("id:redis-live"), { timeout: 5000 });
  for (let i = 0; i < 50 && (await liveClients("rs")) > 0; i++) await sleep(100);
  expect(await liveClients("rs")).toBe(0);
  app.dispose();
});

test("MONITOR:开始前警告确认,本地按命令与 key 过滤,停止后关掉连接", async () => {
  if (!conns().includes("rs")) return;
  const app = await open("../App.tsx", desktop);
  await openLive(app, "rs");
  await app.press("label:MONITOR");
  await app.waitFor("id:rlive-monitor-warning", { timeout: 5000 });
  const k = uniq("datum:ui:mon");
  await app.type("id:rlive-filter-command", "set");
  await app.type("id:rlive-filter-key", k);
  await app.press("id:rlive-monitor-start");
  await app.waitFor("id:rlive-monitor-confirm", { timeout: 5000 });
  expect(has(app, "Start MONITOR?")).toBe(true);
  // 取消不开
  await app.press("id:rlive-monitor-confirm-cancel");
  expect(await liveClients("rs")).toBe(0);
  await app.press("id:rlive-monitor-start");
  await app.press("id:rlive-monitor-confirm-ok");
  await app.waitUntil(() => has(app, "Monitoring"), { timeout: 5000 });
  await Db.redisExec("rs", `SET ${k}:1 hello`);
  await Db.redisExec("rs", `GET ${k}:1`);
  await Db.redisExec("rs", "SET datum:ui:other x");
  await app.waitUntil(() => has(app, `"${k}:1" "hello"`), { timeout: 5000 });
  expect(app.texts().some((s) => s === "GET")).toBe(false);
  expect(has(app, "datum:ui:other")).toBe(false);
  await app.press("id:rlive-stop");
  await app.waitUntil(() => has(app, "Stopped"), { timeout: 5000 });
  for (let i = 0; i < 50 && (await liveClients("rs")) > 0; i++) await sleep(100);
  expect(await liveClients("rs")).toBe(0);
  await Db.redisExec("rs", `DEL ${k}:1 datum:ui:other`);
  app.dispose();
});

test("prod 连接:发布先确认;开 MONITOR 要勾选二次确认", async () => {
  if (!conns().includes("rs")) return;
  const rs = (Store.connections() as { id: string }[]).find((c) => c.id === "rs")!;
  Store.saveConnection({ ...rs, id: "rsp", name: "redis-prod", env: "prod" }, "");
  try {
    await Db.connect("redis", "rsp");
    const app = await open("../App.tsx", desktop);
    await openLive(app, "rsp");
    const ch = uniq("datum:ui:prod");
    await app.type("id:rlive-pub-channel", ch);
    await app.type("id:rlive-pub-message", "careful");
    await app.press("id:rlive-publish");
    await app.waitFor("id:rlive-publish-confirm", { timeout: 5000 });
    expect(has(app, "Publish to a production instance?")).toBe(true);
    await app.press("id:rlive-publish-confirm-ok");
    await app.waitUntil(() => has(app, "subscribers that received it: 0"), { timeout: 5000 });

    await app.press("label:MONITOR");
    await app.press("id:rlive-monitor-start");
    await app.waitFor("id:rlive-monitor-confirm", { timeout: 5000 });
    expect(has(app, "MONITOR affects live traffic")).toBe(true);
    // 没勾选时确认按钮不可用(按下会因 disabled 报错)
    let pressed = true;
    await app.press("id:rlive-monitor-confirm-ok").catch(() => (pressed = false));
    expect(pressed).toBe(false);
    expect(await liveClients("rsp")).toBe(0);
    await app.press("id:rlive-monitor-confirm-ack");
    await app.press("id:rlive-monitor-confirm-ok");
    await app.waitUntil(() => has(app, "Monitoring"), { timeout: 5000 });
    expect(await liveClients("rsp")).toBe(1);
    await app.press("id:rlive-stop");
    for (let i = 0; i < 50 && (await liveClients("rsp")) > 0; i++) await sleep(100);
    expect(await liveClients("rsp")).toBe(0);
    app.dispose();
  } finally {
    await Db.disconnect("redis", "rsp");
    Store.deleteConnection("rsp");
  }
});
