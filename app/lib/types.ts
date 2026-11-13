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
  /** MySQL:改表结构的语句,执行前评估锁表与重建风险 */
  schemaChange?: boolean;
  /** MySQL:只因改表结构才列出(本身不算破坏性);非 prod 连接评估为高风险时才需要确认 */
  assessOnly?: boolean;
}

export type AlterAlgorithm = "instant" | "inplace" | "copy";
export type AlterRisk = "low" | "medium" | "high" | "unknown";

/** 改表结构的一个子操作的判定(按服务器默认算法);null 为未知。 */
export interface AlterOpVerdict {
  clause: string;
  /** 操作代码:add_column、modify_column… */
  op: string;
  target: string | null;
  algorithm: AlterAlgorithm | null;
  rebuild: boolean | null;
  concurrentDml: boolean | null;
  /** 依据说明代码 */
  note: string | null;
}

/** MySQL / MariaDB 改表结构语句的风险评估(规则表 + 表元数据,不执行 ALTER)。 */
export interface AlterAssessment {
  statement: string;
  table: string;
  /** 「MySQL 8.4.9」 */
  server: string;
  ops: AlterOpVerdict[];
  algorithm: AlterAlgorithm | null;
  rebuild: boolean | null;
  blocksWrites: boolean | null;
  /** 语句里显式写的 ALGORITHM / LOCK */
  requested: string | null;
  /** 显式子句满足不了:服务器会直接报错、不执行(原因代码) */
  fails: string | null;
  risk: AlterRisk;
  rows: number | null;
  bytes: number | null;
  /** 耗时量级:metadata | seconds | minutes | hours | days */
  duration: string | null;
  /** 加上保护子句的写法 */
  guarded: string | null;
  /** 建议改用 gh-ost / pt-online-schema-change */
  suggestOsc: boolean;
  /** 表级说明代码 */
  notes: string[];
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
  /** 有元素不是合法 UTF-8(显示的是替换后的文本):按它去改会对不上原值,编辑器只读 */
  binary?: boolean;
  /** 非 UTF-8 单元格的原始字节(base64),查看时识别格式用 */
  raw?: RedisRawCell[];
}

/** 一个单元格的原始字节:`row` / `col` 为在 rows 里的位置(Stream 条目里 row 为字段下标,col 0 字段名、1 值)。 */
export interface RedisRawCell {
  row: number;
  col: number;
  b64: string;
}

/** Redis 值的查看格式;gzip / zlib 解压后再自动识别内层。 */
export type RedisViewFormat = "auto" | "text" | "hex" | "json" | "msgpack" | "gzip" | "zlib" | "java" | "protobuf";

/** 解码结果(core 的 `rds::decode::Decoded`)。 */
export interface RedisDecoded {
  /** 最终展示的格式:text / hex / json / msgpack / java / protobuf */
  format: RedisViewFormat;
  /** 依次解开的压缩层 */
  layers: RedisViewFormat[];
  /** 文本视图:原文、格式化的 JSON 或十六进制转储 */
  text: string;
  /** 树视图的 JSON 文本;不是树形格式为 null */
  tree: string | null;
  /** 手动选的格式解不出来,回退到了文本 / 十六进制 */
  fallback: boolean;
  notes: ("hex_truncated" | "inflate_limit" | "java_partial" | "too_large")[];
  /** 展示内容的字节数(解压后) */
  size: number;
}

/** Redis key 的一次元素级编辑(core 的 `KeyEdit`);分数用文本,允许 inf / -inf。 */
export type RedisEdit =
  | { op: "stringSet"; value: string }
  | { op: "hashAdd"; field: string; value: string }
  | { op: "hashSet"; field: string; value: string; oldField: string | null }
  | { op: "hashDel"; field: string }
  | { op: "listSet"; index: number; old: string; value: string }
  | { op: "listPush"; value: string; head: boolean }
  | { op: "listDel"; index: number; old: string }
  | { op: "setAdd"; member: string }
  | { op: "setDel"; member: string }
  | { op: "zsetAdd"; member: string; score: string }
  | { op: "zsetSet"; member: string; score: string; oldMember: string | null }
  | { op: "zsetDel"; member: string }
  | { op: "streamAdd"; id: string; fields: [string, string][] }
  | { op: "streamDel"; id: string };

/** 编辑结果:实际改动的元素数(删一个已不在的元素为 0);追加 Stream 条目时带回 ID。 */
export interface RedisEditResult {
  changed: number;
  id: string | null;
}

/** 新建 key。`items` 每项按类型:string `[值]`、hash / stream `[字段, 值]`、list / set `[元素]`、zset `[成员, 分数]`。 */
export interface RedisNewKey {
  key: string;
  keyType: string;
  items: string[][];
  /** 秒;null 不过期 */
  ttl: number | null;
}

export interface RedisStreamEntry {
  id: string;
  fields: [string, string][];
  raw?: RedisRawCell[];
}

/** Stream 的一页;`next` 是下一页的起点(含),正序作 start、倒序作 end;没有更多为 null。 */
export interface RedisStreamPage {
  entries: RedisStreamEntry[];
  next: string | null;
  length: number;
}

export interface RedisStreamConsumer {
  name: string;
  pending: number;
  idleMs: number;
  inactiveMs: number | null;
}

export interface RedisStreamGroup {
  name: string;
  consumers: number;
  pending: number;
  lastDeliveredId: string;
  entriesRead: number | null;
  lag: number | null;
  pendingMin: string | null;
  pendingMax: string | null;
  consumerList: RedisStreamConsumer[];
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
  /** JSON 列里的字段路径(在 JSON 树上点字段生成);没有表示比较整列 */
  path?: JsonPath;
  /** 被点字段的类型:等于 / 大小比较按数值、布尔还是文本 */
  jsonType?: JsonKind | null;
}

// ---- JSON 单元格(datum_core::json)

/** JSON 路径:对象键或数组下标。 */
export type JsonPath = (string | number)[];

export type JsonKind = "object" | "array" | "string" | "number" | "bool" | "null";

/** 无损解析出的 JSON 值:对象保留原文键顺序,数字保留原文写法。 */
export type JsonNode =
  | { kind: "object"; entries: { key: string; node: JsonNode }[] }
  | { kind: "array"; items: JsonNode[] }
  | { kind: "string" | "number" | "bool" | "null"; text: string };

/** JSON 解析失败的位置与原因(kind 对应 `json.err.*` 文案)。 */
export interface JsonError {
  kind: "eof" | "unexpected" | "trailing" | "escape" | "number" | "control" | "too_deep";
  line: number;
  column: number;
  found: string | null;
}

export type JsonParse = { ok: true; node: JsonNode } | { ok: false; error: JsonError };

/** 取样推断的 JSON 键结构(补全键名用):每层的键按出现次数降序,数组元素的结构在 items。 */
export interface JsonKeyTree {
  keys: { name: string; count: number; tree: JsonKeyTree }[];
  items?: JsonKeyTree;
}

/** 光标处于 JSON 列的键名位置(偏移为 UTF-16)。 */
export interface JsonKeyContext {
  table: string;
  column: string;
  dataType: string;
  path: JsonPath;
  start: number;
  end: number;
  prefix: string;
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
  /** 整个分区的修复脚本(MySQL 字符集体检:转 utf8mb4),放进编辑器、不直接执行 */
  script: string | null;
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

/** SQLite 连接上会被脚本改动的设置(多语句执行出错后按它恢复)。 */
export interface SqliteSessionFlags {
  legacyAlterTable: boolean;
  foreignKeys: boolean;
}

/** SQLite 改表结构:一列。`orig` 是原表里的列名(新加的列为 null),改名 / 删列 / 调顺序靠它对应。 */
export interface AlterColumn {
  orig: string | null;
  name: string;
  dataType: string;
  notNull: boolean;
  /** 默认值表达式,原样写进 DEFAULT(字符串自带引号) */
  default: string | null;
  pk: boolean;
  /** 生成列(只读,不复制数据) */
  generated: boolean;
}

/** SQLite 改表结构:外键;列名用改名后的新名字,refColumns 为空表示引用主键。 */
export interface AlterFk {
  columns: string[];
  refTable: string;
  refColumns: string[];
  onDelete: string;
  onUpdate: string;
}

/** SQLite 改表结构:目标结构(列的顺序即新表列顺序;原表有而这里没有的列会被删掉)。 */
export interface AlterPlan {
  table: string;
  newName: string;
  columns: AlterColumn[];
  fks: AlterFk[];
}

/** SQLite 表的现状。 */
export interface AlterSnapshot {
  table: string;
  createSql: string;
  columns: AlterColumn[];
  fks: AlterFk[];
  foreignKeysOn: boolean;
}

/** 生成的脚本:noop 无变化;simple 用 ALTER TABLE;rebuild 重建表。`statements` 是事务里依次执行的语句。 */
export interface AlterScript {
  mode: "noop" | "simple" | "rebuild";
  sql: string;
  statements: string[];
  fkCheck: string[];
  foreignKeysOff: boolean;
  warnings: string[];
}

/** 外键检查不过的一行。 */
export interface FkViolation {
  table: string;
  rowid: number | null;
  parent: string;
  fkid: number;
}

/** 执行结果:applied 为 false 时已整体回滚(violations 列出违规行)。 */
export interface AlterOutcome {
  applied: boolean;
  violations: FkViolation[];
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
