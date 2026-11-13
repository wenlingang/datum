// Redis 分析(run-tests.sh 的 redis 组,需要测试用的 Redis):多级前缀展开、删除大 key 先确认;
// 集群逐个主节点扫描,大 key 标出所在节点,在那个节点上查看 / 删除 / 编辑。
// 按类型编辑 key:Hash / List / Set / ZSet / String / Stream 的增删改,TTL、重命名、新建;prod 连接写入先确认。
// 值的格式识别:写入 MessagePack / gzip / Protobuf 字节后查看,手动选格式按前缀记住。

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

const r = (cmd: string) => Db.redisExec("rs", cmd);

/** 打开侧栏里的 key 标签页,等编辑器出来。 */
async function openKey(app: App, key: string) {
  await app.press(`id:table-list >> label:${key}`);
  await app.waitUntil(() => app.shows("id:rkey-editor") && has(app, key), { timeout: 5000 });
}

/** 表单:清空单行字段后填入。 */
async function fill(app: App, field: string, text: string) {
  await app.clear(`id:rkey-field:${field}`);
  await app.type(`id:rkey-field:${field}`, text);
}

async function submitForm(app: App) {
  await app.press("id:rkey-form-ok");
  await app.waitUntil(() => !app.shows("id:rkey-form"), { timeout: 5000 });
}

test("单机:按类型编辑 key;删除元素先确认;TTL、重命名、新建", async () => {
  if (!conns().includes("rs")) return;
  // 先连上种数据,打开应用时侧栏就能列出这些 key
  await Db.connect("redis", "rs");
  await r("FLUSHDB");
  await r("HSET h a 1 b 2");
  await r("RPUSH l x y x");
  await r("SADD s m1");
  await r("ZADD z 1 a 2 b");
  await r("SET str hello EX 1000");
  for (let i = 1; i <= 60; i++) await r(`XADD x ${i}-0 n ${i}`);
  await r("XGROUP CREATE x g1 0");
  await r("XREADGROUP GROUP g1 alice COUNT 3 STREAMS x >");
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-rs");
  await app.waitFor("id:table-list >> label:h", { timeout: 10000 });

  // Hash:改字段名、新增字段、删除字段(先确认,取消不删)
  await openKey(app, "h");
  await app.press("label:Edit a");
  await app.waitFor("id:rkey-form", { timeout: 5000 });
  await fill(app, "field", "z");
  await submitForm(app);
  expect(await r("HGET h z")).toBe("1");
  expect(await r("HEXISTS h a")).toBe("0");
  await app.waitFor("label:Edit z", { timeout: 5000 });
  await app.press("id:rkey-add");
  await app.waitFor("id:rkey-form", { timeout: 5000 });
  await app.type("id:rkey-field:field", "c");
  await app.type("id:rkey-field:value", "3");
  await submitForm(app);
  expect(await r("HGET h c")).toBe("3");
  await app.press("label:Delete b");
  await app.waitFor("id:rkey-confirm", { timeout: 5000 });
  expect(has(app, "HDEL h b")).toBe(true);
  await app.press("id:rkey-confirm-cancel");
  expect(await r("HEXISTS h b")).toBe("1");
  await app.press("label:Delete b");
  await app.press("id:rkey-confirm-ok");
  await app.waitUntil(() => !app.shows("label:Delete b"), { timeout: 5000 });
  expect(await r("HEXISTS h b")).toBe("0");

  // List:按下标删(有重复值时删的是选中的那一格)、尾部追加
  await openKey(app, "l");
  await app.press("label:Delete 2");
  await app.press("id:rkey-confirm-ok");
  await app.waitUntil(() => !app.shows("label:Delete 2"), { timeout: 5000 });
  expect(await r("LRANGE l 0 -1")).toBe("x\ny");
  await app.press("id:rkey-push-tail");
  await app.type("id:rkey-field:value", "t");
  await submitForm(app);
  expect(await r("LRANGE l 0 -1")).toBe("x\ny\nt");

  // Set:新增成员
  await openKey(app, "s");
  await app.press("id:rkey-add");
  await app.type("id:rkey-field:member", "m2");
  await submitForm(app);
  expect(await r("SISMEMBER s m2")).toBe("1");

  // ZSet:改分数
  await openKey(app, "z");
  await app.press("label:Edit a");
  await fill(app, "score", "5.5");
  await submitForm(app);
  expect(await r("ZSCORE z a")).toBe("5.5");

  // String:改值保存,TTL 保留;设置 / 移除 TTL;重命名
  await openKey(app, "str");
  await app.type("id:rkey-string", " world");
  await app.press("id:rkey-string-save");
  await app.waitUntil(() => has(app, "Saved"), { timeout: 5000 });
  expect(await r("GET str")).toContain("world");
  expect(Number(await r("TTL str"))).toBeGreaterThan(900);
  await app.press("id:rkey-ttl-btn");
  await fill(app, "ttl", "500");
  await submitForm(app);
  const ttl = Number(await r("TTL str"));
  expect(ttl > 0 && ttl <= 500).toBe(true);
  await app.waitUntil(() => has(app, "TTL 500s") || has(app, "TTL 499s"), { timeout: 5000 });
  await app.press("id:rkey-ttl-btn");
  await app.press("id:rkey-ttl-persist");
  await app.waitUntil(() => !app.shows("id:rkey-form"), { timeout: 5000 });
  expect(await r("TTL str")).toBe("-1");
  // 重命名到已存在的 key:报错,原 key 不动
  await app.press("id:rkey-rename-btn");
  await fill(app, "name", "h");
  await app.press("id:rkey-form-ok");
  await app.waitUntil(() => has(app, "already exists"), { timeout: 5000 });
  expect(app.shows("id:rkey-form")).toBe(true);
  await fill(app, "name", "str2");
  await submitForm(app);
  expect(await r("EXISTS str str2")).toBe("1");
  expect(await r("TYPE str2")).toBe("string");
  await app.waitUntil(() => has(app, "str2") && app.shows("id:rkey-editor"), { timeout: 5000 });

  // Stream:倒序分页(最新在前),下一页;消费组与待确认;追加条目
  await openKey(app, "x");
  await app.waitUntil(() => has(app, "60-0") && has(app, "Page 1 · showing 50 · 60 in total"), { timeout: 5000 });
  await app.waitFor("id:rkey-groups", { timeout: 5000 });
  expect(has(app, "Entries: 60")).toBe(true);
  expect(has(app, "Consumers: 1")).toBe(true);
  // Stream 不按行列读取,状态栏不显示「0 rows」
  expect(app.shows("id:stat-rows")).toBe(false);
  expect(has(app, "g1")).toBe(true);
  expect(has(app, "Pending: 3 (1-0 … 3-0)")).toBe(true);
  expect(has(app, "alice")).toBe(true);
  await app.press("id:rkey-stream-next");
  await app.waitUntil(() => has(app, "Page 2 · showing 10"), { timeout: 5000 });
  expect(has(app, "1-0")).toBe(true);
  await app.press("id:rkey-stream-add");
  await app.type("id:rkey-field:fields", "k hello world");
  await submitForm(app);
  expect(await r("XLEN x")).toBe("61");
  const last = await r("XREVRANGE x + - COUNT 1");
  expect(last).toContain("hello world");

  // 新建 key:hash,建好后打开它的标签页
  await app.press("label:New Key");
  await app.waitFor("id:rkey-new", { timeout: 5000 });
  await app.press("id:rkey-new-type >> label:hash");
  await app.type("id:rkey-new-name", "nk");
  await app.type("id:rkey-new-ttl", "300");
  await app.type("id:rkey-new-value", "f 1\ng two words");
  await app.press("id:rkey-new-ok");
  await app.waitUntil(() => !app.shows("id:rkey-new") && app.shows("id:rkey-editor") && has(app, "nk"), { timeout: 5000 });
  expect(await r("HGET nk g")).toBe("two words");
  expect(Number(await r("TTL nk"))).toBeGreaterThan(0);
  app.dispose();
});

test("prod 连接:任何写入都先确认,取消不执行", async () => {
  if (!conns().includes("rs")) return;
  const rs = (Store.connections() as { id: string }[]).find((c) => c.id === "rs")!;
  Store.saveConnection({ ...rs, id: "rs-prod", name: "redis-prod", env: "prod" }, "");
  try {
    await Db.connect("redis", "rs-prod");
    await Db.redisExec("rs-prod", "FLUSHDB");
    await Db.redisExec("rs-prod", "HSET ph a 1");
    const app = await open("../App.tsx", desktop);
    await app.press("id:rail-rs-prod");
    await app.waitFor("id:table-list >> label:ph", { timeout: 10000 });
    await openKey(app, "ph");
    await app.press("id:rkey-add");
    await app.type("id:rkey-field:field", "b");
    await app.type("id:rkey-field:value", "2");
    await app.press("id:rkey-form-ok");
    await app.waitFor("id:rkey-confirm", { timeout: 5000 });
    expect(has(app, "This is a PROD connection. Check the command before running it.")).toBe(true);
    expect(has(app, "HSETNX ph b 2")).toBe(true);
    await app.press("id:rkey-confirm-cancel");
    expect(await Db.redisExec("rs-prod", "HEXISTS ph b")).toBe("0");
    // 取消后表单还在,再提交一次并确认
    await app.press("id:rkey-form-ok");
    await app.press("id:rkey-confirm-ok");
    await app.waitUntil(() => !app.shows("id:rkey-form"), { timeout: 5000 });
    expect(await Db.redisExec("rs-prod", "HGET ph b")).toBe("2");
    app.dispose();
  } finally {
    await Db.disconnect("redis", "rs-prod");
    Store.deleteConnection("rs-prod");
  }
});

/** 二进制安全地写入:redisExec 按空白拆分命令,用 Lua 的 string.char 拼字节(脚本里不能有空格)。 */
const setBytes = (cmd: "SET" | "HSET" | "XADD", key: string, pre: string[], bytes: number[]) =>
  r(`EVAL return(redis.call('${cmd}',KEYS[1],${pre.map((p) => `'${p}',`).join("")}string.char(${bytes.join(",")}))) 1 ${key}`);

/** {"id":7,"name":"Ann"} 的 MessagePack */
const MSGPACK = [130, 162, 105, 100, 7, 164, 110, 97, 109, 101, 163, 65, 110, 110];
/** {"city":"Oslo","n":[1,2]} 的 gzip */
const GZIP_JSON = [31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 171, 86, 74, 206, 44, 169, 84, 178, 82, 242, 47, 206, 201, 87, 210, 81, 202, 83, 178, 138, 54, 212, 49, 138, 173, 5, 0, 202, 23, 92, 227, 25, 0, 0, 0];
/** Protobuf:1: 150、2: "hi"、3: { 1: 1 } */
const PROTOBUF = [8, 150, 1, 18, 2, 104, 105, 26, 2, 8, 1];

/** 识别结果的标签是 `label`(`MessagePack`、`JSON · decompressed from gzip`)。 */
const detected = (app: App, label: string) => app.shows(`id:rkey-format-detected >> text:/^${label}$/`);

test("值的格式识别:MessagePack / gzip / Protobuf 自动识别;JSON 可编辑;手动选格式按前缀记住;元素查看", async () => {
  if (!conns().includes("rs")) return;
  await Db.connect("redis", "rs");
  await r("FLUSHDB");
  await setBytes("SET", "fmt:mp", [], MSGPACK);
  await setBytes("SET", "fmt:gz", [], GZIP_JSON);
  await r(`SET fmt:json {"a":1}`);
  await setBytes("HSET", "fh", ["plain", "x", "packed"], MSGPACK);
  await setBytes("XADD", "fx", ["1-0", "txt", "hello", "pb"], PROTOBUF);
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-rs");
  await app.waitFor("id:table-list >> label:fmt:mp", { timeout: 10000 });

  // String:MessagePack 展开成树,只读(非 UTF-8)
  await openKey(app, "fmt:mp");
  await app.waitUntil(() => detected(app, "MessagePack"), { timeout: 5000 });
  await app.waitFor("id:json-node:name", { timeout: 5000 });
  expect(has(app, '"Ann"')).toBe(true);
  expect(app.shows("id:rkey-string-save")).toBe(false);
  // 识别出二进制格式:不提替换符,只说明二进制格式只读
  expect(app.shows("id:rkey-binary-format")).toBe(true);
  expect(has(app, "Some elements aren't valid UTF-8 text")).toBe(false);
  // gzip 里的 JSON:解压后识别
  await openKey(app, "fmt:gz");
  await app.waitUntil(() => detected(app, "JSON · decompressed from gzip"), { timeout: 5000 });
  expect(has(app, '"Oslo"')).toBe(true);

  // 文本 JSON:默认树,切到文本可编辑保存
  await openKey(app, "fmt:json");
  await app.waitUntil(() => detected(app, "JSON"), { timeout: 5000 });
  await app.waitFor("id:json-node:a", { timeout: 5000 });
  await app.press("id:rkey-view-text");
  // 编辑器里的输入落在光标处(开头):加一个空格,仍是合法 JSON
  await app.type("id:rkey-string", " ");
  await app.press("id:rkey-string-save");
  await app.waitUntil(() => has(app, "Saved"), { timeout: 5000 });
  expect(await r("GET fmt:json")).toBe(' {"a":1}');

  // 手动选 Protobuf:不是 Protobuf,回退为文本并提示;选择按「连接 + 前缀」记住
  await app.press("label:View as");
  await app.press("text:Protobuf");
  await app.waitUntil(() => app.shows("id:rkey-format-fallback"), { timeout: 5000 });
  expect(has(app, "Not valid Protobuf, showing as Text")).toBe(true);
  const saved = Store.loadRedisFormats() as { formats: Record<string, Record<string, string>> };
  expect(saved.formats.rs.fmt).toBe("protobuf");
  // 同前缀的另一个 key 也按 Protobuf 看:MessagePack 字节不是合法 Protobuf,回退十六进制
  await openKey(app, "fmt:mp");
  await app.waitUntil(() => detected(app, "Hex"), { timeout: 5000 });
  expect(has(app, "82 a2 69 64")).toBe(true);
  // 回退到十六进制:保留原来的提示
  expect(has(app, "Some elements aren't valid UTF-8 text")).toBe(true);
  expect(app.shows("id:rkey-binary-format")).toBe(false);
  // 选回自动:记录清掉
  await app.press("label:View as");
  await app.press("text:Auto detect");
  await app.waitUntil(() => detected(app, "MessagePack"), { timeout: 5000 });
  expect((Store.loadRedisFormats() as { formats: Record<string, unknown> }).formats.rs).toBeUndefined();

  // Hash 元素:点「查看」弹窗识别
  await openKey(app, "fh");
  await app.press("label:View packed");
  await app.waitFor("id:rkey-view", { timeout: 5000 });
  await app.waitUntil(() => detected(app, "MessagePack"), { timeout: 5000 });
  await app.press("id:rkey-view >> label:Close");
  await app.waitUntil(() => !app.shows("id:rkey-view"), { timeout: 5000 });

  // Stream 条目:弹窗里切换字段逐个识别
  await openKey(app, "fx");
  await app.waitFor("label:View 1-0", { timeout: 5000 });
  await app.press("label:View 1-0");
  await app.waitFor("id:rkey-view", { timeout: 5000 });
  await app.waitUntil(() => detected(app, "Text"), { timeout: 5000 });
  await app.press("label:Field");
  await app.press("text:pb");
  await app.waitUntil(() => detected(app, "Protobuf"), { timeout: 5000 });
  await app.waitFor("id:json-node:2", { timeout: 5000 });
  expect(has(app, '"hi"')).toBe(true);
  app.dispose();
});
