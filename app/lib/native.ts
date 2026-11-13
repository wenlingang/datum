// datum/native 的类型化封装:原生导出返回 `unknown`(JSON),这里断言成 lib/types 的形状。
// 界面代码只从这里调用原生模块,不直接 import "datum/native"。

import { Ai, Db, Prefs as NativePrefs, Sql, Store } from "datum/native";
import type {
  AgentEvent,
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
  WriteImpact,
  UndoPrep,
  RedisOverview,
  RedisReport,
  RedisSlowEntry,
  DiagAction,
  DiagResult,
  DiagSection,
  NlFilter,
  ForeignKey,
  ValueContext,
  Plan,
  DiffReport,
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
  /** 上次退出时的会话(标签与内容);没有为 null。 */
  loadSession: () => Store.loadSession() as unknown,
  saveSession: (session: unknown) => Store.saveSession(session),
  /** 保存过的 AI 对话;没有为 null。 */
  loadAiConversations: () => Store.loadAiConversations() as unknown,
  saveAiConversations: (data: unknown) => Store.saveAiConversations(data),
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
  /** 执行写语句前的撤销准备(抓快照、生成反向语句) */
  undoPrepare: async (kind: DbKind, id: string, sql: string) => (await Db.undoPrepare(kind, id, sql)) as UndoPrep,
  /** 写语句的影响范围预估;改写不了的语句为 null */
  writeImpact: async (kind: DbKind, id: string, sql: string) =>
    (await Db.writeImpact(kind, id, sql)) as WriteImpact | null,
  tableDetail: async (kind: DbKind, id: string, table: string) =>
    (await Db.tableDetail(kind, id, table)) as TableDetail,
  commitEdits: (kind: DbKind, id: string, edits: CellEdit[], deletes: RowDelete[]) =>
    Db.commitEdits(kind, id, edits, deletes),
  importSql: (kind: DbKind, id: string, statements: string[]) => Db.importSql(kind, id, statements),
  createTableDdl: (kind: DbKind, id: string, table: string) => Db.createTableDdl(kind, id, table),
  exportDatabase: (kind: DbKind, id: string, tables: string[]) => Db.exportDatabase(kind, id, tables),
  redisGetKey: async (id: string, key: string) => (await Db.redisGetKey(id, key)) as [unknown, unknown],
  redisExec: (id: string, command: string) => Db.redisExec(id, command),
  redisOverview: async (id: string) => (await Db.redisOverview(id)) as RedisOverview,
  redisSlowlog: async (id: string) => (await Db.redisSlowlog(id)) as RedisSlowEntry[],
  redisAnalysisStart: (id: string) => Db.redisAnalysisStart(id) as Promise<void>,
  redisAnalysisReport: async (id: string) => (await Db.redisAnalysisReport(id)) as RedisReport | null,
  redisAnalysisCancel: (id: string) => Db.redisAnalysisCancel(id) as Promise<void>,
  /** 表结构对比:脚本让目标库与源库一致(删除类语句已注释)。 */
  schemaDiff: async (kind: DbKind, sourceId: string, targetId: string) =>
    (await Db.schemaDiff(kind, sourceId, targetId)) as DiffReport,
  /** 执行计划(树 + 原始输出);`analyze` 会真正执行,只接受只读语句。 */
  explainPlan: async (kind: DbKind, id: string, sql: string, analyze: boolean) =>
    (await Db.explainPlan(kind, id, sql, analyze)) as Plan,
  /** 补全用的外键(物理 + 按命名推断)。 */
  foreignKeys: async (kind: DbKind, id: string) => (await Db.foreignKeys(kind, id)) as ForeignKey[],
  /** 低基数列的实际取值(最多 30 个);不是枚举类的列为 null。 */
  distinctValues: async (kind: DbKind, id: string, table: string, column: string, dataType: string) =>
    (await Db.distinctValues(kind, id, table, column, dataType)) as string[] | null,
  diagSections: (kind: DbKind) => Db.diagSections(kind) as DiagSection[],
  diagRun: async (kind: DbKind, id: string, section: string) => (await Db.diagRun(kind, id, section)) as DiagResult,
  diagAct: (kind: DbKind, id: string, action: DiagAction, args: string[]) =>
    Db.diagAct(kind, id, action, args) as Promise<void>,
};

export const sql = {
  format: (s: string) => Sql.format(s),
  statements: (doc: string) => Sql.statements(doc),
  currentStatement: (doc: string, caret: number) => Sql.currentStatement(doc, caret),
  isQuery: (s: string) => Sql.isQuery(s),
  isReadOnly: (doc: string) => Sql.isReadOnly(doc),
  /** `strict`(prod 连接):带 WHERE 的 UPDATE / DELETE 也列出,执行前确认并预估影响范围 */
  analyze: (doc: string, strict = false) => Sql.analyze(doc, strict) as RiskFinding[],
  analyzeRedis: (cmd: string) => Sql.analyzeRedis(cmd) as RiskFinding[],
  ident: (name: string, kind: DbKind) => Sql.ident(name, kind),
  literal: (value: string, kind: DbKind) => Sql.literal(value, kind),
  pageSql: (table: string, page: number, pageSize: number, kind: DbKind, sortCol?: string | null, desc?: boolean) =>
    Sql.pageSql(table, page, pageSize, kind, sortCol ?? null, desc ?? null),
  guessTableName: (s: string) => Sql.guessTableName(s),
  /** SQL 里的 `:name` 参数(去重,按出现顺序)。 */
  findParams: (s: string) => Sql.findParams(s) as string[],
  /** 代入参数值:数字原样、NULL 为空值,其余写成转义后的字符串。 */
  bindParams: (s: string, values: Record<string, string>, kind: DbKind) => Sql.bindParams(s, values, kind) as string,
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
  /** 停止当前对话(回复以已收到的内容结束)。 */
  stopChat: () => Ai.stopChat(),
  /** 表浏览的自然语言筛选:一句话 → 且/或筛选条件。 */
  nlFilter: async (question: string, columns: { name: string; dataType: string | null }[], samples: (string | null)[][]) =>
    (await Ai.nlFilter(question, columns, samples)) as NlFilter,
  /** 带工具的对话:AI 可在连接 `id` 上列表、看表结构、跑只读查询。 */
  agentChat: (kind: DbKind, id: string, messages: { role: string; content: string }[], onEvent: (e: AgentEvent) => void) =>
    Ai.agentChat(kind, id, messages, (e) => onEvent(e as AgentEvent)),
};

/** 原生调用抛出的错误 → 给用户看的文案。 */
export function errorText(e: unknown): string {
  if (e instanceof Error) return e.message;
  return String(e);
}

// ==== chat ====

export const chat = {
  /** 拼 AI 对话的 system prompt(知识库在原生侧读取,语言跟随界面)。 */
  systemPrompt: (ctx: PromptContext) => Ai.systemPrompt(ctx),
  /** 用户消息里提到的已存在表名,最多 10 张。 */
  mentionedTables: (text: string, tables: string[]) => Ai.mentionedTables(text, tables),
  /** 把 assistant 回复切成文本段与 SQL 段(流式中未闭合的代码块同样识别)。 */
  segments: (content: string) => Ai.segments(content) as ReplySegment[],
  /** 常用服务商预设。 */
  presets: () => Ai.presets() as ProviderPreset[],
};

import type { PromptContext, ProviderPreset, ReplySegment } from "./types";

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

// ==== dialogs ====

import { Dialogs as NativeDialogs } from "datum/native";
import type { AiProtocolOption } from "./types";

export const dialogsNative = {
  /** 系统打开文件框,返回绝对路径;取消为 null。 */
  pickFile: (title: string, filterName: string | null, extensions: string[]) =>
    NativeDialogs.pickFile(title, filterName, extensions),
  aiProtocols: () => NativeDialogs.aiProtocols() as AiProtocolOption[],
};

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
    fks: ForeignKey[] | null = null,
  ) => Grid.completions(kind, text, caret, tables, columns, explicit, fks) as TabTypes.CompletionResult | null,
  /** 表浏览带服务端筛选的分页 SQL(没有有效条件时与普通分页相同)。 */
  filteredPageSql: (
    table: string,
    page: number,
    pageSize: number,
    kind: DbKind,
    sortCol: string | null,
    desc: boolean | null,
    filters: TabTypes.FilterRow[],
    conj: TabTypes.Conj,
    types: Record<string, string>,
  ) => Grid.filteredPageSql(table, page, pageSize, kind, sortCol, desc, filters, conj, types) as string,
  /** 光标是否在某列的取值位置(`col = '…`、`col IN (…`)。 */
  valueContext: (text: string, caret: number, columns: ColumnsByTable | null) =>
    Grid.valueContext(text, caret, columns) as ValueContext | null,
  uppercaseKeywordBefore: (text: string, caret: number) =>
    Grid.uppercaseKeywordBefore(text, caret) as { start: number; end: number; text: string } | null,
};

/** Redis:读取一个 key 的值(表格形态)与详情。 */
export async function redisKey(id: string, key: string) {
  return (await Db.redisGetKey(id, key)) as [TabTypes.RedisKeyValue, TabTypes.KeyDetail];
}

/** 用表单当前参数(不保存)列出实例下的库(新建/编辑连接时下拉选库)。 */
export const probeDatabases = async (conn: import("./types").Connection, password: string) =>
  (await NativeDialogs.probeDatabases(conn, password)) as string[];
