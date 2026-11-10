// datum/native 的类型化封装:原生导出返回 `unknown`(JSON),这里断言成 lib/types 的形状。
// 界面代码只从这里调用原生模块,不直接 import "datum/native"。

import { Ai, Db, Prefs as NativePrefs, Sql, Store } from "datum/native";
import type {
  AiSettings,
  AiStatus,
  CellEdit,
  ChatEvent,
  ColumnsByTable,
  Connection,
  DbKind,
  HistoryEntry,
  KnowledgeDoc,
  Prefs,
  ProviderConfig,
  QueryResult,
  RiskFinding,
  RowDelete,
  TableDetail,
} from "./types";

export const store = {
  connections: () => Store.connections() as Connection[],
  saveConnection: (conn: Connection, password: string | null) =>
    Store.saveConnection(conn, password) as Connection,
  deleteConnection: (id: string) => Store.deleteConnection(id),
  savedPassword: (id: string) => Store.savedPassword(id),
  switchDatabase: (id: string, database: string) => Store.switchDatabase(id, database) as Connection,
  testConnection: (conn: Connection, password: string) => Store.testConnection(conn, password),
  history: () => Store.history() as HistoryEntry[],
  recordHistory: (entry: Omit<HistoryEntry, "id">) =>
    Store.recordHistory({ ...entry, id: "" }) as HistoryEntry[] | null,
  clearHistory: (connId: string | null) => Store.clearHistory(connId) as HistoryEntry[],
  readSqlFile: (path: string) => Store.readSqlFile(path),
  writeTextFile: (path: string, contents: string) => Store.writeTextFile(path, contents),
};

export const db = {
  connect: (kind: DbKind, id: string) => Db.connect(kind, id),
  disconnect: (kind: DbKind, id: string) => Db.disconnect(kind, id),
  listObjects: async (kind: DbKind, id: string) => (await Db.listObjects(kind, id)) as string[],
  listDatabases: async (kind: DbKind, id: string) => (await Db.listDatabases(kind, id)) as string[],
  fetchColumns: async (kind: DbKind, id: string) => (await Db.fetchColumns(kind, id)) as ColumnsByTable,
  query: async (kind: DbKind, id: string, sql: string, maxRows: number | null) =>
    (await Db.query(kind, id, sql, maxRows)) as QueryResult,
  queryReadOnly: async (kind: DbKind, id: string, sql: string, maxRows: number | null) =>
    (await Db.queryReadOnly(kind, id, sql, maxRows)) as QueryResult,
  tableDetail: async (kind: DbKind, id: string, table: string) =>
    (await Db.tableDetail(kind, id, table)) as TableDetail,
  commitEdits: (kind: DbKind, id: string, edits: CellEdit[], deletes: RowDelete[]) =>
    Db.commitEdits(kind, id, edits, deletes),
  importSql: (kind: DbKind, id: string, statements: string[]) => Db.importSql(kind, id, statements),
  createTableDdl: (kind: DbKind, id: string, table: string) => Db.createTableDdl(kind, id, table),
  exportDatabase: (kind: DbKind, id: string, tables: string[]) => Db.exportDatabase(kind, id, tables),
  redisGetKey: async (id: string, key: string) => (await Db.redisGetKey(id, key)) as [unknown, unknown],
  redisExec: (id: string, command: string) => Db.redisExec(id, command),
};

export const sql = {
  format: (s: string) => Sql.format(s),
  statements: (doc: string) => Sql.statements(doc),
  currentStatement: (doc: string, caret: number) => Sql.currentStatement(doc, caret),
  isQuery: (s: string) => Sql.isQuery(s),
  isReadOnly: (doc: string) => Sql.isReadOnly(doc),
  analyze: (doc: string) => Sql.analyze(doc) as RiskFinding[],
  analyzeRedis: (cmd: string) => Sql.analyzeRedis(cmd) as RiskFinding[],
  ident: (name: string, kind: DbKind) => Sql.ident(name, kind),
  literal: (value: string, kind: DbKind) => Sql.literal(value, kind),
  pageSql: (table: string, page: number, pageSize: number, kind: DbKind, sortCol?: string | null, desc?: boolean) =>
    Sql.pageSql(table, page, pageSize, kind, sortCol ?? null, desc ?? null),
  guessTableName: (s: string) => Sql.guessTableName(s),
};

export const prefsStore = {
  load: () => NativePrefs.load() as Prefs,
  save: (p: Prefs) => NativePrefs.save(p),
  setCoreLocale: (locale: string) => NativePrefs.setCoreLocale(locale),
  appVersion: () => NativePrefs.appVersion(),
};

export const ai = {
  status: () => Ai.status() as AiStatus,
  settings: () => Ai.settings() as AiSettings,
  saveSettings: (s: AiSettings) => Ai.saveSettings(s) as AiSettings,
  testConfig: (provider: ProviderConfig, model: string) => Ai.testConfig(provider, model),
  listModels: async (provider: ProviderConfig) => (await Ai.listModels(provider)) as string[],
  knowledge: () => Ai.knowledge() as KnowledgeDoc,
  chat: (messages: { role: string; content: string }[], onEvent: (e: ChatEvent) => void) =>
    Ai.chat(messages, (e) => onEvent(e as ChatEvent)),
};

/** 原生调用抛出的错误 → 给用户看的文案。 */
export function errorText(e: unknown): string {
  if (e instanceof Error) return e.message;
  return String(e);
}

// ==== shell ====
// 工作区外壳:保存对话框写文件、导出格式渲染。

import { Shell } from "datum/native";

export type ExportFormat = "csv" | "sql" | "markdown";

export const shell = {
  /** 系统保存对话框(默认下载目录)写入文本;返回写入的路径,取消为 null。 */
  saveText: (fileName: string, text: string) => Shell.saveText(fileName, text),
  /** 结果行 → 导出文本(CSV / Markdown / 每行一条 INSERT)。 */
  renderExport: (format: ExportFormat, table: string, columns: string[], rows: (string | null)[][], kind: DbKind) =>
    Shell.renderExport(format, table, columns, rows, kind),
  /** 文件名友好的时间戳 `YYYYMMDD_HHmmss`。 */
  fileTimestamp: () => Shell.fileTimestamp(),
};
