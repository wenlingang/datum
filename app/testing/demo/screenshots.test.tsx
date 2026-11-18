// README / 介绍页截图。不进常规测试(run-tests.sh 不跑它),要在真机上跑:
//   D=$(mktemp -d) && testing/demo/make-demo.sh "$D"
//   testing/demo/make-demo-servers.sh up   # 新功能的截图要用 PG / MySQL / Redis 演示库(Docker),拍完 down
//   testing/demo/make-demo-servers.sh locks  # diagnostics.png 要先造出锁等待
//   mkdir -p testing/demo/out
//   DATUM_CONFIG_DIR="$D" nsh test testing/demo/screenshots.test.tsx --target macos
//   testing/demo/publish.sh        # 原图在 testing/demo/out/,缩放 / 裁剪后放进 docs/screenshots/
// 窗口取 1440×922(1600×1025 的同比例):真机窗口不能超出屏幕,1600×1025 在 14 寸 MacBook 上会被截掉。
// 只用 make-demo.sh 生成的临时配置目录:演示库是临时的 roastery.db,PG / MySQL / Redis 连接指向
// 本机不常用的端口,由 make-demo-servers.sh 在这些端口上起演示库(不起时连不上,前五张截图不受影响);
// 不配置 AI 服务商,不会调用真实 AI。每张新截图最好用新的配置目录单独跑(--filter),免得恢复出别的标签。

import { open, test, type App } from "nsh/test";

// 防误用:真机测试里读不到原生模块,没法检查 DATUM_CONFIG_DIR;每个测试先等 make-demo.sh 写的
// demo-roastery 连接出现,指向真实配置时等不到,在任何操作之前就失败。

const OUT = "testing/demo/out";
const T = { timeout: 20_000 };

async function start(scheme: "light" | "dark"): Promise<App> {
  const app = await open("../../App.tsx", {
    platform: "macos",
    width: 1440,
    height: 922,
    colorScheme: scheme,
    locale: "en-US",
  });
  await app.waitFor("id:rail-demo-roastery", T);
  return app;
}

/** 聚焦 roastery 并等表列表加载完。表名在侧栏被拆成两段文字,按 accessibilityLabel 选。 */
async function openRoastery(app: App) {
  await app.press("id:rail-demo-roastery");
  await app.waitFor("id:table-list >> label:subscriptions", T);
}

/** 新建查询 tab 并把编辑器内容换成 `sql`(粘贴,避免逐键输入时编辑器自动补括号 / 缩进)。 */
async function newQuery(app: App, sql: string) {
  await app.keyboard.press("mod+t");
  await app.waitFor("id:sql-editor", T);
  await app.paste("id:sql-editor", { text: sql });
  // 光标回到开头,编辑器滚回顶部
  await app.keyboard.press("mod+up");
}

/** 等右下角的提示都消失(错误提示 6 秒)。 */
async function noToasts(app: App) {
  await app.wait(6500);
}

const TOP_COFFEES = `-- Top single-origin coffees by revenue
SELECT p.name,
       p.origin,
       p.roast_level,
       COUNT(DISTINCT o.id)                                     AS orders,
       SUM(oi.quantity)                                         AS bags_sold,
       ROUND(SUM(oi.quantity * oi.unit_price_cents) / 100.0, 2) AS revenue_usd,
       (SELECT ROUND(AVG(r.rating), 1)
          FROM reviews r
         WHERE r.product_id = p.id)                             AS rating
FROM products p
JOIN categories c   ON c.id = p.category_id
JOIN order_items oi ON oi.product_id = p.id
JOIN orders o       ON o.id = oi.order_id
WHERE c.name = 'Single Origin'
  AND o.status NOT IN ('cancelled', 'refunded')
GROUP BY p.id
ORDER BY revenue_usd DESC
LIMIT 12;`;

test("sql-editor.png:暗色,查询 tab 跑出按销售额排名的单品豆", async () => {
  const app = await start("dark");
  await openRoastery(app);
  await newQuery(app, TOP_COFFEES);
  await app.press("id:run");
  await app.waitFor("text:Boquete Geisha", T);
  // 鼠标移开「运行」按钮,截图里不带悬停提示
  await app.hover("text:-- Top single-origin coffees by revenue");
  await app.wait(800);
  await app.screenshot(`${OUT}/sql-editor.png`);
}, 90_000);

test("table.png:亮色,products 表浏览 + 行详情弹窗", async () => {
  const app = await start("light");
  await openRoastery(app);
  // 单击打开表;在一行上右键「View Details」打开行详情弹窗
  await app.press("id:table-list >> label:products");
  await app.waitFor("text:Boquete Geisha", T);
  await app.rightClick("text:Boquete Geisha");
  await app.chooseMenuItem("View Details");
  await app.waitFor("id:row-detail", T);
  await app.wait(800);
  await app.screenshot(`${OUT}/table.png`);
}, 90_000);

test("new-connection.png:亮色,新建连接对话框", async () => {
  const app = await start("light");
  await openRoastery(app);
  await app.press("label:New Connection");
  await app.waitFor("id:conn-form", T);
  await app.type("id:conn-name", "analytics-prod");
  await app.wait(800);
  await app.screenshot(`${OUT}/new-connection.png`);
}, 90_000);

test("destructive-review.png:破坏性语句的审查弹窗", async () => {
  const app = await start("dark");
  // 审查在执行前,不需要连上;用 PG 连接,弹窗里显示库名而不是本机文件路径
  await app.press("id:rail-demo-analytics");
  await newQuery(app, "DELETE FROM orders;\nDROP TABLE subscriptions;");
  await noToasts(app);
  // 选中全部,两条语句一起审查
  await app.keyboard.press("mod+a");
  await app.press("id:run");
  await app.waitFor("text:Review Destructive Statements", T);
  await app.wait(800);
  await app.screenshot(`${OUT}/destructive-review.png`);
  await app.press("id:risk-cancel");
}, 90_000);

test("ai-assistant.png:暗色,AI 面板未配置时的引导", async () => {
  const app = await start("dark");
  await openRoastery(app);
  await newQuery(app, TOP_COFFEES);
  await app.press("id:run");
  await app.waitFor("text:Boquete Geisha", T);
  await app.press("id:toggle-ai");
  await app.waitFor("id:ai-open-settings", T);
  // 鼠标移开按钮;等演示里连不上的 PG / MySQL 报错提示消失
  await app.hover("text:-- Top single-origin coffees by revenue");
  await noToasts(app);
  await app.screenshot(`${OUT}/ai-assistant.png`);
}, 90_000);

// ---- 0.2.x 新功能(需要 make-demo-servers.sh up 起的演示数据库)----

test("erd.png:亮色,roastery 整库的关系图", async () => {
  const app = await start("light");
  await openRoastery(app);
  await app.press("label:Relationship Diagram");
  await app.waitFor("id:erd-node:orders", T);
  await app.waitFor("id:erd-node:order_items", T);
  await app.hover("id:erd-node:customers");
  await app.wait(1200);
  await app.screenshot(`${OUT}/erd.png`);
}, 90_000);

test("jsonb.png:暗色,PG 的 JSONB 行详情展开成树,点字段筛选整张表", async () => {
  const app = await start("dark");
  await app.press("id:rail-demo-analytics");
  // 单击打开表;在一行上右键「View Details」打开行详情弹窗,JSON 树里点字段生成筛选(弹窗随之关闭)
  await app.waitFor("id:table-list >> label:web_events", T);
  await app.press("id:table-list >> label:web_events");
  await app.waitFor("text:/\"total_cents\": 1563/", T);
  await app.rightClick("text:/\"total_cents\": 1563/");
  await app.chooseMenuItem("View Details");
  await app.waitFor("id:row-json:payload", T);
  await app.press("id:json-node:customer");
  await app.waitFor("id:json-node:customer.tier", T);
  await app.press("id:json-node:customer.tier");
  await app.press("id:json-filter");
  await app.waitFor("id:filter-server-hint", T);
  // 筛选后对另一行(tier 也是 silver)再打开行详情,展开到这个字段
  await app.waitFor("text:/\"total_cents\": 1902/", T);
  await app.rightClick("text:/\"total_cents\": 1902/");
  await app.chooseMenuItem("View Details");
  await app.waitFor("id:row-json:payload", T);
  await app.press("id:json-node:customer");
  await app.waitFor("id:json-node:customer.tier", T);
  await app.press("id:json-node:customer.tier");
  await app.wait(1500);
  await app.screenshot(`${OUT}/jsonb.png`);
}, 90_000);

test("mysql-alter.png:亮色,MySQL 改列类型前的风险评估", async () => {
  const app = await start("light");
  await app.press("id:rail-demo-shop");
  await app.waitFor("id:table-list >> label:orders", T);
  await newQuery(app, "ALTER TABLE orders MODIFY total_cents BIGINT NOT NULL;");
  await app.press("id:run");
  await app.waitFor("id:risk-dialog", T);
  await app.waitFor("id:risk-alter-risk", T);
  await app.wait(800);
  await app.screenshot(`${OUT}/mysql-alter.png`);
  await app.press("id:risk-cancel");
}, 90_000);

test("redis.png:暗色,Redis 按类型编辑 Hash", async () => {
  const app = await start("dark");
  await app.press("id:rail-demo-cache");
  await app.press("id:table-list >> label:customer:1042");
  await app.waitFor("text:Maya Okafor", T);
  await app.hover("id:table-list >> label:customer:1043");
  await app.wait(1000);
  await app.screenshot(`${OUT}/redis.png`);
}, 90_000);

test("sqlite-alter.png:亮色,SQLite 表单改列类型,预览 12 步重建表脚本", async () => {
  const app = await start("light");
  await openRoastery(app);
  await app.rightClick("id:table-list >> label:products");
  await app.press("label:Alter Table");
  await app.waitFor("id:alter-dialog", T);
  // 第 5 列 origin:TEXT → VARCHAR(40)
  await app.clear("id:alter-type:4");
  await app.type("id:alter-type:4", "VARCHAR(40)");
  await app.waitUntil(() => app.texts().includes("Rebuild table"), T);
  await app.waitUntil(() => app.texts().some((x) => x.includes("PRAGMA foreign_keys = OFF;")), T);
  await app.wait(1000);
  await app.screenshot(`${OUT}/sqlite-alter.png`);
}, 90_000);

// ---- 0.2.4 新功能(都用 roastery SQLite 库,不需要演示数据库)----

test("masking.png:暗色,演示模式下 customers 表的敏感列打码", async () => {
  const app = await start("dark");
  await openRoastery(app);
  await app.press("id:demo-toggle");
  await app.waitFor("id:demo-badge", T);
  await app.press("id:table-list >> label:customers");
  await app.waitFor("text:first_name", T);
  await app.wait(1000);
  await app.screenshot(`${OUT}/masking.png`);
  // 点 DEMO 徽章退出演示模式(打开后顶栏开关换成徽章)
  await app.press("id:demo-badge");
}, 90_000);

test("data-compare.png:亮色,products 与改过几行的副本对比数据", async () => {
  const app = await start("light");
  await openRoastery(app);
  // 做一份带主键的副本,改两行、删一行、加一行(本地连接,不弹审查)
  await newQuery(
    app,
    `CREATE TABLE products_backup (
  id INTEGER PRIMARY KEY, category_id INTEGER NOT NULL, sku TEXT NOT NULL, name TEXT NOT NULL,
  origin TEXT, region TEXT, roast_level TEXT, price_cents INTEGER NOT NULL, stock INTEGER NOT NULL DEFAULT 0,
  attributes TEXT, active BOOLEAN NOT NULL DEFAULT 1, created_at TEXT NOT NULL
);
INSERT INTO products_backup SELECT * FROM products;
UPDATE products_backup SET price_cents = price_cents + 200 WHERE id IN (2, 7);
UPDATE products_backup SET stock = 0 WHERE id = 11;
DELETE FROM products_backup WHERE id = 5;
INSERT INTO products_backup (id, category_id, sku, name, origin, region, roast_level, price_cents, stock, attributes, active, created_at)
  VALUES (99, 1, 'SO-OLD-099', 'Retired Blend', 'Brazil', 'Cerrado', 'dark', 1500, 0, '{}', 0, '2024-01-01 00:00:00');`,
  );
  await app.keyboard.press("mod+shift+enter");
  await app.waitFor("text:/Executed successfully/", T);
  await app.rightClick("id:table-list >> label:products");
  await app.press("label:Compare with Another Table");
  await app.waitFor("id:data-diff", T);
  // 目标默认是同一连接里的同名表,改选副本:点下拉框里显示的表名打开,再选 products_backup
  await app.waitFor("id:ddiff-target-table >> text:products", T);
  await app.press("id:ddiff-target-table >> text:products");
  // 侧栏表列表里也有 products_backup(一整段文字),下拉列表在它之后出现:取第二个
  await app.press("text:products_backup >> nth=1");
  await app.waitUntil(() => app.enabled("id:ddiff-run"), T);
  await app.press("id:ddiff-run");
  await app.waitFor("id:ddiff-summary", T);
  await app.wait(1000);
  await app.screenshot(`${OUT}/data-compare.png`);
}, 120_000);

test("index-advisor.png:暗色,慢查询的索引建议与执行计划对比", async () => {
  const app = await start("dark");
  await openRoastery(app);
  await newQuery(
    app,
    `-- Recent shipped web orders
SELECT id, customer_id, total_cents, placed_at
FROM orders
WHERE status = 'shipped'
  AND channel = 'web'
ORDER BY placed_at DESC
LIMIT 20;`,
  );
  await app.press("id:run-limit");
  await app.press("id:suggest-indexes");
  await app.waitFor("id:index-advisor", T);
  await app.waitFor("id:advisor-plan-after", T);
  await app.wait(1000);
  await app.screenshot(`${OUT}/index-advisor.png`);
}, 90_000);

test("journal.png:亮色,写操作记录:一次 UPDATE 与改动前的行", async () => {
  const app = await start("light");
  await openRoastery(app);
  await newQuery(app, "UPDATE products SET stock = stock - 1 WHERE origin = 'Ethiopia';");
  await app.press("id:run");
  await app.waitFor("text:/Executed successfully/", T);
  await app.press("label:Connection Actions");
  await app.press("text:Write Journal");
  await app.waitFor("id:journal-view", T);
  await app.waitFor("id:journal-list >> text:/^UPDATE products SET stock/", T);
  await app.press("id:journal-list >> text:/^UPDATE products SET stock/");
  await app.waitFor("id:journal-snapshot", T);
  await app.wait(1000);
  await app.screenshot(`${OUT}/journal.png`);
}, 90_000);

test("test-data.png:暗色,给 orders 生成测试数据的对话框与预览", async () => {
  const app = await start("dark");
  await openRoastery(app);
  await app.rightClick("id:table-list >> label:orders");
  await app.press("label:Generate Test Data");
  await app.waitFor("id:testdata-dialog", T);
  await app.waitFor("id:testdata-preview-grid", T);
  await app.wait(1000);
  await app.screenshot(`${OUT}/test-data.png`);
}, 90_000);

test("diagnostics.png:暗色,PG 诊断页的锁等待(先跑 make-demo-servers.sh locks)", async () => {
  const app = await start("dark");
  await app.press("id:rail-demo-analytics");
  await app.waitFor("id:table-list >> label:orders", T);
  await app.press("label:Database Diagnostics");
  await app.waitFor("id:diag-tab-locks", T);
  await app.press("id:diag-tab-locks");
  await app.waitFor("text:/UPDATE orders SET status/", T);
  await noToasts(app);
  await app.screenshot(`${OUT}/diagnostics.png`);
}, 90_000);
