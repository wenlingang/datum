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

// ==== tab ====

import { Grid } from "datum/native";
import type * as TabTypes from "./types";

type GridCell = string | null;

/** 结果网格与编辑器的纯计算(datum-core 的 grid / completion / sql::build)。 */
export const grid = {
  classifyCell: (value: GridCell, dataType: string | null) =>
    Grid.classifyCell(value, dataType) as TabTypes.CellStyle,
  colAlign: (dataType: string | null) => Grid.colAlign(dataType) as TabTypes.ColAlign,
  columnWidths: (autos: number[], containerW: number, defaultW: number, flexible: boolean[]) =>
    Grid.columnWidths(autos, containerW, defaultW, flexible),
  sortOrder: (rows: GridCell[][], colIx: number, descending: boolean) => Grid.sortOrder(rows, colIx, descending),
  filterIndices: (rows: GridCell[][], columns: string[], filters: TabTypes.FilterRow[], conj: TabTypes.Conj) =>
    Grid.filterIndices(rows, columns, filters, conj),
  filtersSignature: (filters: TabTypes.FilterRow[], conj: TabTypes.Conj) => Grid.filtersSignature(filters, conj),
  toCsv: (columns: string[], rows: GridCell[][]) => Grid.toCsv(columns, rows),
  toMarkdown: (columns: string[], rows: GridCell[][]) => Grid.toMarkdown(columns, rows),
  buildInsertRow: (table: string, columns: string[], values: GridCell[], kind: DbKind) =>
    Grid.buildInsertRow(table, columns, values, kind),
  buildInsert: (table: string, columns: string[], kind: DbKind) => Grid.buildInsert(table, columns, kind),
  completions: (
    kind: DbKind,
    text: string,
    caret: number,
    tables: string[],
    columns: ColumnsByTable | null,
    explicit: boolean,
  ) => Grid.completions(kind, text, caret, tables, columns, explicit) as TabTypes.CompletionResult | null,
  uppercaseKeywordBefore: (text: string, caret: number) =>
    Grid.uppercaseKeywordBefore(text, caret) as { start: number; end: number; text: string } | null,
};

/** Redis:读取一个 key 的值(表格形态)与详情。 */
export async function redisKey(id: string, key: string) {
  return (await Db.redisGetKey(id, key)) as [TabTypes.RedisKeyValue, TabTypes.KeyDetail];
}
