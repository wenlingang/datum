// 数据对比 tab 的模型:选择(源 / 目标的连接与表、键、筛选、行数上限)、后台任务的轮询与结果、同步脚本。
// 挂在 tab.state 上,切换标签页时组件重建,进行中的对比照常继续。

import { batch, signal, type Signal } from "nsh";
import { t } from "nsh/i18n";
import { db, errorText } from "../lib/native";
import type { DataDiffInspection, DataDiffKind, DataDiffProgress, DataDiffReport } from "../lib/types";
import { conns, loadTables } from "../store/conns";

/** 每边默认最多读的行数(与 core 的默认一致)。 */
export const DEFAULT_MAX_ROWS = 1_000_000;
const POLL_MS = 150;

export type DataDiffFilter = "all" | DataDiffKind;

export interface DataDiffModel {
  readonly kind: "ddiff";
  sourceConn: Signal<string>;
  sourceTable: Signal<string | null>;
  targetConn: Signal<string | null>;
  targetTable: Signal<string | null>;
  inspection: Signal<DataDiffInspection | null>;
  inspectError: Signal<string | null>;
  /** 无主键时选的唯一键(列名用逗号连起来) */
  uniqueKey: Signal<string | null>;
  filter: Signal<string>;
  maxRows: Signal<string>;
  job: Signal<string | null>;
  progress: Signal<DataDiffProgress | null>;
  running: Signal<boolean>;
  error: Signal<string | null>;
  report: Signal<DataDiffReport | null>;
  category: Signal<DataDiffFilter>;
  includeDeletes: Signal<boolean>;
  script: Signal<string>;
  timer: ReturnType<typeof setInterval> | undefined;
  inspectSeq: number;
}

export function isDataDiffModel(v: unknown): v is DataDiffModel {
  return !!v && typeof v === "object" && (v as { kind?: string }).kind === "ddiff";
}

/** 新建模型:源为本连接(可带预选的表);目标默认是另一个同类型连接,没有就是本连接。 */
export function createDataDiffModel(connId: string, table: string | null): DataDiffModel {
  const src = conns.get(connId);
  const other = conns.list().find((c) => c.id !== connId && c.kind === src?.kind);
  const m: DataDiffModel = {
    kind: "ddiff",
    sourceConn: signal(connId),
    sourceTable: signal(table),
    targetConn: signal(other?.id ?? connId),
    targetTable: signal(other ? table : null),
    inspection: signal(null),
    inspectError: signal(null),
    uniqueKey: signal(null),
    filter: signal(""),
    maxRows: signal(String(DEFAULT_MAX_ROWS)),
    job: signal(null),
    progress: signal(null),
    running: signal(false),
    error: signal(null),
    report: signal(null),
    category: signal("all"),
    includeDeletes: signal(false),
    script: signal(""),
    timer: undefined,
    inspectSeq: 0,
  };
  void ensureTables(m);
  return m;
}

/** 两边的表列表(未连接的先连上);目标表未选时默认取与源表同名的表。 */
export async function ensureTables(m: DataDiffModel) {
  const ids = [m.sourceConn(), m.targetConn()].filter((x): x is string => !!x);
  for (const id of new Set(ids)) {
    if (!conns.tables(id) && !conns.isLoading(id)) await loadTables(id);
  }
  const target = m.targetConn();
  if (target && !m.targetTable() && m.sourceTable()) {
    const list = conns.tables(target) ?? [];
    if (list.includes(m.sourceTable()!)) m.targetTable.set(m.sourceTable());
  }
  void reinspect(m);
}

/** 两边的表都选好后读结构对照(键、共同列)。 */
export async function reinspect(m: DataDiffModel) {
  const seq = ++m.inspectSeq;
  const src = conns.get(m.sourceConn());
  const st = m.sourceTable();
  const tc = m.targetConn();
  const tt = m.targetTable();
  batch(() => {
    m.inspection.set(null);
    m.inspectError.set(null);
    m.uniqueKey.set(null);
  });
  if (!src || !st || !tc || !tt) return;
  try {
    const r = await db.dataDiffInspect(src.kind, { connId: src.id, table: st }, { connId: tc, table: tt });
    if (seq !== m.inspectSeq) return;
    batch(() => {
      m.inspection.set(r);
      if (r.primaryKey.length === 0 && r.uniqueKeys.length > 0) m.uniqueKey.set(r.uniqueKeys[0].columns.join(","));
    });
  } catch (e) {
    if (seq === m.inspectSeq) m.inspectError.set(errorText(e));
  }
}

/** 选择变化后旧结果作废。 */
function resetResult(m: DataDiffModel) {
  const job = m.job();
  if (job && !m.running()) db.dataDiffClose(job);
  batch(() => {
    if (!m.running()) m.job.set(null);
    m.report.set(null);
    m.error.set(null);
    m.script.set("");
    m.progress.set(null);
  });
}

export function setSourceTable(m: DataDiffModel, table: string) {
  m.sourceTable.set(table);
  const target = m.targetConn();
  if (target && (conns.tables(target) ?? []).includes(table)) m.targetTable.set(table);
  resetResult(m);
  void reinspect(m);
}

export function setTargetConn(m: DataDiffModel, id: string) {
  batch(() => {
    m.targetConn.set(id);
    m.targetTable.set(null);
  });
  resetResult(m);
  void ensureTables(m);
}

export function setTargetTable(m: DataDiffModel, table: string) {
  m.targetTable.set(table);
  resetResult(m);
  void reinspect(m);
}

/** 交换源与目标(反方向同步),已有结果作废。 */
export function swap(m: DataDiffModel) {
  const tc = m.targetConn();
  if (!tc || m.running()) return;
  batch(() => {
    const [sc, st, tt] = [m.sourceConn(), m.sourceTable(), m.targetTable()];
    m.sourceConn.set(tc);
    m.sourceTable.set(tt);
    m.targetConn.set(sc);
    m.targetTable.set(st);
  });
  resetResult(m);
  void ensureTables(m);
}

/** 用来对齐行的键:主键,或选中的唯一键;都没有为空。 */
export function keyColumns(m: DataDiffModel): string[] {
  const ins = m.inspection();
  if (!ins) return [];
  if (ins.primaryKey.length > 0) return ins.primaryKey;
  return m.uniqueKey()?.split(",") ?? [];
}

export function canRun(m: DataDiffModel): boolean {
  return !m.running() && !!m.inspection() && keyColumns(m).length > 0;
}

function maxRowsValue(m: DataDiffModel): number {
  const n = Number.parseInt(m.maxRows().replace(/[,_\s]/g, ""), 10);
  return Number.isFinite(n) && n > 0 ? n : DEFAULT_MAX_ROWS;
}

/** 开始对比(后台),轮询到完成后取同步脚本。 */
export async function runCompare(m: DataDiffModel) {
  const src = conns.get(m.sourceConn());
  const st = m.sourceTable();
  const tc = m.targetConn();
  const tt = m.targetTable();
  if (!src || !st || !tc || !tt || !canRun(m)) return;
  resetResult(m);
  m.running.set(true);
  try {
    const job = await db.dataDiffStart({
      kind: src.kind,
      source: { connId: src.id, table: st },
      target: { connId: tc, table: tt },
      key: m.inspection()!.primaryKey.length > 0 ? [] : keyColumns(m),
      filter: m.filter().trim() || null,
      maxRows: maxRowsValue(m),
    });
    m.job.set(job);
    if (m.timer) clearInterval(m.timer);
    m.timer = setInterval(() => poll(m), POLL_MS);
  } catch (e) {
    batch(() => {
      m.running.set(false);
      m.error.set(errorText(e));
    });
  }
}

function poll(m: DataDiffModel) {
  const job = m.job();
  const p = job ? db.dataDiffProgress(job) : null;
  if (!p || p.done) {
    if (m.timer) clearInterval(m.timer);
    m.timer = undefined;
  }
  batch(() => {
    m.progress.set(p);
    if (!p) {
      m.running.set(false);
      return;
    }
    if (!p.done) return;
    m.running.set(false);
    if (p.error) {
      m.error.set(p.error);
      return;
    }
    m.report.set(p.report);
    m.category.set("all");
  });
  if (p?.done && p.report) refreshScript(m);
}

export function cancelCompare(m: DataDiffModel) {
  const job = m.job();
  if (job) db.dataDiffCancel(job);
}

export function refreshScript(m: DataDiffModel) {
  const job = m.job();
  if (!job || !m.report()) return;
  try {
    m.script.set(db.dataDiffScript(job, m.includeDeletes()));
  } catch (e) {
    m.error.set(errorText(e));
  }
}

export function setIncludeDeletes(m: DataDiffModel, v: boolean) {
  m.includeDeletes.set(v);
  refreshScript(m);
}

/** 标签标题:带上源表名。 */
export function dataDiffTitle(table: string | null): string {
  return table ? t("ddiff.title_table", { table }) : t("ddiff.title");
}
