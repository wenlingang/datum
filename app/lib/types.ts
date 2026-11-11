// 跨原生边界的数据形状:与 datum-core 的 serde(camelCase)输出一一对应。
// 原生模块返回的是 `unknown`,经 lib/native.ts 的封装断言成这里的类型。

export type DbKind = "pg" | "mysql" | "redis" | "sqlite" | "mongo";
export type Env = "local" | "staging" | "prod";
export type SslMode = "disable" | "prefer" | "require" | "verifyFull";

/** nsh 版支持的连接类型(不含 MongoDB:已存的 Mongo 连接保留在配置里,但界面不显示)。 */
export const DB_KINDS: DbKind[] = ["pg", "mysql", "sqlite", "redis"];
export const isSupportedKind = (k: DbKind) => DB_KINDS.includes(k);
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
  /** 经 SSH 跳板机连接;缺省 / null 为直连 */
  ssh?: SshTunnel | null;
}

export type SshAuth = "password" | "key" | "agent";
export const SSH_AUTHS: SshAuth[] = ["password", "key", "agent"];

export interface SshTunnel {
  host: string;
  port: number;
  user: string;
  auth: SshAuth;
  /** 私钥路径;空则用 ~/.ssh 下的默认私钥 */
  keyPath: string;
  /** SSH 密码 / 私钥密码短语:保存时 null 为沿用原值;从 core 读回来的永远是 null */
  secret?: string | null;
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

// ---------- ER 图(datum-core erd / schema)

/** 一条外键列配对(复合外键按约束展开为多行)。 */
export interface ForeignKey {
  constraint: string;
  table: string;
  column: string;
  refTable: string;
  /** sqlite 引用隐式主键时为空串 */
  refColumn: string;
  /** 按 xxx_id 命名推断,库里没有物理约束 */
  inferred: boolean;
}

export interface ErdSchema {
  fks: ForeignKey[];
  pkByTable: Record<string, string[]>;
}

export interface ErdColumn {
  name: string;
  dataType: string;
  isPk: boolean;
  isFk: boolean;
}

export interface ErdNode {
  table: string;
  /** 可见列(折叠时只含键列) */
  columns: ErdColumn[];
  collapsed: boolean;
  /** 未显示的列数(尾行「+N 个字段」) */
  hiddenCount: number;
  isFocus: boolean;
  /** 估算的绘制尺寸(点) */
  w: number;
  h: number;
}

export interface ErdEdge {
  id: string;
  /** 引用方表 */
  source: string;
  /** 被引用表 */
  target: string;
  /** "col → refCol",复合外键多行 */
  label: string;
  inferred: boolean;
}

export interface ErdGraph {
  nodes: ErdNode[];
  edges: ErdEdge[];
  /** 参与构图的外键(含推断),搜索按约束名匹配用 */
  fks: ForeignKey[];
}

export interface ErdBuildOptions {
  focus?: string | null;
  collapsed?: Record<string, boolean>;
  full?: Record<string, boolean>;
  inferred?: boolean;
}

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

/** 写语句的影响范围预估(改写成只读查询得出):条件匹配的行数与前几行。 */
export interface WriteImpact {
  count: number;
  /** 整表操作(TRUNCATE / DROP TABLE):数的是表里现有的行 */
  wholeTable: boolean;
  columns: string[];
  rows: (string | null)[][];
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

export interface AiProtocolOption {
  id: Protocol;
  label: string;
}

// ==== tab ====

/** Redis key 的值:统一成 列 + 行 的表格形态。 */
export interface RedisKeyValue {
  key: string;
  keyType: string;
  columns: string[];
  rows: string[][];
}

/** Redis key 详情。`ttl`:-1 永不过期,-2 不存在。 */
export interface KeyDetail {
  key: string;
  keyType: string;
  ttl: number;
  size: number;
}

/** 单元格的灰阶档位(Datum 灰阶三档 + 单一语义色)。 */
export type CellTier = "strong" | "normal" | "dim" | "hash" | "null" | "negative";

export interface CellStyle {
  tier: CellTier;
  mono: boolean;
}

export type ColAlign = "left" | "right" | "center";

export type FilterOp =
  | "contains"
  | "eq"
  | "ne"
  | "starts"
  | "ends"
  | "gt"
  | "lt"
  | "gte"
  | "lte"
  | "empty"
  | "notEmpty";

export const FILTER_OPS: FilterOp[] = [
  "contains",
  "eq",
  "ne",
  "starts",
  "ends",
  "gt",
  "lt",
  "gte",
  "lte",
  "empty",
  "notEmpty",
];

/** 条件之间的连接词(全局统一)。 */
export type Conj = "and" | "or";

/** 一条筛选条件。`col` 为空串表示尚未选择列。 */
export interface FilterRow {
  id: number;
  col: string;
  op: FilterOp;
  value: string;
}

/** 编辑器补全的一项。 */
export interface CompletionItem {
  label: string;
  detail: string;
  /** LSP 的 CompletionItemKind 名(keyword / class / field / snippet / function / constant) */
  kind: string;
  insert: string;
}

/** 编辑器补全:替换范围为 UTF-16 偏移。 */
export interface CompletionResult {
  start: number;
  end: number;
  items: CompletionItem[];
}
