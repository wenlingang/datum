// 工作区:标签页、各连接记住的激活 tab、侧栏/详情栏/AI 面板的开合。
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
  | "erd";

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

const tabs = signal<WorkTab[]>([]);
const activeByConn = signal<Record<string, string>>({});
const sidebarOpen = signal(true);
const detailOpen = signal(false);
const aiOpen = signal(false);
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

export function focusTab(tab: WorkTab) {
  activeByConn.update((m) => ({ ...m, [tab.connId]: tab.id }));
}

export function closeTab(id: string) {
  tabs.update((l) => l.filter((t) => t.id !== id));
}

/** 关闭同连接下的其他 tab。 */
export function closeOtherTabs(id: string) {
  const keep = tabs().find((t) => t.id === id);
  if (!keep) return;
  tabs.update((l) => l.filter((t) => t.id === id || t.connId !== keep.connId));
}

/** 关闭某连接的全部 tab(连接被删除时)。 */
export function closeConnTabs(connId: string) {
  tabs.update((l) => l.filter((t) => t.connId !== connId));
}

export const toggleSidebar = () => sidebarOpen.update((v) => !v);
export const toggleDetail = () => detailOpen.update((v) => !v);
export const setDetailOpen = (open: boolean) => detailOpen.set(open);
export const toggleAi = () => aiOpen.update((v) => !v);
export const setAiOpen = (open: boolean) => aiOpen.set(open);
