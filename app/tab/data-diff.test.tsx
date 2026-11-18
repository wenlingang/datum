// 数据对比标签页(无头,按 macOS 桌面跑)。夹具见 run-tests.sh 的 workspace 块:
// 两个 SQLite 连接 c-src / c-dst,同名表 items 各类差异各一行(一致 2、仅源表 1、仅目标表 1、值不同 2),
// 另有没有主键也没有唯一键的 nopk 表。

import { expect, open, type App } from "nsh/test";
import { test } from "nsh/test";

const desktop = { platform: "macos", width: 1280, height: 860, locale: "en-US" } as const;

/** 一致 2、仅源表 1、仅目标表 1、值不同 2(类别按钮上带计数)。 */
function expectCounts(app: App) {
  expect(app.texts().some((s) => s.startsWith("2 identical"))).toBe(true);
  for (const l of ["All Differences 4", "Only in Source 1", "Only in Target 1", "Different 2"]) {
    expect(app.shows(`label:${l}`)).toBe(true);
  }
}

async function openItems(table = "items"): Promise<App> {
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-src");
  await app.waitFor(`id:table-list >> label:${table}`);
  await app.rightClick(`id:table-list >> label:${table}`);
  await app.press("label:Compare with Another Table");
  await app.waitFor("id:data-diff");
  return app;
}

test("表右键对比数据:默认目标为另一连接的同名表,计数正确并生成同步脚本", async () => {
  const app = await openItems();
  // 目标连接与同名表自动选好,主键识别出来后才能开始
  await app.waitUntil(() => app.enabled("id:ddiff-run"), { timeout: 10_000 });
  expect(app.shows("id:ddiff-key")).toBe(true);
  await app.press("id:ddiff-run");
  await app.waitFor("id:ddiff-summary", { timeout: 15_000 });
  // 一致 / 仅源表 / 仅目标表 / 值不同(1.0 与 1 按数值相等;'cups ' 的尾随空格算差异;3 与 NULL 不同)
  expectCounts(app);
  expect(app.texts().some((s) => s === "cups → cups ")).toBe(true);

  // 按类别筛选
  await app.press("label:Only in Target 1");
  await app.waitUntil(() => !app.texts().includes("cups → cups "));
  expect(app.texts()).toContain("scale");

  // 默认脚本:DELETE 被注释
  await app.press("id:ddiff-copy");
  await app.waitUntil(() => (app.clipboard() ?? "").includes("INSERT INTO"));
  const script = app.clipboard() ?? "";
  expect(script).toContain('-- DELETE FROM "items" WHERE "id" = 6;');
  expect(script).toContain(`UPDATE "items" SET "name" = 'cups' WHERE "id" = 2;`);
  expect(script).toContain(`UPDATE "items" SET "qty" = 3 WHERE "id" = 3;`);
  expect(script).toContain("(4, 'grinder', 4, 40, X'02')");
  // 打开「包含删除」后 DELETE 可执行
  await app.press("id:ddiff-include-deletes");
  await app.press("id:ddiff-copy");
  await app.waitUntil(() => (app.clipboard() ?? "").includes('\nDELETE FROM "items" WHERE "id" = 6;'));

  // 在目标库的编辑器里打开(不执行)
  await app.press("id:ddiff-open");
  await app.waitFor("id:sql-editor");
  await app.waitUntil(() => app.texts().includes(`UPDATE "items" SET "name" = 'cups' WHERE "id" = 2;`));
  // 查询标签建在目标连接(dst)上
  expect(app.texts()).toContain("dst.db");
  app.dispose();
});

test("交换方向后重新对比:仅源表 / 仅目标表互换", async () => {
  const app = await openItems();
  await app.waitUntil(() => app.enabled("id:ddiff-run"), { timeout: 10_000 });
  await app.press("id:ddiff-swap");
  await app.waitUntil(() => app.enabled("id:ddiff-run"), { timeout: 10_000 });
  await app.press("id:ddiff-run");
  await app.waitFor("id:ddiff-summary", { timeout: 15_000 });
  expectCounts(app);
  await app.press("id:ddiff-copy");
  await app.waitUntil(() => (app.clipboard() ?? "").includes("INSERT INTO"));
  // 现在目标是 src.db:要插入的是 dst 独有的 scale,要删的是 grinder
  expect(app.clipboard()).toContain("'scale'");
  expect(app.clipboard()).toContain('-- DELETE FROM "items" WHERE "id" = 4;');
  app.dispose();
});

test("没有主键也没有唯一键的表:说明原因,不能开始", async () => {
  const app = await openItems("nopk");
  await app.waitFor("id:ddiff-no-key", { timeout: 10_000 });
  expect(app.enabled("id:ddiff-run")).toBe(false);
  app.dispose();
});
