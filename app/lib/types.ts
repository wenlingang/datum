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
  /** 先经过的跳板(ProxyJump 写法);空则按 ~/.ssh/config */
  proxyJump?: string;
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

/** 带工具的对话(AI 自己查数据)的事件:在 ChatEvent 之外多了工具开始 / 结束。 */
export type AgentEvent =
  | ChatEvent
  | { type: "toolStart"; id: string; name: string; args: Record<string, unknown> }
  | { type: "toolEnd"; id: string; ok: boolean; summary: string; detail: ToolDetail };

/** 工具结果的展示数据:查询给 sql / columns / rows,表结构给 table / columns,出错给 error。 */
export interface ToolDetail {
  sql?: string;
  columns?: unknown[];
  rows?: (string | null)[][];
  capped?: boolean;
  table?: string;
  tables?: string[];
  error?: string;
}

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
  /** 元素太多只取了前 1000 个(字符串前 1 MB) */
  truncated?: boolean;
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

// ---------------------------------------------------------------- Redis 分析

/** INFO 里分析页关心的几项(原样字符串)。 */
export interface RedisOverview {
  version: string | null;
  usedMemory: string | null;
  usedMemoryPeak: string | null;
  maxMemory: string | null;
  maxMemoryPolicy: string | null;
  fragmentationRatio: string | null;
  connectedClients: string | null;
  /** 命中率(%) */
  hitRate: number | null;
  evictedKeys: string | null;
  expiredKeys: string | null;
  keys: number | null;
  expires: number | null;
}

export interface RedisSlowEntry {
  id: number;
  /** Unix 秒 */
  time: number;
  micros: number;
  command: string;
  client: string;
}

export interface RedisKeyStat {
  key: string;
  type: string;
  bytes: number | null;
  /** 剩余毫秒;-1 永不过期 */
  ttlMs: number;
  freq: number | null;
  /** 集群模式:所在主节点 host:port */
  node?: string;
}

export interface RedisNodeStat {
  addr: string;
  keys: number;
  error: string | null;
}

export interface RedisGroupStat {
  name: string;
  keys: number;
  bytes: number;
}

export interface RedisReport {
  scanned: number;
  total: number;
  done: boolean;
  cancelled: boolean;
  /** 达到上限提前停止:结果是抽样 */
  truncated: boolean;
  error: string | null;
  memorySupported: boolean;
  lfu: boolean;
  totalBytes: number;
  biggest: RedisKeyStat[];
  hottest: RedisKeyStat[];
  /** 第一级前缀(name 不含 `:*`) */
  prefixes: RedisGroupStat[];
  /** 前缀 → 下一级子前缀(name 为完整前缀);最后一段是 `…` 的是放不下的「其余」 */
  prefixChildren: Record<string, RedisGroupStat[]>;
  cluster: boolean;
  nodes: RedisNodeStat[];
  types: RedisGroupStat[];
  ttl: { persistent: number; withinHour: number; withinDay: number; withinWeek: number; longer: number };
}

/** 写语句的撤销准备:反向语句(空则不能撤销)、会恢复的行数、不能撤销的原因。 */
export interface UndoPrep {
  statements: string[];
  rows: number;
  reason: string | null;
  /** 前端合成的整体撤销:由哪几条语句各自的撤销组成(撤销整体后,它们在历史里也不能再撤销) */
  parts?: UndoPrep[];
}

// ---------------------------------------------------------------- 关系库诊断

/** 诊断里能触发的写操作。 */
export type DiagAction =
  | "cancel"
  | "terminate"
  | "kill_query"
  | "kill"
  | "vacuum_table"
  | "optimize_table"
  | "vacuum"
  | "analyze";

/** 诊断分区说明(PG / MySQL / SQLite 各自一组)。 */
export interface DiagSection {
  key: string;
  /** cards:只有一行,按列显示成卡片 */
  layout: "table" | "cards";
  /** 每行可做的操作;参数取自 targetColumns 指定的列 */
  rowActions: DiagAction[];
  targetColumns: string[];
  /** 整库操作(分区标题旁的按钮) */
  dbActions: DiagAction[];
  /** 开销大,点「运行」才查 */
  manual: boolean;
}

/** 一个分区的结果;依赖没开(扩展 / 开关 / 权限)时 unavailable 说明原因。 */
export interface DiagResult {
  columns: string[];
  rows: (string | null)[][];
  unavailable: { reason: string; detail: string | null } | null;
}

/** 自然语言筛选的转换结果(条件已在原生侧校验过列名与操作符)。 */
export interface NlFilter {
  conj: Conj;
  filters: { col: string; op: FilterOp; value: string }[];
  note: string | null;
  /** 被丢弃的条件(列不存在、操作符不支持等) */
  dropped: string[];
}

/** 外键列配对(物理外键或按命名推断的),补全 JOIN 的 ON 条件用。 */
export interface ForeignKey {
  constraint: string;
  table: string;
  column: string;
  refTable: string;
  refColumn: string;
  inferred: boolean;
}

/** 光标处于某列的取值位置(偏移为 UTF-16)。 */
export interface ValueContext {
  table: string;
  column: string;
  dataType: string;
  start: number;
  end: number;
  prefix: string;
  quoted: boolean;
}

/** 执行计划树的一个节点(三种库统一的形状)。 */
export interface PlanNode {
  label: string;
  detail: string;
  cost: number | null;
  selfCost: number | null;
  rows: number | null;
  actualMs: number | null;
  actualRows: number | null;
  fullScan: boolean;
  children: PlanNode[];
}

/** 一次 Explain 的结果:计划树(解析不了为 null)与原始输出。 */
export interface Plan {
  tree: PlanNode | null;
  raw: QueryResult;
  analyzed: boolean;
  totalMs: number | null;
}

/** 表结构对比的一张表。 */
export interface TableDiff {
  kind: "table" | "view" | "trigger";
  name: string;
  status: "added" | "removed" | "changed";
  changes: string[];
}

/** 表结构对比报告:差异与迁移脚本(在目标库上执行)。 */
export interface DiffReport {
  tables: TableDiff[];
  same: number;
  sql: string;
}

/** SQLite 导入 CSV / JSON 的列类型。 */
export type ImportColType = "INTEGER" | "REAL" | "TEXT";
export type ImportDelimiter = "auto" | "comma" | "tab" | "semicolon";
export type ImportQuote = "double" | "single" | "none";
export type ImportEncoding = "utf8" | "gbk";

/** 读哪个文件、怎么解析(CSV 选项对 JSON 无效)。 */
export interface ImportSource {
  path: string;
  format: "csv" | "json";
  delimiter: ImportDelimiter;
  quote: ImportQuote;
  encoding: ImportEncoding;
  hasHeader: boolean;
}

/** 预览:列(来源键、建议列名、推断类型)与前若干行。 */
export interface ImportPreview {
  format: "csv" | "json";
  /** CSV 实际用的分隔符(自动时为探测结果) */
  delimiter: Exclude<ImportDelimiter, "auto"> | null;
  jsonLines: boolean;
  columns: { key: string; name: string; type: ImportColType }[];
  rows: (string | null)[][];
  sampled: number;
  tableName: string;
  /** 解码遇到非法字节:多半选错了编码 */
  badEncoding: boolean;
  fileSize: number;
}

/** 确认后的一列;index / key 原样取自预览。 */
export interface ImportColumnSpec {
  index: number;
  key: string;
  name: string;
  type: ImportColType;
  include: boolean;
}

export interface ImportPlan {
  source: ImportSource;
  table: string;
  mode: "create" | "append";
  columns: ImportColumnSpec[];
}

export interface ImportProgress {
  rows: number;
  bytes: number;
  totalBytes: number;
  done: boolean;
  cancelled: boolean;
  error: string | null;
  /** 追加时表里没有、被跳过的列 */
  skipped: string[];
}
