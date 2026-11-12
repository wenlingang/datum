// 会话:重启后恢复上次打开的标签页与内容。
// 保存:标签列表(类型、连接、表名、标题、查询 tab 的 SQL)、各连接记住的激活 tab、当前连接;
// 有变化后防抖写进配置目录的 session.json。恢复:只放回标签与内容,不自动执行;
// 当前连接照常连上,表浏览 tab 显示出来、连接就绪后才查数据(见 TabView)。

import { effect, signal } from "nsh";
import { t } from "nsh/i18n";
import { store } from "../lib/native";
import { isSupportedKind, supportsSql } from "../lib/types";
import { activate, conns, loadTables } from "../store/conns";
import { activeTabIds, newTabId, restoreTabs, type TabKind, type WorkTab, workspace } from "../store/workspace";
import { createModel, modelOf, setSql } from "./model";

/** 写盘前等多久(连续输入只写最后一次)。 */
const SAVE_DELAY_MS = 800;
/** 单个查询 tab 最多保存的 SQL 长度(防止误粘贴的大段文本把会话文件撑大)。 */
const MAX_SQL_CHARS = 200_000;

interface SessionTab {
  kind: TabKind;
  connId: string;
  table: string | null;
  title: string;
  sql?: string;
}

interface Session {
  version: 1;
  tabs: SessionTab[];
  /** 连接 → 它激活的 tab 在 tabs 里的下标 */
  active: Record<string, number>;
  activeConn: string | null;
}

function snapshot(): Session {
  const list = workspace.tabs();
  const ids = activeTabIds();
  const active: Record<string, number> = {};
  for (const [connId, id] of Object.entries(ids)) {
    const ix = list.findIndex((x) => x.id === id);
    if (ix >= 0) active[connId] = ix;
  }
  return {
    version: 1,
    tabs: list.map((tab) => {
      const m = modelOf(tab);
      const sql = tab.kind === "query" && m ? m.sql() : undefined;
      return {
        kind: tab.kind,
        connId: tab.connId,
        table: tab.table,
        title: tab.title(),
        ...(sql !== undefined ? { sql: sql.slice(0, MAX_SQL_CHARS) } : {}),
      };
    }),
    active,
    activeConn: conns.activeId(),
  };
}

/** 由保存的一项重建 tab;连接已删除或类型不再支持时跳过。 */
function rebuild(s: SessionTab): WorkTab | null {
  const conn = conns.get(s.connId);
  if (!conn || !isSupportedKind(conn.kind)) return null;
  const base = { id: newTabId(), kind: s.kind, connId: conn.id, dbKind: conn.kind, table: s.table, title: signal(s.title) };
  switch (s.kind) {
    case "query": {
      if (conn.kind === "mongo") return null;
      const m = createModel("query", conn.id, conn.kind, null);
      if (s.sql) setSql(m, s.sql);
      return { ...base, table: null, state: m };
    }
    case "table": {
      if (!s.table || conn.kind === "mongo") return null;
      const m = createModel("table", conn.id, conn.kind, s.table);
      m.pendingLoad = true;
      return { ...base, state: m };
    }
    case "diag":
      return supportsSql(conn.kind) ? { ...base, title: signal(t("diag.title")), state: null } : null;
    case "redis":
      return conn.kind === "redis" ? { ...base, title: signal(t("rinsight.title")), state: null } : null;
    case "erd":
      return supportsSql(conn.kind) ? { ...base, title: signal(t("erd.title")), state: null } : null;
    default:
      return null;
  }
}

/** 启动时调用:放回上次的标签与内容,并连上上次的当前连接。 */
export function restoreSession() {
  const raw = store.loadSession() as Partial<Session> | null;
  if (!raw || raw.version !== 1 || !Array.isArray(raw.tabs)) return;
  const kept: WorkTab[] = [];
  const index = new Map<number, WorkTab>();
  raw.tabs.forEach((s, i) => {
    const tab = rebuild(s);
    if (tab) {
      kept.push(tab);
      index.set(i, tab);
    }
  });
  const active: Record<string, string> = {};
  for (const [connId, ix] of Object.entries(raw.active ?? {})) {
    const tab = index.get(ix);
    if (tab && tab.connId === connId) active[connId] = tab.id;
  }
  restoreTabs(kept, active);
  const head = raw.activeConn && conns.get(raw.activeConn) ? raw.activeConn : null;
  if (head) {
    activate(head);
    if (!conns.tables(head) && !conns.isLoading(head)) void loadTables(head);
  }
}

/** 启动时调用(在恢复之后):有变化就防抖保存。 */
export function persistSession() {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let last = "";
  effect(() => {
    // 读一遍所有会变的状态,作为依赖
    const json = JSON.stringify(snapshot());
    if (json === last) return;
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      last = json;
      try {
        store.saveSession(JSON.parse(json));
      } catch {
        // 写不了(磁盘满、权限)就算了,下次变化再试
      }
    }, SAVE_DELAY_MS);
  });
}
