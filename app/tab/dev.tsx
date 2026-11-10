// 临时开发入口:不经外壳,直接挂 TabView + DetailPanel + TabStatus,验证查询/表浏览标签页。
// 用法(只连临时目录里的库):DATUM_CONFIG_DIR=<临时目录> nsh run TabDev.tsx --port <端口>
// (nsh 以入口所在目录为应用目录,入口须在 app/ 下,故由 app/TabDev.tsx 引入本文件;tab/tab.test.tsx 也用它。)
// 明暗由配置目录的 ui_prefs.json 决定。下面的 DATUM_DEV_* 只在 JS 能读到宿主环境变量时生效(macOS 开发宿主读不到)。

import { keymap, mount, setColorScheme, Text, View } from "nsh";
import { Button } from "nsh/base";
import { activate, conns, loadTables, reloadConnections } from "../store/conns";
import { apply as applyPrefs } from "../store/prefs";
import { dismissToast, toasts } from "../store/toast";
import { focusTab, setDetailOpen, toggleDetail, workspace } from "../store/workspace";
import { DbIcon, Dot, Icon, IconButton } from "../ui/kit";
import { FS } from "../ui/text";
import { DetailPanel, modelOf, newQueryTab, openErd, openTable, runTabCommand, TabStatus, TabView } from "./index";

applyPrefs();
reloadConnections();

const env = (k: string): string | undefined => {
  try {
    return process.env[k];
  } catch {
    return undefined;
  }
};

const theme = env("DATUM_DEV_THEME");
if (theme === "light" || theme === "dark") setColorScheme(theme);

const connId = env("DATUM_DEV_CONN") ?? conns.list()[0]?.id;

async function boot() {
  if (!connId) return;
  activate(connId);
  await loadTables(connId);
  const view = env("DATUM_DEV_VIEW") ?? "query";
  openTable(connId, "products");
  const q = newQueryTab(
    "-- Top products by revenue\nSELECT p.name,\n       p.origin,\n       p.roast_level,\n       COUNT(o.id) AS orders,\n       SUM(o.quantity) AS bags_sold,\n       ROUND(SUM(o.quantity * p.price_cents) / 100.0, 2) AS revenue_usd\nFROM products p\nJOIN orders o ON o.product_id = p.id\nGROUP BY p.id\nORDER BY revenue_usd DESC;",
  );
  if (view === "table") {
    const t = workspace.tabs().find((x) => x.kind === "table");
    if (t) focusTab(t);
  } else if (q) focusTab(q);
  // 截图用:自动运行查询 / 打开详情栏并选中一行
  if (env("DATUM_DEV_RUN") && view !== "table") runTabCommand("run");
  const row = env("DATUM_DEV_ROW");
  if (row) {
    setTimeout(() => {
      const m = modelOf(workspace.activeTab());
      m?.selectedRow.set(Number(row));
      setDetailOpen(true);
    }, 800);
  }
}
void boot();

// 外壳的全局键位(这里代为绑定,行为与 GPUI 版 actions.rs 一致)
keymap.bind({
  "mod+s": () => runTabCommand("commit"),
  "mod+r": () => runTabCommand("refresh"),
  "mod+y": () => runTabCommand("history"),
  "mod+f": () => runTabCommand("filter"),
  "mod+e": () => runTabCommand("explain"),
  "mod+shift+f": () => runTabCommand("format"),
  "mod+t": () => newQueryTab(),
});

function TabStrip() {
  return (
    <View class="h-10 shrink-0 flex-row items-center gap-1 border-b border-border bg-tab-bar px-2">
      <Each of={workspace.visibleTabs()} key={(t) => t.id}>
        {(t) => (
          <Button
            testID={`tab:${t().title()}`}
            onPress={() => focusTab(t())}
            selected={workspace.activeTab()?.id === t().id}
            class="h-8 flex-row items-center gap-1.5 rounded-(--radius) px-3 hover:bg-list-hover selected:bg-tab-active"
          >
            <Icon name={t().kind === "query" ? "terminal" : t().kind === "erd" ? "network" : "table"} size={13} />
            <Text class={`${FS.sm} font-semibold text-foreground`}>{t().title()}</Text>
          </Button>
        )}
      </Each>
      <View class="flex-1" />
      <Each of={conns.tables(connId ?? "") ?? []}>
        {(name) => (
          <Button testID={`open:${name()}`} onPress={() => connId && openTable(connId, name())} class="px-1.5">
            <Text class={`${FS.xs} text-muted-foreground`}>{name()}</Text>
          </Button>
        )}
      </Each>
      <IconButton icon="plus" tooltip="new query" onPress={() => newQueryTab()} />
      <IconButton icon="network" tooltip="erd" onPress={() => connId && openErd(connId)} />
      <IconButton icon="panel-left" tooltip="detail" active={workspace.detailOpen()} onPress={toggleDetail} />
    </View>
  );
}

function StatusBar() {
  const head = () => workspace.headConn();
  return (
    <View class="h-7 shrink-0 flex-row items-center gap-2.5 border-t border-border bg-status-bar pl-3.5 pr-2.5">
      <If when={head()}>
        {(c) => (
          <View class="flex-row items-center gap-1.5">
            <Dot color="success" />
            <DbIcon kind={c().kind} size={12} />
            <Text class={`${FS.xs} text-muted-foreground`}>Connected</Text>
          </View>
        )}
      </If>
      <TabStatus />
      <Text class={`${FS.xs} font-mono text-fg-faint`}>v0.0.1</Text>
    </View>
  );
}

function Dev() {
  return (
    <View class={`flex-1 bg-background ${FS.sm}`}>
      <View class="h-10 shrink-0 bg-title-bar" />
      <TabStrip />
      <View class="min-h-0 flex-1 flex-row">
        <View class="min-w-0 flex-1">
          <If when={workspace.activeTab()}>{(t) => <TabView tab={t()} />}</If>
        </View>
        <If when={workspace.detailOpen()}>
          <View class="w-[340px]">
            <DetailPanel />
          </View>
        </If>
      </View>
      <StatusBar />
      <View class="absolute bottom-10 right-4 w-80 gap-2" style={{ pointerEvents: "box-none" }}>
        <Each of={toasts()} key={(t) => t.id}>
          {(item) => (
            <View
              onPress={() => dismissToast(item().id)}
              class="rounded-(--radius-lg) border border-border bg-popover px-3 py-2.5 shadow-lg"
            >
              <Text class={`${FS.sm} text-popover-foreground`}>{item().text}</Text>
            </View>
          )}
        </Each>
      </View>
    </View>
  );
}

mount(Dev);
