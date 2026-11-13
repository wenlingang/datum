// tab/ 对外接口(外壳、AI 面板只经这里与标签页交互)。签名是约定,改动需同步调用方。

import { signal } from "nsh";
import { t } from "nsh/i18n";
import { activate, conns, loadTables } from "../store/conns";
import { supportsSql } from "../lib/types";
import { toast } from "../store/toast";
import { addTab, focusTab, newTabId, setResultToolsProvider, workspace, type WorkTab } from "../store/workspace";
import { DetailPanelView } from "./DetailPanel";
import {
  beautify,
  clearSelection,
  commit,
  createModel,
  explain,
  exportName,
  isActive,
  modelOf,
  refresh,
  run,
  selectionText,
  sendSelectionToAi,
  setSql,
  toggleFilter,
  viewRows,
} from "./model";
import { TabStatusView } from "./TabStatus";
import { TabBody } from "./TabView";

export { modelOf, type TabModel } from "./model";

/** 标签页主体(编辑器 + 结果 / 表浏览网格 / ER 图占位)。换 tab 时整体重建。 */
export function TabView(props: { tab: WorkTab }) {
  return (
    <Each of={[props.tab]} key={(tab) => tab.id}>
      {(tab) => <TabBody tab={tab()} />}
    </Each>
  );
}

/** 右侧详情栏:行详情 / 表结构 / Redis key 详情(跟随当前 tab)。 */
export function DetailPanel() {
  return <DetailPanelView />;
}

/** 状态栏里属于当前 tab 的部分:行列数、耗时、分页控件等。 */
export function TabStatus() {
  return <TabStatusView />;
}

function makeTab(kind: "table" | "query", connId: string, table: string | null, title: string): WorkTab | undefined {
  const conn = conns.get(connId);
  if (!conn) return undefined;
  return {
    id: newTabId(),
    kind,
    connId,
    dbKind: conn.kind,
    table,
    title: signal(title),
    state: createModel(kind, connId, conn.kind, table),
  };
}

/** 打开(或聚焦已打开的)表浏览 tab;Redis 为 key。MongoDB 不支持:提示并返回 undefined。 */
export function openTable(connId: string, table: string): WorkTab | undefined {
  if (conns.get(connId)?.kind === "mongo") {
    toast.info(t("tab.mongo_unsupported"));
    return undefined;
  }
  const existing = workspace
    .tabs()
    .find((x) => x.kind === "table" && x.connId === connId && x.table === table);
  if (existing) {
    addTabFocus(existing);
    return existing;
  }
  const tab = makeTab("table", connId, table, table);
  if (!tab) return undefined;
  addTab(tab);
  const m = modelOf(tab);
  if (m) refresh(m);
  return tab;
}

function addTabFocus(tab: WorkTab) {
  // addTab 会激活连接;已有 tab 只需激活连接并聚焦它
  activate(tab.connId);
  focusTab(tab);
}

/** 在当前连接下新开查询 tab,可预填 SQL。未选连接时提示并返回 undefined。 */
export function newQueryTab(sql?: string): WorkTab | undefined {
  const conn = workspace.headConn();
  if (!conn) {
    toast.error(t("workspace.select_conn_first"));
    return undefined;
  }
  const n = workspace.tabs().filter((x) => x.kind === "query" && x.connId === conn.id).length;
  if (conn.kind === "mongo") {
    toast.info(t("tab.mongo_unsupported"));
    return undefined;
  }
  const base = "SQL Query";
  const tab = makeTab("query", conn.id, null, n === 0 ? base : `${base} ${n + 1}`);
  if (!tab) return undefined;
  addTab(tab);
  if (!conns.isConnected(conn.id)) void loadTables(conn.id);
  const model = modelOf(tab);
  if (sql && model) setSql(model, sql);
  return tab;
}

/** 集合右键「插入文档」:打开集合 tab 并弹出新增文档编辑器。 */
export function insertDocument(connId: string, coll: string) {
  void connId;
  void coll;
  toast.info(t("tab.mongo_unsupported"));
}

/** 打开 ER 图 tab(画布 TODO)。 */
export function openErd(connId: string) {
  const conn = conns.get(connId);
  if (!conn) return;
  const existing = workspace.tabs().find((x) => x.kind === "erd" && x.connId === connId);
  if (existing) {
    addTabFocus(existing);
    return;
  }
  addTab({
    id: newTabId(),
    kind: "erd",
    connId,
    dbKind: conn.kind,
    table: null,
    title: signal(t("erd.title")),
    state: null,
  });
}

/** 打开 Redis 分析 tab(每个连接一个,已开则切过去)。 */
export function openRedisInsight(connId: string) {
  const conn = conns.get(connId);
  if (!conn || conn.kind !== "redis") return;
  const existing = workspace.tabs().find((x) => x.kind === "redis" && x.connId === connId);
  if (existing) {
    addTabFocus(existing);
    return;
  }
  addTab({
    id: newTabId(),
    kind: "redis",
    connId,
    dbKind: conn.kind,
    table: null,
    title: signal(t("rinsight.title")),
    state: null,
  });
}

/** 打开数据库诊断 tab(PG / MySQL / SQLite,每个连接一个,已开则切过去)。 */
export function openDiagnostics(connId: string) {
  const conn = conns.get(connId);
  if (!conn || !supportsSql(conn.kind)) return;
  const existing = workspace.tabs().find((x) => x.kind === "diag" && x.connId === connId);
  if (existing) {
    addTabFocus(existing);
    return;
  }
  addTab({
    id: newTabId(),
    kind: "diag",
    connId,
    dbKind: conn.kind,
    table: null,
    title: signal(t("diag.title")),
    state: null,
  });
}

/** 打开表结构对比 tab:本连接为源库,在 tab 里选目标库(每个源库一个,已开则切过去)。 */
export function openSchemaDiff(connId: string) {
  const conn = conns.get(connId);
  if (!conn || !supportsSql(conn.kind)) return;
  const existing = workspace.tabs().find((x) => x.kind === "diff" && x.connId === connId);
  if (existing) {
    addTabFocus(existing);
    return;
  }
  addTab({
    id: newTabId(),
    kind: "diff",
    connId,
    dbKind: conn.kind,
    table: null,
    title: signal(t("sdiff.title")),
    state: null,
  });
}

/** 在指定连接上新开查询 tab 并放入 SQL(不执行)。 */
export function openQueryOn(connId: string, sql: string) {
  activate(connId);
  if (!conns.tables(connId) && !conns.isLoading(connId)) void loadTables(connId);
  newQueryTab(sql);
}

/** AI 生成的 SQL「编辑器运行」:只在 connId 上执行;`confirmed` 为卡片已做过破坏性确认。
 *  当前查询 tab 属于该连接则复用,否则切到该连接并新开查询 tab,绝不打到别的库。 */
export function insertAndRun(sql: string, connId: string, confirmed: boolean) {
  if (!conns.get(connId)) {
    toast.error(t("workspace.chat_conn_gone"));
    return;
  }
  let tab: WorkTab | undefined = workspace.tabs().find((x) => x.id === workspace.activeTab()?.id);
  if (!tab || tab.kind !== "query" || tab.connId !== connId) {
    // 先聚焦目标连接,newQueryTab 才会建在它下面
    activate(connId);
    tab = newQueryTab();
  }
  const m = modelOf(tab);
  if (!tab || !m || tab.connId !== connId) return;
  focusTab(tab);
  setSql(m, sql);
  // 未经确认的(静态检查判定为只读而免确认)在只读模式下执行:
  // 静态检查看不穿视图里的写函数等副作用,由数据库兜底拒绝写入
  m.readOnlyNext = !confirmed;
  // 仅当卡片已弹过破坏性确认时跳过审查;其余一律走审查门
  run(m, sql, false, confirmed);
}

/** 当前 tab 编辑器里选中的文本(引用给 AI)。 */
export function activeSelectionText(): string {
  const m = modelOf(workspace.activeTab());
  return m ? selectionText(m) : "";
}

/** 外壳打开详情栏 / 「查看表详情」时调用:清掉当前 tab 遗留的行选中,让右栏展示表详情
 *  (GPUI 版 toggle_detail 里的 clear_selection)。 */
export function clearRowSelection() {
  const m = modelOf(workspace.activeTab());
  if (m) clearSelection(m);
}

// 标签条右侧「已筛选 n / 过滤 / 导出」靠这里取当前结果的信息
setResultToolsProvider((tab) => {
  const m = modelOf(tab);
  if (!m || !m.result()) return null;
  return {
    filterCount: m.applied().filter(isActive).length,
    filterOpen: m.filterOpen(),
    exportData: () => {
      const result = m.result();
      const rows = viewRows(m);
      return result && rows.length > 0 ? { table: exportName(m), columns: result.columns, rows } : null;
    },
  };
});

/** 快捷键/菜单命令转给当前 tab:运行、格式化、解释、历史、过滤、提交编辑、刷新。 */
export type TabCommand = "run" | "format" | "explain" | "history" | "filter" | "commit" | "refresh" | "sendToAi";
export function runTabCommand(cmd: TabCommand) {
  const m = modelOf(workspace.activeTab());
  if (!m) return;
  switch (cmd) {
    case "run":
      if (m.kind === "query") run(m);
      break;
    case "format":
      beautify(m);
      break;
    case "explain":
      explain(m);
      break;
    case "history":
      if (m.kind === "query") m.historyOpen.update((v) => !v);
      break;
    case "filter":
      toggleFilter(m);
      break;
    case "commit":
      void commit(m);
      break;
    case "refresh":
      refresh(m);
      break;
    case "sendToAi":
      sendSelectionToAi(m);
      break;
  }
}
