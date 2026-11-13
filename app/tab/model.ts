// 工作标签页的模型:绑定一个连接,持有各自的 SQL、结果、分页、排序、筛选与暂存编辑。
// 对应 GPUI 版 crates/app/src/tab/mod.rs 的 WorkTab。
//
// 两种形态:
// - 表浏览(kind "table"):纯网格,服务端分页与排序,可暂存编辑/删除后 ⌘S 提交;Redis 为 key 的值;
// - 查询(kind "query"):编辑器 + 工具条 + 结果,运行光标所在语句或选区。
//
// 状态都是这个模型自己的 signal(一个 tab 一份),挂在 WorkTab.state 上;界面只读这些 signal、
// 调这里的动作函数。异步执行用递增的 token 丢弃过期结果(相当于 GPUI 版替换 `_task`)。

import { batch, signal, type Signal } from "nsh";
import type { EditorRef, EditorSelection } from "nsh/base";
import { t } from "nsh/i18n";
import { askAiExplainPlan, askAiFixError, planText, sendToAi } from "../chat/index";
import { ai, db, errorText, grid, redisKey, sql as sqlText } from "../lib/native";
import {
  type AlterAssessment,
  type CellEdit,
  type Conj,
  type DbKind,
  type FilterRow,
  type JsonKind,
  type JsonPath,
  type KeyDetail,
  type Plan,
  type RedisKeyValue,
  type QueryResult,
  type RiskFinding,
  type RowDelete,
  type TableDetail,
  type UndoPrep,
  type WriteImpact,
  supportsSql,
} from "../lib/types";
import { conns, refreshTables } from "../store/conns";
import { forgetUndo, recordHistory } from "../store/history";
import { maxRows } from "../store/prefs";
import { toast } from "../store/toast";
import { setDetailOpen, type WorkTab } from "../store/workspace";
import { caretOf, primaryRange } from "./textpos";

/** 表浏览每页行数。 */
export const PAGE_SIZE = 100;

/** 排序:列名 + 是否倒序。 */
export interface SortState {
  col: string;
  desc: boolean;
}

/** 破坏性语句审查弹窗的内容。 */
export interface RiskGate {
  findings: RiskFinding[];
  /** 目标连接展示名 */
  targetLabel: string;
  /** 当前数据库(SQLite 为文件路径) */
  database: string | null;
  /** 关系库(SQL 高亮);Redis 为 false */
  relational: boolean;
  /** 每条命中的影响范围预估,与 findings 一一对应(Redis 不预估,为空) */
  impacts: Signal<ImpactState[]>;
  /** 改表结构语句的风险评估,与 findings 一一对应(不是改表结构的为 none;非 MySQL 为空) */
  alters: Signal<AlterState[]>;
  /** 不显示影响范围预估(改表结构脚本里的 DROP TABLE 删的是已复制过数据的旧表,数行数会误导) */
  noImpact?: boolean;
  /** 目标说明下方的补充说明 */
  note?: string;
  /** 「确认执行」 */
  onConfirm: () => void;
}

/** 一条写语句的影响范围预估状态。 */
export type ImpactState =
  | { state: "loading" }
  | { state: "ok"; impact: WriteImpact }
  /** 改写不了(多表、CTE 等),不预估 */
  | { state: "none" }
  | { state: "error"; error: string };

/** 改表结构语句的风险评估状态。 */
export type AlterState =
  | { state: "loading" }
  | { state: "ok"; assessment: AlterAssessment }
  | { state: "none" }
  | { state: "error"; error: string };

/** 依次预估各条命中的影响范围(逐条跑,不挤占同一连接)。弹窗关掉后结果写进已丢弃的信号,无副作用。 */
async function loadImpacts(m: TabModel, gate: RiskGate) {
  for (let i = 0; i < gate.findings.length; i++) {
    // 改表结构的语句看锁表与重建评估,不数行
    if (gate.findings[i].schemaChange) {
      gate.impacts.update((list) => list.map((x, j) => (j === i ? { state: "none" } : x)));
      continue;
    }
    let next: ImpactState;
    try {
      const impact = await db.writeImpact(m.dbKind, m.connId, gate.findings[i].sql);
      next = impact ? { state: "ok", impact } : { state: "none" };
    } catch (e) {
      next = { state: "error", error: errorText(e) };
    }
    gate.impacts.update((list) => list.map((x, j) => (j === i ? next : x)));
  }
}

/** 依次评估各条改表结构语句(只读版本与表信息,不执行 ALTER)。 */
async function loadAlters(m: TabModel, gate: RiskGate) {
  for (let i = 0; i < gate.findings.length; i++) {
    if (!gate.findings[i].schemaChange) continue;
    let next: AlterState;
    try {
      const assessment = await db.alterAssess(m.dbKind, m.connId, gate.findings[i].sql);
      next = assessment ? { state: "ok", assessment } : { state: "none" };
    } catch (e) {
      next = { state: "error", error: errorText(e) };
    }
    gate.alters.update((list) => list.map((x, j) => (j === i ? next : x)));
  }
}

/** 单元格全文弹窗的内容。 */
export interface CellDialogState {
  viewIx: number;
  colIx: number;
  column: string;
  dataType: string | null;
  value: string | null;
  editable: boolean;
}

export interface TabModel {
  readonly kind: "table" | "query";
  readonly connId: string;
  readonly dbKind: DbKind;
  /** 表浏览的表名(Redis 为 key 名) */
  readonly table: string | null;

  /** 编辑器全文(查询 tab) */
  readonly sql: Signal<string>;
  /** 编辑器选区(主选区在最后) */
  readonly selections: Signal<EditorSelection[]>;
  editor: EditorRef | undefined;
  readonly editorFocused: Signal<boolean>;
  /** 表浏览当前页的 SQL */
  browseSql: string;

  readonly result: Signal<QueryResult | null>;
  readonly detail: Signal<TableDetail | null>;
  readonly redisDetail: Signal<KeyDetail | null>;
  /** Redis key 标签页:读到的值(按类型的编辑器用;集合类最多 1000 个元素) */
  readonly redisValue: Signal<RedisKeyValue | null>;
  readonly page: Signal<number>;
  /** 非空表示处于分页浏览(仅关系库表 tab) */
  readonly browseTable: Signal<string | null>;
  /** 暂存的编辑:`主键值\0列名` → 编辑 */
  readonly dirty: Signal<Record<string, CellEdit>>;
  /** 暂存删除的行(主键值) */
  readonly deletes: Signal<string[]>;
  readonly err: Signal<string | null>;
  /** 视图中选中的行(右栏行详情) */
  readonly selectedRow: Signal<number | null>;
  /** 当前结果来自查询(SELECT)还是非查询语句(DDL/DML) */
  readonly resultIsQuery: Signal<boolean>;
  readonly sort: Signal<SortState | null>;
  readonly elapsedMs: Signal<number | null>;
  readonly running: Signal<boolean>;
  /** 下一次执行在只读模式下进行(AI 生成、免确认执行的语句),由数据库强制拒绝写入 */
  readOnlyNext: boolean;

  // 筛选:编辑只改草稿,应用后才作用到结果
  readonly filterOpen: Signal<boolean>;
  readonly drafts: Signal<FilterRow[]>;
  readonly conj: Signal<Conj>;
  readonly applied: Signal<FilterRow[]>;
  readonly appliedConj: Signal<Conj>;
  /** 自然语言筛选:转换中 / 转换后的说明(模型备注、丢弃的条件)或错误 */
  readonly nlBusy: Signal<boolean>;
  readonly nlNote: Signal<{ error: boolean; text: string } | null>;

  // 工具条浮层与弹窗
  readonly limitOpen: Signal<boolean>;
  readonly historyOpen: Signal<boolean>;
  /** SQL 片段浮层是否打开 */
  readonly snippetsOpen: Signal<boolean>;
  readonly risk: Signal<RiskGate | null>;
  readonly cell: Signal<CellDialogState | null>;
  /** 编辑器里最近一次执行(运行 / Explain)的 SQL:报错时「让 AI 修复」、Explain 后「让 AI 解读」用 */
  readonly lastRun: Signal<{ sql: string; explain: boolean } | null>;
  /** 最近一次写操作的撤销(执行前抓的快照生成);下一次执行即失效 */
  readonly undo: Signal<UndoPrep | null>;
  /** 撤销确认框是否打开 */
  readonly undoOpen: Signal<boolean>;

  /** 最近一次 Explain 的执行计划(其他执行会清掉);planView 选树形或原始输出 */
  readonly plan: Signal<Plan | null>;
  readonly planView: Signal<"tree" | "raw">;
  /** 查询参数(`:name`)待填写:执行前弹框 */
  readonly params: Signal<ParamsGate | null>;
  /** 本标签上次填过的参数值(下次弹框预填) */
  paramValues: Record<string, string>;

  /** 一次执行多条语句时每条的结果(两条以上才有);结果区上方按条切换 */
  readonly multi: Signal<StmtResult[] | null>;
  readonly multiIndex: Signal<number>;

  /** 结果区实测宽度(列宽铺满用) */
  readonly gridWidth: Signal<number>;


  /** 恢复会话得到的表浏览 tab:第一次显示且连接就绪时再加载数据 */
  pendingLoad: boolean;
  /** 递增:只有最新一次异步执行的结果生效 */
  token: number;
  filterSeq: number;
  viewCache: { result: QueryResult | null; applied: FilterRow[]; conj: Conj; out: number[] } | null;
}

/** 等待填写的查询参数:填完后用代入值的 SQL 继续 `onRun`。 */
export interface ParamsGate {
  sql: string;
  names: string[];
  values: Signal<Record<string, string>>;
  onRun: (bound: string) => void;
}

/** SQL 里有 `:name` 参数时先弹框填值,填完再继续;没有参数返回 false(调用方照常执行)。 */
function askParams(m: TabModel, sql: string, onRun: (bound: string) => void): boolean {
  if (!supportsSql(m.dbKind)) return false;
  const names = sqlText.findParams(sql);
  if (names.length === 0) return false;
  const values = Object.fromEntries(names.map((n) => [n, m.paramValues[n] ?? ""]));
  m.params.set({ sql, names, values: signal(values), onRun });
  return true;
}

/** 参数弹框里点「运行」:记住填的值,代入后继续。 */
export function confirmParams(m: TabModel) {
  const gate = m.params();
  if (!gate) return;
  const values = gate.values();
  Object.assign(m.paramValues, values);
  m.params.set(null);
  gate.onRun(sqlText.bindParams(gate.sql, values, m.dbKind));
}

/** 多语句执行里的一条:结果或错误;前面出错后没执行的标为 skipped。 */
export interface StmtResult {
  sql: string;
  result: QueryResult | null;
  error: string | null;
  elapsedMs: number | null;
  isQuery: boolean;
  skipped: boolean;
}

export function createModel(kind: "table" | "query", connId: string, dbKind: DbKind, table: string | null): TabModel {
  return {
    kind,
    connId,
    dbKind,
    table,
    sql: signal(""),
    selections: signal<EditorSelection[]>([]),
    editor: undefined,
    editorFocused: signal(false),
    browseSql: "",
    result: signal<QueryResult | null>(null),
    detail: signal<TableDetail | null>(null),
    redisDetail: signal<KeyDetail | null>(null),
    redisValue: signal<RedisKeyValue | null>(null),
    page: signal(0),
    browseTable: signal<string | null>(kind === "table" && supportsSql(dbKind) ? table : null),
    dirty: signal<Record<string, CellEdit>>({}),
    deletes: signal<string[]>([]),
    err: signal<string | null>(null),
    selectedRow: signal<number | null>(null),
    resultIsQuery: signal(true),
    sort: signal<SortState | null>(null),
    elapsedMs: signal<number | null>(null),
    running: signal(false),
    readOnlyNext: false,
    filterOpen: signal(false),
    drafts: signal<FilterRow[]>([]),
    conj: signal<Conj>("and"),
    applied: signal<FilterRow[]>([]),
    appliedConj: signal<Conj>("and"),
    nlBusy: signal(false),
    nlNote: signal<{ error: boolean; text: string } | null>(null),
    limitOpen: signal(false),
    historyOpen: signal(false),
    snippetsOpen: signal(false),
    risk: signal<RiskGate | null>(null),
    cell: signal<CellDialogState | null>(null),
    lastRun: signal<{ sql: string; explain: boolean } | null>(null),
    undo: signal<UndoPrep | null>(null),
    undoOpen: signal(false),
    plan: signal<Plan | null>(null),
    planView: signal<"tree" | "raw">("tree"),
    params: signal<ParamsGate | null>(null),
    paramValues: {},
    multi: signal<StmtResult[] | null>(null),
    multiIndex: signal(0),
    gridWidth: signal(0),
    pendingLoad: false,
    token: 0,
    filterSeq: 0,
    viewCache: null,
  };
}

/** WorkTab 上挂的模型(ER 图 tab 没有)。 */
export function modelOf(tab: WorkTab | undefined | null): TabModel | undefined {
  const s = tab?.state as TabModel | undefined;
  return s && typeof s === "object" && "sql" in s ? s : undefined;
}

// ------------------------------------------------------------ 视图与派生

/** 当前视图行(按已应用的筛选)在 result.rows 中的下标。 */
/** 表浏览(关系库):筛选条件下推到服务端,对整张表生效;查询结果仍在本地筛选。 */
export function filterOnServer(m: TabModel): boolean {
  return m.browseTable() !== null && supportsSql(m.dbKind);
}

/** 表浏览某一页的 SQL:带上已应用的筛选条件与排序。 */
function browsePageSql(m: TabModel, table: string, page: number): string {
  const sort = m.sort();
  return grid.filteredPageSql(
    table,
    page,
    PAGE_SIZE,
    m.dbKind,
    sort?.col ?? null,
    sort?.desc ?? null,
    m.applied(),
    m.appliedConj(),
    columnTypes(m),
  );
}

export function viewIndices(m: TabModel): number[] {
  const result = m.result();
  const applied = m.applied();
  const conj = m.appliedConj();
  const c = m.viewCache;
  if (c && c.result === result && c.applied === applied && c.conj === conj) return c.out;
  let out: number[];
  if (!result) out = [];
  // 表浏览的条件已经拼进 SQL 由服务端筛过,这里不再二次过滤
  else if (filterOnServer(m) || !applied.some(isActive)) out = result.rows.map((_, i) => i);
  else out = grid.filterIndices(result.rows, result.columns, applied, conj);
  m.viewCache = { result, applied, conj, out };
  return out;
}

/** 视图中第 `viewIx` 行的原始数据。 */
export function viewRow(m: TabModel, viewIx: number): (string | null)[] | undefined {
  const result = m.result();
  const ri = viewIndices(m)[viewIx];
  return result && ri !== undefined ? result.rows[ri] : undefined;
}

export function isActive(f: FilterRow): boolean {
  return f.col !== "" && (f.op === "empty" || f.op === "notEmpty" || f.value !== "");
}

/** 列名 → 类型(表浏览才有)。 */
export function columnTypes(m: TabModel): Record<string, string> {
  const d = m.detail();
  const out: Record<string, string> = {};
  if (d) for (const c of d.columns) out[c.name] = c.dataType;
  return out;
}

export function pkCol(m: TabModel): string | null {
  return m.detail()?.columns.find((c) => c.isPk)?.name ?? null;
}

export function pkIndex(m: TabModel): number | null {
  const pk = pkCol(m);
  const ix = pk ? (m.result()?.columns.indexOf(pk) ?? -1) : -1;
  return ix >= 0 ? ix : null;
}

export const dirtyKey = (pk: string, col: string) => `${pk}\u0000${col}`;

export function canPrev(m: TabModel) {
  return m.browseTable() !== null && m.page() > 0;
}

export function canNext(m: TabModel) {
  return m.browseTable() !== null && (m.result()?.rows.length ?? 0) === PAGE_SIZE;
}

export function hasStaged(m: TabModel) {
  return Object.keys(m.dirty()).length > 0 || m.deletes().length > 0;
}

// ------------------------------------------------------------ 编辑器文本

/** 编辑器选区文本(无选区为空串)。 */
export function selectionText(m: TabModel): string {
  const text = m.sql();
  const r = primaryRange(text, m.selections());
  return r ? text.slice(r[0], r[1]) : "";
}

/**
 * 程序替换编辑器全文(保留撤销):格式化、插入历史、AI「编辑器运行」等。
 * 编辑器不受控(只用 defaultValue 给初值):用户打字只回报到 `m.sql`,不再把它当 value 写回去——
 * 原生侧打字快于 JS 处理 onChange 时,写回的是过时的文本,会把编辑器里已经打出来的内容替换掉、
 * 光标跳回去,后面的字插错位置。所以只有程序主动改时才经 `setValue` 推给编辑器。
 */
export function setSql(m: TabModel, text: string) {
  m.sql.set(text);
  m.editor?.setValue(text);
}

/** 在光标处插入文本(有选区则替换选区);插入的片段前后按需补换行,不与已有语句粘在一行。 */
export function insertAtCursor(m: TabModel, snippet: string) {
  const text = m.sql();
  const [a, b] = primaryRange(text, m.selections()) ?? [text.length, text.length];
  const before = text.slice(0, a);
  const after = text.slice(b);
  const lead = before && !before.endsWith("\n") ? "\n" : "";
  const tail = after && !after.startsWith("\n") ? "\n" : "";
  setSql(m, before + lead + snippet + tail + after);
}

/** 运行时取:有选区跑选区,否则跑光标所在的那条语句(Redis 为光标所在行)。 */
export function sqlToRun(m: TabModel): string {
  if (m.kind !== "query") return m.browseSql;
  const sel = selectionText(m);
  if (sel.trim()) return sel.trim();
  const text = m.sql();
  const caret = Math.min(caretOf(text, m.selections()), text.length);
  if (m.dbKind === "redis") {
    const start = text.lastIndexOf("\n", caret - 1) + 1;
    const nl = text.indexOf("\n", caret);
    return text.slice(start, nl < 0 ? text.length : nl).trim();
  }
  return sqlText.currentStatement(text, caret);
}

// ------------------------------------------------------------ 结果

function setResult(m: TabModel, r: QueryResult) {
  batch(() => {
    m.result.set(r);
    m.selectedRow.set(null);
  });
}

function connName(m: TabModel) {
  return conns.get(m.connId)?.name ?? "";
}

/** 执行成功才记历史(失败 / EXPLAIN / 翻页都不记)。写语句带上撤销,过后可从历史里撤销。 */
function record(
  m: TabModel,
  sql: string,
  elapsed: number,
  rowCount: number | null,
  affected: number | null,
  undo: UndoPrep | null = null,
) {
  recordHistory(
    {
      sql,
      connId: m.connId,
      kind: m.dbKind,
      connName: connName(m),
      executedAt: Date.now(),
      elapsedMs: elapsed,
      rowCount,
      affected,
    },
    undo,
  );
}

/** 会改表结构的语句(建 / 改 / 删表、视图、索引等):执行后要重新加载表清单、列与外键(补全用)。
 *  普通的增删改不用——每次都重新查一遍整库的列与外键太重。 */
export function changesSchema(sql: string): boolean {
  const body = sql.replace(/^(\s|--[^\n]*(\n|$)|\/\*[\s\S]*?\*\/)*/, "");
  return /^(create|alter|drop|rename|comment)\b/i.test(body);
}

/** 写语句执行前的撤销准备;出错时以原因的形式返回(不阻止执行)。 */
function prepareUndo(m: TabModel, sql: string): Promise<UndoPrep> {
  return db.undoPrepare(m.dbKind, m.connId, sql).catch((e): UndoPrep => ({ statements: [], rows: 0, reason: errorText(e) }));
}

/** 执行一条 SQL;成功走 `onOk`,失败把错误挂到结果区。 */
async function spawnQuery(m: TabModel, sql: string, onOk: (r: QueryResult, elapsed: number) => void) {
  const token = ++m.token;
  m.multi.set(null);
  m.plan.set(null);
  // 一次性标记:只作用于紧接着的这一次执行
  const readOnly = m.readOnlyNext;
  m.readOnlyNext = false;
  m.running.set(true);
  const started = Date.now();
  try {
    const r = readOnly
      ? await db.queryReadOnly(m.dbKind, m.connId, sql, maxRows())
      : await db.query(m.dbKind, m.connId, sql, maxRows());
    if (token !== m.token) return;
    batch(() => {
      m.running.set(false);
      m.err.set(null);
      onOk(r, Date.now() - started);
    });
  } catch (e) {
    if (token !== m.token) return;
    batch(() => {
      m.running.set(false);
      m.err.set(errorText(e));
      m.result.set(null);
      m.selectedRow.set(null);
    });
  }
}

function resetFilter(m: TabModel) {
  batch(() => {
    m.drafts.set([]);
    m.applied.set([]);
    m.conj.set("and");
    m.appliedConj.set("and");
    m.filterOpen.set(false);
    m.nlNote.set(null);
  });
}

/** 运行编辑器里的 SQL(或 Redis 命令)。`override` 用于「插入并运行」「再次执行历史」。 */
export function run(m: TabModel, override?: string, skipHistory = false, skipRisk = false) {
  const sql = override ?? sqlToRun(m);
  if (!sql.trim()) {
    m.err.set(m.dbKind === "redis" ? t("tab.empty_command") : t("tab.empty_sql"));
    return;
  }
  // `:name` 参数:先填值,代入后再走风险审查与执行
  if (askParams(m, sql, (bound) => run(m, bound, skipHistory, skipRisk))) return;
  if (m.dbKind === "mongo") {
    toast.info(t("tab.mongo_unsupported"));
    return;
  }
  if (!skipRisk) {
    const conn = conns.get(m.connId);
    // prod 连接从严:带 WHERE 的 UPDATE / DELETE 也先确认,并预估影响范围;
    // MySQL 的改表结构语句也列出,评估能否 INSTANT / INPLACE、会不会锁表
    const strict = conn?.env === "prod";
    const findings =
      m.dbKind === "redis"
        ? sqlText.analyzeRedis(sql)
        : m.dbKind === "mysql"
          ? sqlText.analyzeMysql(sql, strict)
          : sqlText.analyze(sql, strict);
    if (findings.length > 0) {
      // 转为由用户在确认框里明确决定:不再沿用免确认的只读标记,
      // 也避免取消后标记残留到下一次手动执行
      m.readOnlyNext = false;
      const database = conn ? (conn.kind === "sqlite" ? (conn.filePath ?? "") : conn.database) : "";
      const relational = supportsSql(m.dbKind);
      const gate: RiskGate = {
        findings,
        targetLabel: conn?.name ?? "",
        database: database ? database : null,
        relational,
        impacts: signal<ImpactState[]>(relational ? findings.map(() => ({ state: "loading" as const })) : []),
        alters: signal<AlterState[]>(
          findings.map((f) => (f.schemaChange ? { state: "loading" as const } : { state: "none" as const })),
        ),
        onConfirm: () => run(m, sql, skipHistory, true),
      };
      // 非 prod 连接上只因改表结构才命中:先评估,高风险才弹确认框,否则直接执行
      if (!strict && findings.every((f) => f.assessOnly)) {
        void assessThenRun(m, gate);
        return;
      }
      m.risk.set(gate);
      if (relational) void loadImpacts(m, gate);
      void loadAlters(m, gate);
      return;
    }
  }
  if (m.dbKind === "redis") {
    void runRedis(m, sql, skipHistory);
    return;
  }
  // 多条语句:逐条执行,每条一个结果
  const statements = sqlText.statements(sql);
  if (statements.length > 1) {
    void runMany(m, statements, skipHistory);
    return;
  }
  const query = sqlText.isQuery(sql);
  m.lastRun.set({ sql, explain: false });
  m.undo.set(null);
  const go = (undo: UndoPrep | null) =>
    spawnQuery(m, sql, (r, elapsed) => {
      m.browseTable.set(null);
      m.page.set(0);
      m.resultIsQuery.set(query);
      m.sort.set(null);
      resetFilter(m);
      m.elapsedMs.set(elapsed);
      setResult(m, r);
      m.undo.set(undo);
      // 改了表结构:刷新表清单、列与外键(补全的 JOIN … ON 推荐跟着变)
      if (!query && changesSchema(sql) && conns.tables(m.connId)) void refreshTables(m.connId);
      if (!skipHistory) record(m, sql, elapsed, query ? r.rows.length : null, query ? null : (r.affected ?? null), undo);
    });
  // 写语句:先抓会被改动的原始行,生成撤销用的反向语句,再执行
  if (supportsSql(m.dbKind) && !sqlText.isReadOnly(sql)) {
    const token = m.token;
    m.running.set(true);
    void prepareUndo(m, sql).then((undo) => {
        // 等待期间又执行了别的(或标签页被重用):放弃这次
        if (m.token !== token) return;
        void go(undo);
      });
    return;
  }
  void go(null);
}

/** 非 prod 连接上的改表结构语句:评估完再决定——有一条高风险(COPY / 锁写 / 大表重建)就弹确认框,
 *  否则直接执行。评估失败不拦着(表不存在之类的错误执行时自会报出)。 */
async function assessThenRun(m: TabModel, gate: RiskGate) {
  const token = m.token;
  m.running.set(true);
  await loadAlters(m, gate);
  // 等待期间又执行了别的(或标签页被重用):放弃这次
  if (m.token !== token) return;
  m.running.set(false);
  const high = gate.alters().some((a) => a.state === "ok" && a.assessment.risk === "high");
  if (high) {
    gate.impacts.set(gate.findings.map(() => ({ state: "none" as const })));
    m.risk.set(gate);
  } else gate.onConfirm();
}

/** 运行编辑器里的全部语句(⌘⇧↵)。 */
export function runAll(m: TabModel) {
  if (m.kind !== "query") return;
  run(m, m.sql());
}

/** 逐条执行多条语句:遇到错误即停,后面的标为未执行。
 *  每条写语句执行前各自抓快照(前面的语句可能已经改了数据);执行过的写语句都能撤销时,
 *  合成一份倒序的整体撤销;任一条不能撤销就整体不提供(给出那条的原因)。 */
async function runMany(m: TabModel, statements: string[], skipHistory: boolean) {
  const token = ++m.token;
  const readOnly = m.readOnlyNext;
  m.readOnlyNext = false;
  batch(() => {
    m.running.set(true);
    m.undo.set(null);
    m.plan.set(null);
    m.lastRun.set({ sql: statements.join(";\n"), explain: false });
  });
  // SQLite:脚本可能自己写了 BEGIN / PRAGMA(如结构对比生成的重建表脚本),中途出错停下会把连接留在
  // 未结束的事务里、legacy_alter_table / foreign_keys 停在改过的值;先记下,出错后善后
  const flags = m.dbKind === "sqlite" && !readOnly ? await db.sqliteSessionFlags(m.connId).catch(() => null) : null;
  if (token !== m.token) return;
  const out: StmtResult[] = [];
  let failed = false;
  let schemaChanged = false;
  const undos: UndoPrep[] = [];
  for (const sql of statements) {
    const isQuery = sqlText.isQuery(sql);
    if (failed) {
      out.push({ sql, result: null, error: null, elapsedMs: null, isQuery, skipped: true });
      continue;
    }
    const undo = !readOnly && !sqlText.isReadOnly(sql) ? await prepareUndo(m, sql) : null;
    if (token !== m.token) return;
    const started = Date.now();
    try {
      const r = readOnly
        ? await db.queryReadOnly(m.dbKind, m.connId, sql, maxRows())
        : await db.query(m.dbKind, m.connId, sql, maxRows());
      if (token !== m.token) return;
      const elapsed = Date.now() - started;
      out.push({ sql, result: r, error: null, elapsedMs: elapsed, isQuery, skipped: false });
      if (!isQuery && changesSchema(sql)) schemaChanged = true;
      if (undo) undos.push(undo);
      if (!skipHistory) record(m, sql, elapsed, isQuery ? r.rows.length : null, isQuery ? null : (r.affected ?? null), undo);
    } catch (e) {
      if (token !== m.token) return;
      out.push({ sql, result: null, error: errorText(e), elapsedMs: null, isQuery, skipped: false });
      failed = true;
    }
  }
  if (token !== m.token) return;
  // 默认显示出错的那条,否则最后一条
  const errorAt = out.findIndex((x) => x.error !== null);
  if (failed && flags) {
    const rolledBack = await db.sqliteRecover(m.connId, flags).catch(() => false);
    if (token !== m.token) return;
    if (rolledBack && errorAt >= 0) {
      const item = out[errorAt];
      out[errorAt] = { ...item, error: `${item.error}\n${t("tab.sqlite_rolled_back")}` };
    }
  }
  batch(() => {
    m.running.set(false);
    m.undo.set(combineUndo(undos));
    m.multi.set(out);
    selectResult(m, errorAt >= 0 ? errorAt : out.length - 1);
  });
  if (schemaChanged && conns.tables(m.connId)) void refreshTables(m.connId);
}

/** 多条写语句的撤销合成一份:后执行的先撤销;任一条不能撤销就整体不提供。没改到行的语句不参与。 */
function combineUndo(list: UndoPrep[]): UndoPrep | null {
  const blocked = list.find((u) => u.statements.length === 0 && u.reason);
  if (blocked) return { statements: [], rows: 0, reason: blocked.reason };
  const usable = list.filter((u) => u.statements.length > 0);
  if (usable.length === 0) return null;
  return {
    statements: usable
      .slice()
      .reverse()
      .flatMap((u) => u.statements),
    rows: usable.reduce((n, u) => n + u.rows, 0),
    reason: null,
    parts: usable,
  };
}

/** 切换多语句结果里的第 i 条:结果区、状态行、筛选与排序都跟着换。 */
export function selectResult(m: TabModel, i: number) {
  const item = m.multi()?.[i];
  if (!item) return;
  batch(() => {
    m.multiIndex.set(i);
    m.browseTable.set(null);
    m.page.set(0);
    m.sort.set(null);
    resetFilter(m);
    m.resultIsQuery.set(item.isQuery);
    m.elapsedMs.set(item.elapsedMs);
    m.err.set(item.error);
    m.result.set(item.result);
    // 「让 AI 修复」针对的是这一条
    m.lastRun.set({ sql: item.sql, explain: false });
    m.selectedRow.set(null);
  });
}

/** 撤销最近一次写操作:在一个事务里执行反向语句。 */
export async function undoLast(m: TabModel) {
  const undo = m.undo();
  if (!undo || undo.statements.length === 0) return;
  try {
    await db.importSql(m.dbKind, m.connId, undo.statements);
    forgetUndo(undo);
    batch(() => {
      m.undo.set(null);
      m.undoOpen.set(false);
    });
    toast.success(t("undo.done", { rows: undo.rows }));
  } catch (e) {
    m.undoOpen.set(false);
    toast.error(t("undo.failed", { error: errorText(e) }));
  }
}

async function runRedis(m: TabModel, cmd: string, skipHistory: boolean) {
  const token = ++m.token;
  m.running.set(true);
  const started = Date.now();
  try {
    const reply = await db.redisExec(m.connId, cmd);
    if (token !== m.token) return;
    const elapsed = Date.now() - started;
    batch(() => {
      m.running.set(false);
      m.err.set(null);
      m.elapsedMs.set(elapsed);
      m.resultIsQuery.set(true);
      setResult(m, {
        columns: [t("tab.redis_result_column")],
        rows: reply === "" ? [] : reply.split("\n").map((l) => [l]),
        affected: null,
      });
    });
    if (!skipHistory) record(m, cmd, elapsed, null, null);
  } catch (e) {
    if (token !== m.token) return;
    batch(() => {
      m.running.set(false);
      m.err.set(errorText(e));
      m.result.set(null);
    });
  }
}

/** EXPLAIN 当前/选中的 SQL(纯 EXPLAIN 不执行查询)。 */
export function explain(m: TabModel, override?: string) {
  if (!supportsSql(m.dbKind) || m.kind !== "query") return;
  const sql = override ?? sqlToRun(m);
  if (!sql.trim()) {
    m.err.set(t("tab.empty_sql"));
    return;
  }
  if (askParams(m, sql, (bound) => explain(m, bound))) return;
  void runExplain(m, sql, false);
}

/** 对当前执行计划的语句真正执行一次(EXPLAIN ANALYZE),拿到实际耗时与行数;只对只读语句开放。 */
export function explainAnalyze(m: TabModel) {
  const last = m.lastRun();
  if (!last?.explain || !sqlText.isReadOnly(last.sql)) return;
  void runExplain(m, last.sql, true);
}

/** 能否对当前计划做 EXPLAIN ANALYZE:PG / MySQL 的只读语句,且这份计划还没跑过 ANALYZE。 */
export function canAnalyze(m: TabModel): boolean {
  const last = m.lastRun();
  const plan = m.plan();
  return (m.dbKind === "pg" || m.dbKind === "mysql") && !!last?.explain && !!plan && !plan.analyzed && sqlText.isReadOnly(last.sql);
}

async function runExplain(m: TabModel, sql: string, analyze: boolean) {
  const token = ++m.token;
  batch(() => {
    m.multi.set(null);
    m.plan.set(null);
    m.running.set(true);
    m.lastRun.set({ sql, explain: true });
  });
  const started = Date.now();
  try {
    const plan = await db.explainPlan(m.dbKind, m.connId, sql, analyze);
    if (token !== m.token) return;
    batch(() => {
      m.running.set(false);
      m.err.set(null);
      m.browseTable.set(null);
      m.page.set(0);
      m.resultIsQuery.set(true);
      m.sort.set(null);
      resetFilter(m);
      m.elapsedMs.set(Date.now() - started);
      setResult(m, plan.raw);
      m.plan.set(plan);
      m.planView.set(plan.tree ? "tree" : "raw");
    });
  } catch (e) {
    if (token !== m.token) return;
    batch(() => {
      m.running.set(false);
      m.err.set(errorText(e));
      m.result.set(null);
    });
  }
}

/** 格式化整篇 SQL。 */
export function beautify(m: TabModel) {
  if (!supportsSql(m.dbKind) || m.kind !== "query") return;
  const source = m.sql();
  const formatted = sqlText.format(source);
  if (formatted !== source) setSql(m, formatted);
}

/** 跳页(仅表浏览)。 */
export function gotoPage(m: TabModel, page: number) {
  const table = m.browseTable();
  if (!table || !supportsSql(m.dbKind)) return;
  const sql = browsePageSql(m, table, page);
  m.browseSql = sql;
  void spawnQuery(m, sql, (r, elapsed) => {
    m.page.set(page);
    m.resultIsQuery.set(true);
    m.elapsedMs.set(elapsed);
    setResult(m, r);
  });
}

/** 点击表头:浏览表时循环 倒序 → 升序 → 无 并重查第一页;其余结果做客户端排序(升 ↔ 降)。 */
export function cycleSort(m: TabModel, col: string) {
  const cur = m.sort();
  if (m.browseTable()) {
    const next: SortState | null =
      cur?.col === col ? (cur.desc ? { col, desc: false } : null) : { col, desc: true };
    m.sort.set(next);
    gotoPage(m, 0);
    return;
  }
  const result = m.result();
  if (!result) return;
  const ci = result.columns.indexOf(col);
  if (ci < 0) return;
  const desc = cur?.col === col && !cur.desc;
  const order = grid.sortOrder(result.rows, ci, desc);
  batch(() => {
    m.sort.set({ col, desc });
    setResult(m, { columns: result.columns, rows: order.map((i) => result.rows[i]), affected: result.affected });
  });
}

/** 刷新当前 tab:表 tab 重拉结构与当前页;查询 tab 只重跑查询类语句(写语句不隐式重放)。 */
export function refresh(m: TabModel) {
  if (m.kind === "query") {
    const sql = sqlToRun(m);
    if (!sql.trim()) toast.info(t("tab.refreshed_tables"));
    else if (supportsSql(m.dbKind) && !sqlText.isQuery(sql)) toast.info(t("tab.refresh_skip_write"));
    else {
      toast.info(t("tab.rerunning"));
      run(m);
    }
    return;
  }
  const table = m.table;
  if (!table) return;
  if (m.dbKind === "redis") {
    void refreshRedisKey(m, table);
    return;
  }
  if (!supportsSql(m.dbKind)) return;
  void refreshTable(m, table);
}

/** 表结构改了(SQLite 改表结构之后):列可能已改名或删掉,排序、筛选与暂存的编辑都可能指向不存在的列,
 *  清掉后回到第一页重新读数据与列信息。 */
export function reloadAfterSchemaChange(m: TabModel) {
  batch(() => {
    m.sort.set(null);
    m.page.set(0);
    m.dirty.set({});
    m.deletes.set([]);
    m.selectedRow.set(null);
  });
  resetFilter(m);
  refresh(m);
}

/** 重读 Redis key 标签页的值与详情(编辑器写完后调用)。 */
export async function refreshRedisKey(m: TabModel, key: string) {
  const token = ++m.token;
  m.running.set(true);
  try {
    const [kv, detail] = await redisKey(m.connId, key);
    if (token !== m.token) return;
    batch(() => {
      m.running.set(false);
      m.err.set(null);
      m.redisDetail.set(detail);
      m.redisValue.set(kv);
      m.resultIsQuery.set(true);
      setResult(m, { columns: kv.columns, rows: kv.rows, affected: null });
    });
  } catch (e) {
    if (token !== m.token) return;
    batch(() => {
      m.running.set(false);
      m.err.set(errorText(e));
    });
  }
}

async function refreshTable(m: TabModel, table: string) {
  const sql = browsePageSql(m, table, m.page());
  m.browseSql = sql;
  const token = ++m.token;
  m.running.set(true);
  const started = Date.now();
  try {
    const r = await db.query(m.dbKind, m.connId, sql, maxRows());
    const detail = await db.tableDetail(m.dbKind, m.connId, table);
    if (token !== m.token) return;
    const elapsed = Date.now() - started;
    batch(() => {
      m.running.set(false);
      m.err.set(null);
      m.detail.set(detail);
      m.resultIsQuery.set(true);
      m.elapsedMs.set(elapsed);
      setResult(m, r);
    });
  } catch (e) {
    if (token !== m.token) return;
    batch(() => {
      m.running.set(false);
      m.err.set(errorText(e));
    });
  }
}

// ------------------------------------------------------------ 编辑与删除

function pkOfView(m: TabModel, viewIx: number): string | null {
  const pi = pkIndex(m);
  if (pi === null) return null;
  const row = viewRow(m, viewIx);
  return row ? (row[pi] ?? "") : null;
}

/** 暂存单元格修改(视图行 + 列)。 */
export function stageEdit(m: TabModel, viewIx: number, colIx: number, newValue: string) {
  const pk = pkCol(m);
  const table = m.table;
  const result = m.result();
  if (!pk || !table || !result) return;
  const pkValue = pkOfView(m, viewIx);
  if (pkValue === null) return;
  const column = result.columns[colIx];
  if (column === undefined) return;
  m.dirty.update((d) => ({ ...d, [dirtyKey(pkValue, column)]: { table, pkCol: pk, pkValue, column, newValue } }));
}

/** 右键「删除行 / 撤销删除」:暂存/取消暂存该行的删除。 */
export function toggleDelete(m: TabModel, viewIx: number) {
  const pk = pkOfView(m, viewIx);
  if (pk === null || pkCol(m) === null) return;
  m.deletes.update((l) => (l.includes(pk) ? l.filter((d) => d !== pk) : [...l, pk]));
}

/** ⌘S:一个事务里提交暂存的编辑与删除,成功后清空暂存并重查当前页。 */
export async function commit(m: TabModel) {
  const table = m.table;
  if (!table) return;
  const edits = Object.values(m.dirty());
  const pk = pkCol(m);
  const deletes: RowDelete[] = pk ? m.deletes().map((v) => ({ table, pkCol: pk, pkValue: v })) : [];
  if (edits.length === 0 && deletes.length === 0) return;
  const token = ++m.token;
  m.running.set(true);
  try {
    await db.commitEdits(m.dbKind, m.connId, edits, deletes);
    if (token !== m.token) return;
    batch(() => {
      m.running.set(false);
      m.dirty.set({});
      m.deletes.set([]);
    });
    if (deletes.length > 0) toast.success(t("tab.deleted_rows", { n: deletes.length }));
    refresh(m);
  } catch (e) {
    if (token !== m.token) return;
    batch(() => {
      m.running.set(false);
      m.err.set(errorText(e));
    });
  }
}

// ------------------------------------------------------------ 行操作

/** 双击 / 右键「编辑 | 查看」:弹出单元格全文。 */
export function openCell(m: TabModel, viewIx: number, colIx: number) {
  const result = m.result();
  const row = viewRow(m, viewIx);
  if (!result || !row) return;
  const column = result.columns[colIx] ?? "";
  m.cell.set({
    viewIx,
    colIx,
    column,
    dataType: columnTypes(m)[column] ?? null,
    value: row[colIx] ?? null,
    editable: pkCol(m) !== null && m.table !== null,
  });
}

/** 右键「查看详情」:选中该行并打开右栏行详情。 */
export function openRowDetail(m: TabModel, viewIx: number) {
  m.selectedRow.set(viewIx);
  setDetailOpen(true);
}

/** 复制结果某一行的 INSERT(真实值)。 */
export async function copyInsertRow(m: TabModel, viewIx: number, write: (text: string) => Promise<void>) {
  const result = m.result();
  const row = viewRow(m, viewIx);
  if (!result || !row || !supportsSql(m.dbKind)) return;
  const table = m.table ?? "table";
  await write(grid.buildInsertRow(table, result.columns, row, m.dbKind));
  toast.success(t("tab.copied"));
}

export function clearSelection(m: TabModel) {
  m.selectedRow.set(null);
}

/** 导出用的表名:真实表名优先,查询 tab 从 SQL 文本里猜。 */
export function exportName(m: TabModel): string {
  return m.table ?? sqlText.guessTableName(m.sql()) ?? "export";
}

/** 当前视图行(导出)。 */
export function viewRows(m: TabModel): (string | null)[][] {
  const result = m.result();
  if (!result) return [];
  return viewIndices(m).map((i) => result.rows[i]);
}

// ------------------------------------------------------------ 筛选

function newDraft(m: TabModel, col: string): FilterRow {
  m.filterSeq += 1;
  return { id: m.filterSeq, col, op: "contains", value: "" };
}

export function toggleFilter(m: TabModel) {
  const result = m.result();
  if (!result) return;
  batch(() => {
    if (m.filterOpen()) {
      const had = m.applied().some(isActive);
      m.filterOpen.set(false);
      m.applied.set([]);
      // 服务端筛过的结果:收起筛选条即取消条件,重查第一页
      if (had && filterOnServer(m)) gotoPage(m, 0);
    } else {
      m.filterOpen.set(true);
      if (m.drafts().length === 0) m.drafts.set([newDraft(m, result.columns[0] ?? "")]);
    }
    m.selectedRow.set(null);
  });
}

export function filterAdd(m: TabModel) {
  const col = m.result()?.columns[0] ?? "";
  m.drafts.update((l) => [...l, newDraft(m, col)]);
}

export function updateDraft(m: TabModel, id: number, patch: Partial<FilterRow>) {
  m.drafts.update((l) => l.map((d) => (d.id === id ? { ...d, ...patch } : d)));
}

export function removeDraft(m: TabModel, id: number) {
  m.drafts.update((l) => l.filter((d) => d.id !== id));
}

/** 草稿与已应用是否不一致(决定「应用」按钮态)。 */
export function filterDirty(m: TabModel): boolean {
  return grid.filtersSignature(m.drafts(), m.conj()) !== grid.filtersSignature(m.applied(), m.appliedConj());
}

export function filterApply(m: TabModel) {
  if (!filterDirty(m)) return;
  batch(() => {
    m.applied.set(m.drafts());
    m.appliedConj.set(m.conj());
    m.selectedRow.set(null);
  });
  // 表浏览:条件拼进 SQL,从第一页重查整张表
  if (filterOnServer(m)) gotoPage(m, 0);
}

/** 清除:草稿与已应用一并清空,并收起筛选条。 */
export function filterClear(m: TabModel) {
  const had = m.applied().some(isActive);
  batch(() => {
    resetFilter(m);
    m.selectedRow.set(null);
  });
  if (had && filterOnServer(m)) gotoPage(m, 0);
}

/** 发给 AI 的样例行数(帮它认出枚举值的写法)。 */
const NL_SAMPLE_ROWS = 5;

/** 自然语言筛选:一句话交给 AI 转成且/或条件,替换草稿并应用;条件留在筛选条上可见可改。 */
export async function filterFromText(m: TabModel, text: string) {
  const result = m.result();
  const question = text.trim();
  if (!result || !question || m.nlBusy()) return;
  if (!ai.status().ready) {
    m.nlNote.set({ error: true, text: t("filter.nl_not_configured") });
    return;
  }
  const token = m.token;
  batch(() => {
    m.nlBusy.set(true);
    m.nlNote.set(null);
  });
  try {
    const types = columnTypes(m);
    const columns = result.columns.map((name) => ({ name, dataType: types[name] ?? null }));
    const r = await ai.nlFilter(question, columns, result.rows.slice(0, NL_SAMPLE_ROWS));
    // 等待期间结果被换掉(翻页、刷新、重新执行):条件可能已不适用,丢弃
    if (token !== m.token) return;
    if (r.filters.length === 0) {
      m.nlNote.set({ error: true, text: r.note ?? t("filter.nl_nothing") });
      return;
    }
    batch(() => {
      m.drafts.set(
        r.filters.map((f) => {
          m.filterSeq += 1;
          return { id: m.filterSeq, col: f.col, op: f.op, value: f.value };
        }),
      );
      m.conj.set(r.conj);
      m.filterOpen.set(true);
    });
    filterApply(m);
    const parts = [r.note, r.dropped.length > 0 ? t("filter.nl_dropped", { list: r.dropped.join("; ") }) : null];
    const info = parts.filter((p): p is string => !!p).join(" ");
    m.nlNote.set(info ? { error: false, text: info } : null);
  } catch (e) {
    m.nlNote.set({ error: true, text: errorText(e) });
  } finally {
    m.nlBusy.set(false);
  }
}

// ------------------------------------------------------------ JSON

/** 单元格值是 JSON 对象 / 数组:展开成树显示。不看列类型——SQLite 的 JSON 文本、查询结果(没有类型)
 *  一样展开;json 列里的标量(`3`、`"x"`)没什么可展开的,不算。 */
export function isJsonDoc(value: string | null): value is string {
  return value !== null && grid.isJsonDocument(value);
}

/** 能否在 JSON 树上点字段生成筛选:只做 PG。表浏览下推到服务端,要求列是 json / jsonb
 *  (其他类型的列要先转 jsonb,有一行不是合法 JSON 整条查询就会报错);查询结果在本地按路径筛,不限类型。 */
export function canFilterJson(m: TabModel, column: string): boolean {
  if (m.dbKind !== "pg" || !m.result()) return false;
  return !filterOnServer(m) || /json/i.test(columnTypes(m)[column] ?? "");
}

/** JSON 树上「按此值筛选」:追加一条 `列 → 路径 = 值` 的条件(记下字段类型,等于按类型比较),
 *  打开筛选条并立即应用(表浏览下推成 WHERE,从第一页重查)。空字符串改用「为空」。 */
export function filterByJson(m: TabModel, col: string, path: JsonPath, kind: JsonKind, value: string) {
  m.filterSeq += 1;
  const blank = kind === "string" && value === "";
  const row: FilterRow = {
    id: m.filterSeq,
    col,
    op: blank ? "empty" : "eq",
    value: blank ? "" : value,
    path,
    jsonType: kind,
  };
  batch(() => {
    // 打开筛选条时放的那条空白占位不留
    m.drafts.set([...m.drafts().filter(isActive), row]);
    m.filterOpen.set(true);
    m.nlNote.set(null);
  });
  filterApply(m);
}

export function filterPreviewCount(m: TabModel): number {
  const result = m.result();
  if (!result) return 0;
  const drafts = m.drafts();
  if (!drafts.some(isActive)) return result.rows.length;
  return grid.filterIndices(result.rows, result.columns, drafts, m.conj()).length;
}

// ------------------------------------------------------------ AI

/** 最近一次执行报错:把 SQL 与报错交给 AI 修正。 */
export function aiFixError(m: TabModel) {
  const last = m.lastRun();
  const err = m.err();
  if (!last || err === null) return;
  askAiFixError(last.sql, err);
}

/** 最近一次是 Explain 且有结果:把执行计划交给 AI 解读。 */
export function aiExplainPlan(m: TabModel) {
  const last = m.lastRun();
  const r = m.result();
  if (!last?.explain || !r) return;
  askAiExplainPlan(last.sql, planText(r.columns, r.rows));
}

/** ⌘⇧A / 划词按钮:把选中的 SQL 作为引用发给 AI 助手。 */
export function sendSelectionToAi(m: TabModel) {
  const text = selectionText(m).trim();
  if (!text) toast.error(t("tab.select_sql_for_ai"));
  else sendToAi(text);
}
