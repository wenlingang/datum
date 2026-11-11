// 对话框的无头测试。必须在临时配置目录里跑,不碰用户真实配置:
//   DATUM_CONFIG_DIR=<临时目录> nsh test dialogs/dialogs.test.tsx
// 配置目录里要先放一个 SQLite 文件 test.db(测试连接、保存、导入都只用它)。
// (TSX 读不到未声明的环境变量,配置目录经原生模块取得。)

import { Store } from "datum/native";
import { describe, expect, open, test } from "nsh/test";

const CONFIG = Store.configDir();
const SQLITE = `${CONFIG}/test.db`;

// 防误用:没设 DATUM_CONFIG_DIR 时 core 会用用户真实的配置目录
if (!/^\/(private\/)?(tmp|var\/folders)\//.test(CONFIG)) {
  throw new Error(`refusing to run against ${CONFIG}: set DATUM_CONFIG_DIR to a temporary directory`);
}

const start = () => open("./testing/Harness.tsx", { platform: "macos", width: 1200, height: 800 });

// 测试里 fs 受沙箱限制,经原生模块读落盘文件
async function readJson(name: string): Promise<any> {
  return JSON.parse(Store.readSqlFile(`${CONFIG}/${name}`));
}

describe("connection form", () => {
  test("validates name and SQLite file before saving", async () => {
    const app = await start();
    await app.press("id:dev-new-conn");
    await app.waitFor("text:New Connection");
    await app.press("id:conn-save");
    await app.waitFor("text:Please enter a name");
    // 换成 SQLite,不选文件
    await app.press("label:Type");
    await app.press("text:SQLite");
    await app.type("id:conn-name", "local-file");
    await app.press("id:conn-save");
    await app.waitFor("text:Please choose a SQLite database file");
    expect(app.shows("text:New Connection")).toBe(true);
    app.dispose();
  });

  test("previews the connection string as fields change", async () => {
    const app = await start();
    await app.press("id:dev-new-conn");
    await app.waitFor("text:postgresql://postgres@127.0.0.1:5432/postgres?sslmode=prefer");
    await app.type("id:conn-password", "secret");
    await app.waitFor("text:postgresql://postgres:••••@127.0.0.1:5432/postgres?sslmode=prefer");
    expect(app.texts().some((t) => t.includes("secret"))).toBe(false);
    // MySQL:端口与用户换成惯用默认值
    await app.press("label:Type");
    await app.press("text:MySQL");
    await app.waitFor("text:mysql://root:••••@127.0.0.1:3306/postgres?sslmode=prefer");
    // Redis + Require:走 rediss://,不带 sslmode
    await app.press("label:Type");
    await app.press("text:Redis");
    await app.press("label:SSL");
    await app.press("text:Require");
    await app.waitFor("text:rediss://root:••••@127.0.0.1:6379/0");
    app.dispose();
  });

  test("tests a SQLite file, saves, refreshes the list and focuses it", async () => {
    const app = await start();
    await app.press("id:dev-new-conn");
    await app.press("label:Type");
    await app.press("text:SQLite");
    await app.type("id:conn-name", "temp-sqlite");
    await app.type("id:conn-file", SQLITE);
    await app.waitFor(`text:sqlite://${SQLITE}`);
    await app.press("id:conn-test");
    await app.waitFor("text:/^Connection OK · took \\d+ ms$/", { timeout: 15_000 });
    await app.press("id:conn-save");
    await app.waitFor("id:conn-form", { state: "gone" });
    await app.waitFor("text:temp-sqlite");
    const saved = (await readJson("connections.json")) as any;
    const list: any[] = Array.isArray(saved) ? saved : (saved.connections ?? []);
    const conn = list.find((c) => c.name === "temp-sqlite");
    expect(conn?.kind).toBe("sqlite");
    expect(conn?.filePath).toBe(SQLITE);
    await app.waitFor(`text:/active: ${conn.id}/`);
    app.dispose();
  });
  test("连接测试失败时完整显示报错", async () => {
    const app = await start();
    await app.press("id:dev-new-conn");
    await app.clear("id:conn-port");
    await app.type("id:conn-port", "1");
    await app.press("id:conn-test");
    await app.waitFor("id:conn-test-error", { timeout: 15_000 });
    // 报错整段出现在提示框里,不再被截断成一行
    const full = app.texts().find((x) => x.startsWith("Failed to connect to PostgreSQL"));
    expect(full).toBeTruthy();
    expect(full!.endsWith("…")).toBe(false);
    app.dispose();
  });
});

describe("settings", () => {
  test("appearance changes are written to ui_prefs.json", async () => {
    const app = await start();
    await app.press("id:dev-settings-general");
    await app.waitFor("text:Appearance");
    await app.press("label:Theme");
    await app.press("text:Light");
    await app.waitUntil(() => app.shows("text:/theme: light/"));
    await app.press("label:Font Size");
    await app.press("text:Large (15px)");
    await app.type("id:pref-max-rows-custom", "2500");
    await app.submit("id:pref-max-rows-custom");
    await app.waitFor("text:/maxRows: 2500/");
    const prefs = await readJson("ui_prefs.json");
    expect(prefs.theme).toBe("light");
    expect(prefs.fontSize).toBe(15);
    expect(prefs.maxRows).toBe(2500);
    app.dispose();
  });

  test("adds a provider, edits it, and saves to ai_settings.json", async () => {
    const app = await start();
    await app.press("id:dev-settings");
    await app.waitFor("text:No AI providers yet");
    await app.press("text:Add Provider");
    await app.press("text:OpenAI");
    await app.waitFor("id:provider-base-url");
    await app.clear("id:provider-base-url");
    // 本机拒绝连接的地址:获取模型只会得到错误,不会请求真实服务商
    await app.type("id:provider-base-url", "http://127.0.0.1:9/v1");
    await app.type("id:model-add", "gpt-test");
    await app.submit("id:model-add");
    await app.waitFor("text:gpt-test");
    const ai = await readJson("ai_settings.json");
    const p = ai.providers.find((x: any) => x.name === "OpenAI");
    expect(p.baseUrl).toBe("http://127.0.0.1:9/v1");
    expect(p.models).toEqual([{ id: "gpt-test", enabled: true }]);
    await app.press("id:models-fetch");
    await app.waitFor("id:settings-notice", { timeout: 15_000 });
    app.dispose();
  });

  test("about shows the version and the development-build state", async () => {
    const app = await start();
    await app.press("id:dev-settings-about");
    await app.waitFor("text:Development build; auto-update is unavailable");
    expect(app.shows("id:app-version")).toBe(true);
    app.dispose();
  });
});

describe("import", () => {
  test("imports statements into the SQLite connection in one transaction", async () => {
    const app = await start();
    await app.waitFor("text:temp-sqlite");
    await app.press("id:dev-import");
    await app.waitFor("text:Import Database");
    await app.type("id:import-sql", "create table imp_test(id integer primary key);\ninsert into imp_test(id) values (1);");
    await app.waitFor("text:2 statements");
    await app.press("id:import-run");
    await app.waitFor("text:/^Import complete: 2 statements/", { timeout: 15_000 });
    app.dispose();
  });
});
