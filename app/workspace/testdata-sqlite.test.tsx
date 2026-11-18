// 生成测试数据对话框的交互测试(无头,macOS 桌面)。
// 运行前 DATUM_CONFIG_DIR 指向临时配置目录,第一个连接 c-td 是临时 SQLite 文件(见 run-tests.sh):
// categories 2 行、products 3 行(外键 → categories,sku 唯一,status 有 CHECK 清单),
// orphan 的外键指向空表 empty_parent。跑完 run-tests.sh 再用 sqlite3 核对库里的约束。

import { expect, open, test } from "nsh/test";

const desktop = { platform: "macos", width: 1280, height: 860, locale: "en-US" } as const;

/** 状态栏里表浏览的行数(「N rows · M cols」里的 N)。 */
function statRows(app: Awaited<ReturnType<typeof open>>) {
  const ts = app.texts();
  const i = ts.indexOf("rows ·");
  return i > 0 ? ts[i - 1] : null;
}

async function openDialog(app: Awaited<ReturnType<typeof open>>, table: string) {
  await app.rightClick(`id:table-list >> label:${table}`);
  await app.press("label:Generate Test Data");
  await app.waitFor("id:testdata-dialog");
  await app.waitFor("id:testdata-rows", { timeout: 10_000 });
}

test("自动方案 → 预览 → 插入 50 行,已打开的表多出 50 行", async () => {
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-td");
  await app.waitFor("id:table-list >> label:products");
  await app.press("id:table-list >> label:products");
  await app.waitUntil(() => app.texts().includes("Boquete"), { timeout: 10_000 });
  expect(statRows(app)).toBe("3");

  await openDialog(app, "products");
  // 自动方案:rowid 别名跳过,外键取父表的键,CHECK 清单成了取值列表
  expect(app.texts()).toContain("Row ID — numbered by the database");
  expect(app.texts().some((s) => s.startsWith("Existing keys of categories"))).toBe(true);
  expect(app.value("id:testdata-param-5-values")).toBe("active, draft, archived");
  // 预览前 10 行
  await app.waitFor("id:testdata-preview-grid", { timeout: 10_000 });
  await app.waitUntil(() => app.texts().includes("First 10 of 100 rows"), { timeout: 10_000 });

  // 固定种子:预览可复现
  await app.clear("id:testdata-seed");
  await app.type("id:testdata-seed", "12345");
  await app.clear("id:testdata-rows");
  await app.type("id:testdata-rows", "50");
  await app.waitFor("id:testdata-run >> text:Generate 50 Rows");
  await app.press("id:testdata-run");
  await app.waitUntil(() => app.texts().includes("Inserted 50 rows into products"), { timeout: 15_000 });

  await app.press("id:testdata-close");
  await app.waitFor("id:testdata-dialog", { state: "gone" });
  await app.waitUntil(() => statRows(app) === "53", { timeout: 10_000 });
  app.dispose();
});

test("复制为 INSERT:小批量直接拿 SQL 文本", async () => {
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-td");
  await app.waitFor("id:table-list >> label:categories");
  await openDialog(app, "categories");
  await app.clear("id:testdata-rows");
  await app.type("id:testdata-rows", "3");
  await app.waitUntil(() => app.enabled("id:testdata-copy-sql"));
  await app.press("id:testdata-copy-sql");
  await app.waitUntil(() => (app.clipboard() ?? "").startsWith('INSERT INTO "categories" ("name") VALUES'), { timeout: 10_000 });
  // 超过 1000 行不能复制
  await app.clear("id:testdata-rows");
  await app.type("id:testdata-rows", "5000");
  await app.waitUntil(() => !app.enabled("id:testdata-copy-sql"));
  app.dispose();
});

test("父表为空:提示先给父表造数,插入被拒绝", async () => {
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-td");
  await app.waitFor("id:table-list >> label:orphan");
  await openDialog(app, "orphan");
  await app.waitFor("id:testdata-parent-empty");
  expect(app.texts()).toContain("p_id references empty_parent, which has no rows yet.");
  expect(app.texts()).toContain("Generate for empty_parent");
  await app.press("id:testdata-run");
  await app.waitFor("id:testdata-report", { timeout: 10_000 });
  expect(app.texts().some((x) => x.includes("references empty_parent") && x.includes("Generate data for empty_parent first"))).toBe(true);
  // 一键切到父表
  await app.press("text:Generate for empty_parent");
  await app.waitUntil(() => app.texts().includes("Generate Test Data — empty_parent"), { timeout: 10_000 });
  app.dispose();
});

test("让 AI 推断各列:只发表结构,建议经本地校验后填进方案", async () => {
  const app = await open("../App.tsx", desktop);
  await app.press("id:rail-c-td");
  await app.waitFor("id:table-list >> label:products");
  await openDialog(app, "products");
  await app.waitUntil(() => app.enabled("id:testdata-ai"), { timeout: 10_000 });
  await app.press("id:testdata-ai");
  // 不存在的列、清单外的取值被丢弃;其余照填
  await app.waitUntil(() => app.texts().includes("AI updated 3 columns · Ignored: ghost → email · ok"), { timeout: 15_000 });
  expect(app.value("id:testdata-param-4-min")).toBe("5");
  expect(app.value("id:testdata-param-4-max")).toBe("50");
  expect(app.value("id:testdata-param-2-minLen")).toBe("10");
  expect(app.value("id:testdata-param-5-values")).toBe("active");
  app.dispose();
});
