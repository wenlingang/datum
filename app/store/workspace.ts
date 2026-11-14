// 工作区:标签页、各连接记住的激活 tab、侧栏/详情栏/AI 面板的开合与宽度、tab 重命名。
// 对应 GPUI 版 workspace/mod.rs 的 Workspace 状态部分。
//
// 标签页按连接隔离:中间栏只显示当前聚焦连接的 tab,每个连接各自记住激活的 tab。
// 一个 tab 的内部状态(编辑器文本、结果、分页、暂存编辑…)由 tab/ 自己的模型持有,
// 这里只存 tab 的身份与标题;`state` 字段留给 tab/ 挂它的模型对象。

import { batch, signal, type Signal } from "nsh";
import type { DbKind } from "../lib/types";
import { activate, conns } from "./conns";

export type TabKind =
  /** 表浏览(纯网格,分页);Redis 的 key、Mongo 的集合也走这里 */
  | "table"
  /** SQL / Mongo / Redis 查询工作区(编辑器 + 结果) */
  | "query"
  /** ER 图(TODO:画布暂未移植) */
  | "erd"
  /** Redis 分析:概览、整库扫描(大 key / 前缀 / TTL / 热 key)、慢日志 */
  | "redis"
  /** Redis 实时:Pub/Sub 订阅与发布、MONITOR(专用连接) */
  | "rlive"
  /** 关系库诊断(PG / MySQL / SQLite):会话、锁等待、长事务、慢 SQL、索引与空间 */
  | "diag"
  /** 表结构对比:本连接(源)对比另一个同类型的库(目标),生成迁移脚本 */
  | "diff";

export interface WorkTab {
  /** 稳定 id(标签条 key、记忆激活 tab) */
  readonly id: string;
  readonly kind: TabKind;
  readonly connId: string;
  readonly dbKind: DbKind;
  /** 表浏览的表名(Redis 为 key 名,Mongo 为集合名) */
  readonly table: string | null;
  /** 标签标题(双击可重命名) */
  readonly title: Signal<string>;
  /** tab/ 的内部模型(由 tab/ 创建与解释) */
  state: unknown;
}

/** 标签条右侧「过滤 / 导出」工具需要的当前结果信息(由 tab/ 通过 `setResultToolsProvider` 提供)。 */
export interface ResultTools {
  /** 已应用的筛选条件数(> 0 时显示「已筛选 n」胶囊) */
  filterCount: number;
  /** 筛选栏是否展开(过滤按钮的按下态) */
  filterOpen: boolean;
  /** 导出用的数据:已应用筛选后的行;没有行时返回 null(导出按钮置灰) */
  exportData: () => { table: string; columns: string[]; rows: (string | null)[][] } | null;
}

/** 侧栏 / 详情栏 / AI 面板的宽度与范围(GPUI 版 h_resizable 的 size / size_range)。 */
export const PANEL = {
  sidebar: { size: 320, min: 260, max: 520 },
  detail: { size: 320, min: 200, max: 640 },
  ai: { size: 400, min: 280, max: 900 },
} as const;

const tabs = signal<WorkTab[]>([]);
const activeByConn = signal<Record<string, string>>({});
const sidebarOpen = signal(true);
const detailOpen = signal(false);
const aiOpen = signal(false);
/** AI 对话铺满主区域(对话工作台):编辑器、标签页与右侧面板收起,侧栏保留 */
const aiFullscreen = signal(false);
const renamingId = signal<string | null>(null);
const sidebarWidth = signal<number>(PANEL.sidebar.size);
const detailWidth = signal<number>(PANEL.detail.size);
const aiWidth = signal<number>(PANEL.ai.size);
const updateAvailable = signal(false);
type ToolsProvider = (tab: WorkTab) => ResultTools | null;
const resultToolsProvider = signal<{ fn: ToolsProvider } | null>(null);
let nextTabId = 1;

export const workspace = {
  tabs: () => tabs(),
  /** 当前聚焦连接的 tab。 */
  visibleTabs: () => {
    const active = conns.activeId();
    return active ? tabs().filter((t) => t.connId === active) : [];
  },
  /** 当前 tab:该连接记住的 tab,记忆失效时回退到该连接最后一个 tab。 */
  activeTab: (): WorkTab | undefined => {
    const visible = workspace.visibleTabs();
    const active = conns.activeId();
    if (!active) return undefined;
    const remembered = activeByConn()[active];
    return visible.find((t) => t.id === remembered) ?? visible[visible.length - 1];
  },
  /** 顶栏 / AI 用的「当前连接」:当前 tab 的连接,否则侧栏聚焦的连接。 */
  headConn: () => conns.get(workspace.activeTab()?.connId) ?? conns.active(),
  sidebarOpen: () => sidebarOpen(),
  detailOpen: () => detailOpen(),
  aiOpen: () => aiOpen(),
  aiFullscreen: () => aiFullscreen(),
  /** 正在双击重命名的 tab id。 */
  renamingId: () => renamingId(),
  sidebarWidth: () => sidebarWidth(),
  detailWidth: () => detailWidth(),
  aiWidth: () => aiWidth(),
  /** 有新版本可装(顶栏设置按钮上的红点)。 */
  updateAvailable: () => updateAvailable(),
  /** 当前 tab 的结果工具信息;tab/ 未提供或无结果时为 null(不显示过滤 / 导出)。 */
  resultTools: (): ResultTools | null => {
    const tab = workspace.activeTab();
    const provider = resultToolsProvider();
    return tab && provider ? provider.fn(tab) : null;
  },
};

export function newTabId() {
  return `tab-${nextTabId++}`;
}

/** 加入并激活一个 tab(同时聚焦它的连接)。 */
export function addTab(tab: WorkTab) {
  batch(() => {
    tabs.update((l) => [...l, tab]);
    activate(tab.connId);
    focusTab(tab);
  });
}

/** 各连接记住的激活 tab(id),会话保存用。 */
export const activeTabIds = () => activeByConn();

/** 恢复上次的会话:一次放回全部 tab 与各连接记住的激活 tab(不切换当前连接)。 */
export function restoreTabs(list: WorkTab[], active: Record<string, string>) {
  batch(() => {
    tabs.set(list);
    activeByConn.set(active);
  });
}

export function focusTab(tab: WorkTab) {
  activeByConn.update((m) => ({ ...m, [tab.connId]: tab.id }));
}

/** 该连接下已打开的表浏览 tab。 */
export function findTableTab(connId: string, table: string): WorkTab | undefined {
  return tabs().find((t) => t.kind === "table" && t.connId === connId && t.table === table);
}

/** 关闭 tab;若它是该连接的激活 tab,激活同连接里原位置的邻居(右边优先,其次左边)。 */
export function closeTab(id: string) {
  const all = tabs();
  const closing = all.find((t) => t.id === id);
  if (!closing) return;
  const siblings = all.filter((t) => t.connId === closing.connId).map((t) => t.id);
  const ix = siblings.indexOf(id);
  const rest = siblings.filter((s) => s !== id);
  const neighbor = rest[ix] ?? (ix > 0 ? rest[ix - 1] : undefined);
  batch(() => {
    tabs.set(all.filter((t) => t.id !== id));
    if (activeByConn()[closing.connId] === id) {
      activeByConn.update((m) => {
        const next = { ...m };
        if (neighbor) next[closing.connId] = neighbor;
        else delete next[closing.connId];
        return next;
      });
    }
    if (renamingId() === id) renamingId.set(null);
  });
}

/** 关闭同连接下的其他 tab。 */
export function closeOtherTabs(id: string) {
  const keep = tabs().find((t) => t.id === id);
  if (!keep) return;
  batch(() => {
    tabs.update((l) => l.filter((t) => t.id === id || t.connId !== keep.connId));
    focusTab(keep);
  });
}

/** 关闭某连接的全部 tab(连接被删除时)。 */
export function closeConnTabs(connId: string) {
  batch(() => {
    tabs.update((l) => l.filter((t) => t.connId !== connId));
    activeByConn.update((m) => {
      const next = { ...m };
      delete next[connId];
      return next;
    });
  });
}

/** 开始双击重命名。 */
export function startRename(id: string) {
  renamingId.set(id);
}

/** 结束重命名:标题非空才生效。 */
export function finishRename(id: string, title: string) {
  const t = title.trim();
  const tab = tabs().find((x) => x.id === id);
  batch(() => {
    if (tab && t) tab.title.set(t);
    if (renamingId() === id) renamingId.set(null);
  });
}

export function cancelRename() {
  renamingId.set(null);
}

export const toggleSidebar = () => sidebarOpen.update((v) => !v);
export const toggleDetail = () => detailOpen.update((v) => !v);
export const setDetailOpen = (open: boolean) => detailOpen.set(open);
/** 开关 AI 面板;全屏时关面板即退出全屏。 */
export const toggleAi = () => {
  if (aiFullscreen()) {
    batch(() => {
      aiFullscreen.set(false);
      aiOpen.set(false);
    });
  } else aiOpen.update((v) => !v);
};
export const setAiOpen = (open: boolean) => aiOpen.set(open);
/** 切换对话工作台(全屏对话);进入时顺带打开面板,退出后面板仍在右侧。 */
export const toggleAiFullscreen = () =>
  batch(() => {
    const next = !aiFullscreen();
    aiFullscreen.set(next);
    if (next) aiOpen.set(true);
  });
export const setAiFullscreen = (on: boolean) => aiFullscreen.set(on);

/** 拖动分隔条后记住宽度(面板收起再展开时保持)。 */
export function setPanelWidth(panel: "sidebar" | "detail" | "ai", width: number) {
  ({ sidebar: sidebarWidth, detail: detailWidth, ai: aiWidth })[panel].set(width);
}

/** 设置页检查更新后调用:顶栏设置按钮显示红点。 */
export const setUpdateAvailable = (v: boolean) => updateAvailable.set(v);

/** tab/ 注册:由 tab 模型算出标签条右侧过滤 / 导出工具的信息(读取处应是响应式的)。 */
export function setResultToolsProvider(fn: ToolsProvider | null) {
  resultToolsProvider.set(fn ? { fn } : null);
}
