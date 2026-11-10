// 跨原生边界的数据形状:与 datum-core 的 serde(camelCase)输出一一对应。
// 原生模块返回的是 `unknown`,经 lib/native.ts 的封装断言成这里的类型。

export type DbKind = "pg" | "mysql" | "redis" | "sqlite" | "mongo";
export type Env = "local" | "staging" | "prod";
export type SslMode = "disable" | "prefer" | "require" | "verifyFull";

export const DB_KINDS: DbKind[] = ["pg", "mysql", "sqlite", "redis", "mongo"];
export const ENVS: Env[] = ["local", "staging", "prod"];
export const SSL_MODES: SslMode[] = ["disable", "prefer", "require", "verifyFull"];

export const KIND_LABEL: Record<DbKind, string> = {
  pg: "PostgreSQL",
  mysql: "MySQL",
  sqlite: "SQLite",
  redis: "Redis",
  mongo: "MongoDB",
};

export const KIND_SCHEME: Record<DbKind, string> = {
  pg: "postgresql",
  mysql: "mysql",
  sqlite: "sqlite",
  redis: "redis",
  mongo: "mongodb",
};

/** 关系库:支持 SQL、有列信息。 */
export const supportsSql = (k: DbKind) => k === "pg" || k === "mysql" || k === "sqlite";

export interface Connection {
  id: string;
  name: string;
  kind: DbKind;
  env: Env;
  host: string;
  port: number;
  user: string;
  database: string;
  plaintextPassword?: string | null;
  filePath?: string | null;
  sslMode: SslMode;
  uri?: string | null;
  authSource?: string | null;
}

export interface HistoryEntry {
  id: string;
  sql: string;
  connId: string;
  kind: string;
  connName: string;
  /** Unix 毫秒 */
  executedAt: number;
  elapsedMs: number;
  rowCount?: number | null;
  affected?: number | null;
}

export interface QueryResult {
  columns: string[];
  rows: (string | null)[][];
  affected?: number | null;
}

export interface ColumnInfo {
  name: string;
  dataType: string;
  default?: string | null;
  comment?: string | null;
  notNull: boolean;
  isPk: boolean;
}

export interface IndexInfo {
  name: string;
  columns: string;
  isPrimary: boolean;
  isUnique: boolean;
}

export interface TableDetail {
  columns: ColumnInfo[];
  indexes: IndexInfo[];
}

export interface ColumnBrief {
  name: string;
  dataType: string;
}

/** 表名 → 列 */
export type ColumnsByTable = Record<string, ColumnBrief[]>;

export interface CellEdit {
  table: string;
  pkCol: string;
  pkValue: string;
  column: string;
  newValue: string;
}

export interface RowDelete {
  table: string;
  pkCol: string;
  pkValue: string;
}

export interface RiskFinding {
  sql: string;
  level: "danger" | "warning";
  kind: string;
  object?: string | null;
}

export type ThemePreference = "light" | "dark" | "system";
export type LanguagePreference = "system" | "zhCn" | "zhTw" | "en";

export interface Prefs {
  theme: ThemePreference;
  language: LanguagePreference;
  fontFamily: string;
  fontSize: number;
  fontWeight: number;
  maxRows: number;
  aiInputHeight: number;
}

export type Protocol = "openai" | "anthropic" | string;

export interface ModelEntry {
  id: string;
  enabled: boolean;
}

export interface ProviderConfig {
  id: string;
  name: string;
  protocol: Protocol;
  baseUrl: string;
  apiKey: string;
  enabled: boolean;
  models: ModelEntry[];
}

export interface ModelRef {
  providerId: string;
  model: string;
}

export interface AiSettings {
  providers: ProviderConfig[];
  current?: ModelRef | null;
  knowledgeName: string;
  knowledgePath: string;
}

export interface AiStatus {
  ready: boolean;
  model?: string | null;
  error?: string | null;
}

export interface KnowledgeDoc {
  name: string;
  content: string;
  warning?: string | null;
}

export type ChatEvent =
  | { type: "delta"; text: string }
  | { type: "done"; full: string }
  | { type: "error"; message: string };

// ==== chat ====

/** AI 回复的一段:普通文本(Markdown)或 SQL 代码块。 */
export interface ReplySegment {
  kind: "text" | "sql";
  text: string;
}

/** 拼 system prompt 的上下文(`chat.systemPrompt`)。 */
export interface PromptContext {
  kind: DbKind;
  /** 对话绑定的连接标签(「连接名 / 库名」) */
  database: string;
  tables: string[];
  /** 已按需加载的表结构(当前浏览的表 + 用户消息里命中的表) */
  details?: Record<string, TableDetail>;
  /** 全库表→列速览 */
  columns?: ColumnsByTable | null;
}

/** 服务商预设(设置页新建服务商时预填)。 */
export interface ProviderPreset {
  name: string;
  /** 按当前语言的展示名 */
  displayName: string;
  protocol: Protocol;
  baseUrl: string;
}

// ==== dialogs ====

/** Mongo 连接串拆分结果(core `mongo::uri::split_uri`)。 */
export interface MongoUriParts {
  /** 不含密码与库名的连接串 */
  sanitized: string;
  user: string;
  /** null:串里没写密码 */
  password: string | null;
  hosts: string;
  database: string;
}

export interface AiProtocolOption {
  id: Protocol;
  label: string;
}
