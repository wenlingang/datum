// Redis Lua 脚本(run-tests.sh 的 redis 组,需要测试用的 Redis;连接 rl 用库 11):
// 写命令先确认、KEYS / ARGV 逐个传参、结果按类型显示,换 tab 回来脚本与参数还在;
// 只读脚本直接执行,字符串结果走格式识别;语法错误给出说明;危险命令要勾选;prod 连接一律确认;
// 只读死循环超过 busy 阈值后显示 SCRIPT KILL,另开连接终止;脚本存进片段并标记为 Lua。

import { Db, Store } from "datum/native";
import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const desktop = { platform: "macos", width: 1400, height: 900, locale: "en-US" } as const;
const has = (app: App, part: string) => app.texts().some((s) => s.includes(part));
const conns = () => (Store.connections() as { id: string }[]).map((c) => c.id);
const uniq = (p: string) => `${p}:${Math.random().toString(36).slice(2, 10)}`;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function openLua(app: App, rail: string) {
  await app.press(`id:rail-${rail}`);
  await app.press("label:Lua Script (EVAL)");
  await app.waitFor("id:redis-lua", { timeout: 10000 });
}

/** 测试结束时确保没有脚本还在跑(只读脚本才 KILL 得掉,测试只用只读的死循环)。 */
async function killLeftover(conn: string) {
  for (let i = 0; i < 40; i++) {
    const r = (await Db.redisScriptKill(conn).catch(() => null)) as { status: string } | null;
    if (r && (r.status === "killed" || r.status === "notBusy")) return;
    await sleep(250);
  }
}

test("写命令先确认;KEYS / ARGV 逐个传参;结果按类型显示;换 tab 回来脚本与参数还在", async () => {
  if (!conns().includes("rl")) return;
  const app = await open("../App.tsx", desktop);
  await openLua(app, "rl");
  const k = uniq("datum:ui:lua");
  await app.type("id:rlua-editor", "redis.call('SET', KEYS[1], ARGV[1]) return {redis.call('GET', KEYS[1]), 7, false}");
  await app.waitFor("id:rlua-tag-writes", { timeout: 5000 });
  expect(has(app, "Writes: SET")).toBe(true);
  await app.press("id:rlua-add-keys");
  await app.type("id:rlua-keys-1", k);
  await app.press("id:rlua-add-argv");
  await app.type("id:rlua-argv-1", "hello world");

  await app.press("id:rlua-run");
  await app.waitFor("id:rlua-confirm", { timeout: 5000 });
  expect(has(app, "The script writes data: SET")).toBe(true);
  // 取消不执行
  await app.press("id:rlua-confirm-cancel");
  expect(await Db.redisExec("rl", `EXISTS ${k}`)).toBe("0");
  await app.press("id:rlua-run");
  await app.press("id:rlua-confirm-ok");
  await app.waitUntil(() => has(app, '"hello world"') && has(app, "(integer) 7") && has(app, "(nil)"), { timeout: 10000 });
  expect(has(app, "array · items: 3")).toBe(true);
  // 带空格的参数原样作为一个参数传过去,写在连接设置的库 11 上
  expect(await Db.redisExec("rl", `GET ${k}`)).toBe("hello world");

  // 换到别的标签页再回来:脚本、KEYS、ARGV 都还在
  await app.press("label:Redis Live (Pub/Sub, MONITOR)");
  await app.waitFor("id:redis-live", { timeout: 5000 });
  await app.press("label:Lua Script");
  await app.waitFor("id:redis-lua", { timeout: 5000 });
  expect(app.value("id:rlua-keys-1")).toBe(k);
  expect(app.value("id:rlua-argv-1")).toBe("hello world");
  expect(has(app, "redis.call('SET', KEYS[1], ARGV[1])")).toBe(true);
  await Db.redisExec("rl", `DEL ${k}`);
  app.dispose();
});

test("只读脚本直接执行,字符串结果走格式识别;语法错误给出说明;危险命令要勾选", async () => {
  if (!conns().includes("rl")) return;
  const app = await open("../App.tsx", desktop);
  await openLua(app, "rl");
  const k = uniq("datum:ui:luaro");
  await Db.redisExec("rl", `SET ${k} {"order":42,"state":"paid"}`);
  await app.type("id:rlua-editor", "return redis.call('GET', KEYS[1])");
  await app.waitFor("id:rlua-tag-readonly", { timeout: 5000 });
  await app.press("id:rlua-add-keys");
  await app.type("id:rlua-keys-1", k);
  await app.press("id:rlua-run");
  await app.waitFor("id:rkey-format-detected", { timeout: 10000 });
  expect(app.shows("id:rlua-confirm")).toBe(false);
  expect(has(app, "JSON")).toBe(true);

  // 语法错误
  await app.clear("id:rlua-editor");
  await app.type("id:rlua-editor", "return (");
  await app.press("id:rlua-run");
  await app.waitFor("id:rlua-error-hint", { timeout: 10000 });
  expect(has(app, "The script has a syntax error.")).toBe(true);

  // KEYS 命令:标为危险,要勾选才能执行
  await app.clear("id:rlua-editor");
  await app.type("id:rlua-editor", "return #redis.call('KEYS', 'datum:ui:luaro:*')");
  await app.waitFor("id:rlua-tag-danger", { timeout: 5000 });
  await app.press("id:rlua-run");
  await app.waitFor("id:rlua-confirm-danger", { timeout: 5000 });
  let pressed = true;
  await app.press("id:rlua-confirm-ok").catch(() => (pressed = false));
  expect(pressed).toBe(false);
  await app.press("id:rlua-confirm-ack");
  await app.press("id:rlua-confirm-ok");
  await app.waitUntil(() => has(app, "(integer) "), { timeout: 10000 });
  await Db.redisExec("rl", `DEL ${k}`);
  app.dispose();
});

test("prod 连接:只读脚本也要勾选确认", async () => {
  if (!conns().includes("rl")) return;
  const rl = (Store.connections() as { id: string }[]).find((c) => c.id === "rl")!;
  Store.saveConnection({ ...rl, id: "rlp", name: "redis-prod", env: "prod" }, "");
  try {
    await Db.connect("redis", "rlp");
    const app = await open("../App.tsx", desktop);
    await openLua(app, "rlp");
    await app.type("id:rlua-editor", "return redis.call('PING')");
    await app.press("id:rlua-run");
    await app.waitFor("id:rlua-confirm-prod", { timeout: 5000 });
    expect(has(app, "redis-prod is a production instance.")).toBe(true);
    let pressed = true;
    await app.press("id:rlua-confirm-ok").catch(() => (pressed = false));
    expect(pressed).toBe(false);
    await app.press("id:rlua-confirm-ack");
    await app.press("id:rlua-confirm-ok");
    await app.waitUntil(() => has(app, "PONG"), { timeout: 10000 });
    app.dispose();
  } finally {
    await Db.disconnect("redis", "rlp");
    Store.deleteConnection("rlp");
  }
});

test("只读死循环:超过 busy 阈值后显示 SCRIPT KILL,另开连接终止;脚本存进片段并标记为 Lua", async () => {
  if (!conns().includes("rl")) return;
  const app = await open("../App.tsx", desktop);
  try {
    await openLua(app, "rl");
    await app.type("id:rlua-editor", "while true do end");
    await app.press("id:rlua-run");
    await app.waitUntil(() => has(app, "Running"), { timeout: 5000 });
    await app.waitFor("id:rlua-kill", { timeout: 10000 });
    expect(app.shows("id:rlua-slow")).toBe(true);
    await app.press("id:rlua-kill");
    await app.waitUntil(() => has(app, "Script stopped"), { timeout: 20000 });
    await app.waitFor("id:rlua-error-hint", { timeout: 10000 });
    expect(has(app, "The script was stopped with SCRIPT KILL.")).toBe(true);
    expect(app.shows("id:rlua-kill")).toBe(false);
    // 实例恢复正常
    expect(await Db.redisExec("rl", "PING")).toBe("PONG");

    // 存进片段(Redis 分类,标记为 Lua)
    await app.press("id:rlua-snippets");
    await app.press("id:snippet-add");
    await app.clear("id:snippet-name");
    await app.type("id:snippet-name", "busy loop");
    await app.press("id:snippet-save");
    const saved = (Store.loadSnippets() as { snippets: { name: string; lang: string; lua?: boolean; sql: string }[] }).snippets;
    expect(saved.some((s) => s.name === "busy loop" && s.lang === "redis" && s.lua === true && s.sql === "while true do end")).toBe(true);
  } finally {
    await killLeftover("rl");
    app.dispose();
  }
});
