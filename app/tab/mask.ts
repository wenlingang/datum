// 结果网格的敏感数据遮罩:每份结果按连接的规则算一次各列的遮罩方式(原生侧识别),
// 网格、行详情、单元格弹窗、复制、导出都经这里取「展示值」。原值只在点了「显示原值」的格子上出现;
// 演示模式下不能显示原值。

import { t } from "nsh/i18n";
import { grid, mask } from "../lib/native";
import { type MaskKind, type QueryResult, supportsSql } from "../lib/types";
import { maskSettings, privacy, setMaskEnabled, setMaskOverride } from "../store/privacy";
import { toast } from "../store/toast";
import { openCell, viewIndices, viewRow, type TabModel } from "./model";

/** 抽样识别看的行数(原生侧每列最多看 50 个非空值)。 */
const SAMPLE_ROWS = 100;

const planCache = new WeakMap<QueryResult, { rev: number; table: string | null; plan: (MaskKind | null)[] }>();

/** 手动覆盖匹配用的来源表:表浏览为表名;查询 tab 为单表查询猜出的表名(带 JOIN 的不猜)。 */
export function sourceTable(m: TabModel): string | null {
  if (m.table) return m.table;
  const last = m.lastRun();
  if (!last) return null;
  try {
    return mask.sourceTable(last.sql);
  } catch {
    return null;
  }
}

/** 当前结果各列的遮罩方式(null 不遮罩);设置变化后重算。 */
export function maskPlan(m: TabModel): (MaskKind | null)[] {
  const result = m.result();
  if (!result) return [];
  const rev = privacy.rev();
  const table = sourceTable(m);
  const hit = planCache.get(result);
  if (hit && hit.rev === rev && hit.table === table) return hit.plan;
  let plan: (MaskKind | null)[];
  try {
    plan = mask.plan(m.connId, table, result.columns, result.rows.slice(0, SAMPLE_ROWS));
  } catch {
    // 原生侧出错时宁可多遮:不放行原值
    plan = result.columns.map(() => "secret");
  }
  planCache.set(result, { rev, table, plan });
  return plan;
}

/** 某列的遮罩方式(null 不遮罩)。 */
export function columnMask(m: TabModel, col: number): MaskKind | null {
  return maskPlan(m)[col] ?? null;
}

const cellKey = (rowIx: number, col: number) => `${rowIx}:${col}`;

/** 这个格子点过「显示原值」(演示模式下一律视为没有)。 */
export function isRevealed(m: TabModel, viewIx: number, col: number): boolean {
  if (privacy.demo()) return false;
  const r = m.revealed();
  const ri = viewIndices(m)[viewIx];
  return r.of === m.result() && ri !== undefined && r.keys.includes(cellKey(ri, col));
}

/** 格子当前是遮罩状态(列遮罩、值非空、没点显示原值)。 */
export function isMaskedCell(m: TabModel, viewIx: number, col: number): boolean {
  const v = viewRow(m, viewIx)?.[col];
  return v !== null && v !== undefined && columnMask(m, col) !== null && !isRevealed(m, viewIx, col);
}

/** 格子的展示值:遮罩列给遮罩后的值,点过显示原值的给原值;NULL 仍是 NULL。 */
export function displayValue(m: TabModel, viewIx: number, col: number): string | null {
  const v = viewRow(m, viewIx)?.[col] ?? null;
  const kind = columnMask(m, col);
  if (v === null || kind === null || isRevealed(m, viewIx, col)) return v;
  try {
    return mask.apply(kind, v);
  } catch {
    return "••••••";
  }
}

/** 一整行的展示值(复制行、复制 INSERT、行详情)。 */
export function displayRow(m: TabModel, viewIx: number): (string | null)[] {
  const n = m.result()?.columns.length ?? 0;
  return Array.from({ length: n }, (_, c) => displayValue(m, viewIx, c));
}

/** 导出用:敏感列一律遮罩(不看单个格子是否显示过原值)。 */
export function maskedRows(m: TabModel, rows: (string | null)[][]): (string | null)[][] {
  const plan = maskPlan(m);
  if (!plan.some((k) => k !== null)) return rows;
  return mask.maskRows(plan, rows);
}

/** 当前结果有没有遮罩的列。 */
export function hasMaskedColumns(m: TabModel): boolean {
  return maskPlan(m).some((k) => k !== null);
}

/** 「显示原值」:演示模式下拒绝。返回是否显示了。 */
export function revealCell(m: TabModel, viewIx: number, col: number): boolean {
  if (privacy.demo()) {
    toast.info(t("mask.reveal_disabled_demo"));
    return false;
  }
  const ri = viewIndices(m)[viewIx];
  if (ri === undefined) return false;
  const cur = m.revealed();
  const keys = cur.of === m.result() ? cur.keys : [];
  m.revealed.set({ of: m.result(), keys: [...keys, cellKey(ri, col)] });
  return true;
}

/** 列的手动覆盖键:`表.列`;不知道来源表时只用列名(对所有表生效)。 */
export function overrideKey(m: TabModel, col: number): string {
  const name = m.result()?.columns[col] ?? "";
  const table = sourceTable(m);
  return table ? `${table}.${name}` : name;
}

/** 表头右键「遮罩此列 / 取消遮罩」。遮罩关着的连接上手动遮罩一列时顺手打开遮罩。 */
export function toggleColumnMask(m: TabModel, col: number) {
  const key = overrideKey(m, col);
  if (columnMask(m, col) !== null) {
    if (privacy.demo()) {
      toast.info(t("mask.unmask_disabled_demo"));
      return;
    }
    setMaskOverride(m.connId, key, "plain");
    toast.success(t("mask.column_unmasked", { column: key }));
  } else {
    setMaskOverride(m.connId, key, "mask");
    if (!maskSettings(m.connId).enabled) setMaskEnabled(m.connId, true);
    toast.success(t("mask.column_masked", { column: key }));
  }
}

// ------------------------------------------------------------ 打开与复制(都用展示值)

/** 双击 / 右键「编辑 | 查看」:遮罩的格子弹窗里先显示遮罩值(只读),点「显示原值」后才是原值、才能修改。 */
export function openCellView(m: TabModel, viewIx: number, col: number) {
  openCell(m, viewIx, col);
  const c = m.cell();
  if (!c || !isMaskedCell(m, viewIx, col)) return;
  m.cell.set({ ...c, value: displayValue(m, viewIx, col), masked: columnMask(m, col) });
}

/** 弹窗里「显示原值」:记下这个格子,重新打开成原值。 */
export function revealOpenCell(m: TabModel) {
  const c = m.cell();
  if (!c || !revealCell(m, c.viewIx, c.colIx)) return;
  openCellView(m, c.viewIx, c.colIx);
}

/** 复制一个格子的展示值。 */
export async function copyValueDisplayed(m: TabModel, viewIx: number, col: number, write: (text: string) => Promise<unknown>) {
  await write(displayValue(m, viewIx, col) ?? "NULL");
  toast.success(t("tab.copied"));
}

/** 复制一行(CSV,带表头)。 */
export async function copyRowDisplayed(m: TabModel, viewIx: number, write: (text: string) => Promise<unknown>) {
  const result = m.result();
  if (!result || !viewRow(m, viewIx)) return;
  await write(grid.toCsv(result.columns, [displayRow(m, viewIx)]));
  toast.success(t("tab.copied"));
}

/** 复制一行的 INSERT(展示值:遮罩的列是遮罩后的值)。 */
export async function copyInsertDisplayed(m: TabModel, viewIx: number, write: (text: string) => Promise<unknown>) {
  const result = m.result();
  if (!result || !viewRow(m, viewIx) || !supportsSql(m.dbKind)) return;
  await write(grid.buildInsertRow(m.table ?? "table", result.columns, displayRow(m, viewIx), m.dbKind));
  toast.success(t("tab.copied"));
}
