// README / 介绍页截图。不进常规测试(run-tests.sh 不跑它),要在真机上跑:
//   D=$(mktemp -d) && testing/demo/make-demo.sh "$D"
//   mkdir -p testing/demo/out
//   DATUM_CONFIG_DIR="$D" nsh test testing/demo/screenshots.test.tsx --target macos
//   testing/demo/publish.sh        # 原图在 testing/demo/out/,缩放 / 裁剪后放进 docs/screenshots/
// 窗口取 1440×922(1600×1025 的同比例):真机窗口不能超出屏幕,1600×1025 在 14 寸 MacBook 上会被截掉。
// 只用 make-demo.sh 生成的临时配置目录:演示库是临时的 roastery.db,PG / MySQL / Redis 连接指向
// 本机不常用的端口,连不上;不配置 AI 服务商,不会调用真实 AI。

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

test("table.png:亮色,products 表浏览 + 右侧行详情", async () => {
  const app = await start("light");
  await openRoastery(app);
  // 表右键「View Table Details」:打开表并展开右侧详情栏;再点一行,详情栏换成行详情
  await app.rightClick("id:table-list >> label:products");
  await app.press("label:View Table Details");
  await app.waitFor("text:Boquete Geisha", T);
  await app.press("text:Boquete Geisha");
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
