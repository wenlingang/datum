// SQLite 改表结构表单的交互测试(无头,macOS 桌面)。
// 运行前 DATUM_CONFIG_DIR 指向临时配置目录,第一个连接 c-alter 是临时 SQLite 文件
// (products 带索引与视图 v_kenya,order_items 有一行 product_id 指向不存在的商品,见 run-tests.sh)。

import { expect, open, test } from "nsh/test";

const desktop = { platform: "macos", width: 1280, height: 860, locale: "en-US" } as const;

async function openAlter(table: string) {
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-alter");
  await app.waitFor(`label:${table}`);
  await app.rightClick(`id:table-list >> label:${table}`);
  await app.press("label:Alter Table…");
  await app.waitFor("id:alter-dialog");
  await app.waitFor("id:alter-mode");
  return app;
}

const sqlShown = (app: Awaited<ReturnType<typeof open>>, part: string) => app.texts().some((s) => s.includes(part));

test("改列类型:预览重建表脚本,经危险语句确认后执行", async () => {
  const app = await openAlter("products");
  expect(app.texts()).toContain("No changes");
  expect(app.enabled("id:alter-execute")).toBe(false);
  // 第 3 列 country:TEXT → VARCHAR(40),并设为非空、默认值 'n/a'
  await app.clear("id:alter-type:2");
  await app.type("id:alter-type:2", "VARCHAR(40)");
  await app.press("id:alter-nn:2");
  await app.type("id:alter-default:2", "'n/a'");
  await app.waitUntil(() => app.texts().includes("Rebuild table"));
  await app.waitUntil(() => sqlShown(app, "PRAGMA foreign_keys = OFF;"));
  expect(sqlShown(app, "\"country\" VARCHAR(40) NOT NULL DEFAULT 'n/a'")).toBe(true);
  // 编辑器只渲染可见的行:完整脚本从「复制」拿
  await app.press("id:alter-copy");
  await app.waitUntil(() => (app.clipboard() ?? "").includes("PRAGMA foreign_key_check(\"products\");"));
  const script = app.clipboard() ?? "";
  expect(script).toContain("CREATE INDEX products_country");
  expect(script.indexOf("COMMIT;")).toBeGreaterThan(script.indexOf("PRAGMA foreign_key_check"));
  // 新的 NOT NULL:原有 NULL 会复制失败,给出提示
  expect(app.shows("id:alter-warnings")).toBe(true);
  await app.press("id:alter-execute");
  // 脚本里有 DROP TABLE:照常弹危险语句确认
  await app.waitFor("id:risk-dialog");
  expect(sqlShown(app, "DROP TABLE \"products\"")).toBe(true);
  await app.press("id:risk-confirm");
  await app.waitFor("id:alter-dialog", { state: "gone", timeout: 10_000 });

  // 再打开:类型与非空已生效(表清单刷新完再右键)
  await app.waitFor("id:table-list >> label:products", { timeout: 10_000 });
  await app.rightClick("id:table-list >> label:products");
  await app.press("label:Alter Table…");
  await app.waitFor("id:alter-mode");
  expect(app.value("id:alter-type:2")).toBe("VARCHAR(40)");
  expect(app.texts()).toContain("No changes");
  app.dispose();
});

test("只改列名与加列:直接 ALTER TABLE,不走重建;已打开的表 tab 按新列刷新", async () => {
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-alter");
  await app.waitFor("id:table-list >> label:products");
  // 先打开 products 的表浏览 tab
  await app.press("id:table-list >> label:products");
  await app.waitFor("text:Boquete", { timeout: 10_000 });
  await app.rightClick("id:table-list >> label:products");
  await app.press("label:Alter Table…");
  await app.waitFor("id:alter-mode");
  await app.clear("id:alter-name:3");
  await app.type("id:alter-name:3", "unit_price");
  await app.press("id:alter-add-column");
  await app.waitUntil(() => app.texts().includes("ALTER TABLE"));
  await app.waitUntil(() => sqlShown(app, "RENAME COLUMN \"price\" TO \"unit_price\""));
  expect(sqlShown(app, "ADD COLUMN \"column_5\" TEXT")).toBe(true);
  expect(sqlShown(app, "PRAGMA foreign_keys")).toBe(false);
  await app.press("id:alter-execute");
  // 改列名属于需要核对的改动,同编辑器里执行一样先确认
  await app.waitFor("id:risk-dialog");
  expect(sqlShown(app, "DROP TABLE")).toBe(false);
  await app.press("id:risk-confirm");
  await app.waitFor("id:alter-dialog", { state: "gone", timeout: 10_000 });
  // 表 tab 重新加载:表头换成新列名,多出新列
  await app.waitFor("text:unit_price", { timeout: 10_000 });
  expect(app.shows("text:column_5")).toBe(true);
  expect(app.shows("text:Boquete")).toBe(true);
  app.dispose();
});

test("加外键:外键检查不过时整体回滚并列出违规行", async () => {
  const app = await openAlter("order_items");
  await app.type("id:alter-fk-cols", "product_id");
  await app.type("id:alter-fk-table", "products");
  await app.type("id:alter-fk-ref", "id");
  await app.press("id:alter-add-fk");
  await app.waitFor("id:alter-fk:0");
  await app.waitUntil(() => app.texts().includes("Rebuild table"));
  // 编辑器只渲染可见的行:完整脚本从「复制」拿
  await app.press("id:alter-copy");
  await app.waitUntil(() => (app.clipboard() ?? "").includes("FOREIGN KEY (\"product_id\") REFERENCES \"products\" (\"id\")"));
  await app.press("id:alter-execute");
  await app.waitFor("id:risk-dialog");
  await app.press("id:risk-confirm");
  await app.waitFor("id:alter-violations", { timeout: 10_000 });
  // 违规的是 rowid 2(product_id = 99)
  expect(app.texts().some((s) => s.includes("rowid 2"))).toBe(true);
  // 表单还开着,结构没变
  expect(app.shows("id:alter-dialog")).toBe(true);
  // 删掉外键后回到「无变化」
  await app.press("id:alter-fk-drop:0");
  await app.waitUntil(() => app.texts().includes("No changes"));
  app.dispose();
});

test("改表名:已打开的旧表 tab 关掉,换成新表名的 tab", async () => {
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-alter");
  await app.waitFor("id:table-list >> label:order_items");
  await app.press("id:table-list >> label:order_items");
  await app.waitFor("id:tab-strip >> label:order_items", { timeout: 10_000 });
  await app.rightClick("id:table-list >> label:order_items");
  await app.press("label:Alter Table…");
  await app.waitFor("id:alter-mode");
  await app.clear("id:alter-table-name");
  await app.type("id:alter-table-name", "line_items");
  await app.waitUntil(() => sqlShown(app, "RENAME TO \"line_items\""));
  await app.press("id:alter-execute");
  await app.waitFor("id:risk-dialog");
  await app.press("id:risk-confirm");
  await app.waitFor("id:alter-dialog", { state: "gone", timeout: 10_000 });
  await app.waitFor("id:tab-strip >> label:line_items", { timeout: 10_000 });
  expect(app.count("id:tab-strip >> label:order_items")).toBe(0);
  await app.waitFor("id:table-list >> label:line_items", { timeout: 10_000 });
  app.dispose();
});
