// AI 面板依赖的外部能力(连接、表结构、执行、标签页、设置、剪贴板)集中在这里。
// 页面与动作只经这里调用;测试入口替换其中的函数,做到不碰真实连接与数据库。

import { Clipboard } from "nsh";
import { openSettings } from "../dialogs/index";
import { chat as chatNative, db } from "../lib/native";
import type { ColumnsByTable, Connection, DbKind, QueryResult, TableDetail } from "../lib/types";
import { activate, conns, loadTables, refreshTables } from "../store/conns";
import { maxRows } from "../store/prefs";
import { setAiFullscreen, workspace } from "../store/workspace";
import { activeSelectionText, insertAndRun, newQueryTab } from "../tab/index";

export const chatDeps = {
  /** 当前连接(当前 tab 的连接,否则侧栏聚焦的连接)。 */
  headConn: (): Connection | undefined => workspace.headConn(),
  /** 当前 tab 正在浏览的表(零成本的上下文来源)。 */
  currentTable: (): { connId: string; table: string } | null => {
    const tab = workspace.activeTab();
    return tab && tab.kind === "table" && tab.table ? { connId: tab.connId, table: tab.table } : null;
  },
  /** 连接的表清单(有缓存用缓存,否则加载)。 */
  tables: async (connId: string): Promise<string[]> => {
    const cached = conns.tables(connId);
    return cached && cached.length > 0 ? cached : loadTables(connId);
  },
  /** 全库列速览(可能尚未加载)。 */
  columns: (connId: string): ColumnsByTable | null => conns.columns(connId) ?? null,
  tableDetail: (kind: DbKind, connId: string, table: string): Promise<TableDetail> =>
    db.tableDetail(kind, connId, table),
  query: (kind: DbKind, connId: string, sql: string): Promise<QueryResult> => db.query(kind, connId, sql, maxRows()),
  /** 只读会话里执行(整段只读的 SQL 走这里,作为最后一道保险)。 */
  queryReadOnly: (kind: DbKind, connId: string, sql: string): Promise<QueryResult> =>
    db.queryReadOnly(kind, connId, sql, maxRows()),
  /** 表结构变更后刷新对象列表。 */
  refreshTables: (connId: string): void => {
    void refreshTables(connId);
  },
  /** 写语句的影响范围预估(执行确认框用);改写不了的语句为 null。 */
  writeImpact: db.writeImpact,
  /** 「在编辑器中打开」:退出对话工作台,在该连接上新开查询 tab 并填入 SQL(不执行)。 */
  openQuery: (sql: string, connId: string): void => {
    setAiFullscreen(false);
    activate(connId);
    newQueryTab(sql);
  },
  systemPrompt: chatNative.systemPrompt,
  insertAndRun,
  activeSelectionText,
  openSettings: () => openSettings("ai"),
  copy: (text: string): void => {
    void Clipboard.writeText(text);
  },
};
