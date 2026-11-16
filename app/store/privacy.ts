// 敏感数据遮罩与演示模式的界面状态。规则与设置的读写都在原生侧(datum-core 的 mask 模块),
// 这里只记一个版本号:设置一变就加一,网格等读到它的地方重新算遮罩方式。

import { signal } from "nsh";
import { errorText, mask } from "../lib/native";
import type { MaskOverride, MaskSettingsView } from "../lib/types";
import { toast } from "./toast";

const rev = signal(0);
const demo = signal(readDemo());

function readDemo(): boolean {
  try {
    return mask.demoMode();
  } catch {
    return false;
  }
}

/** 演示时代替主机、端口、用户名、文件路径的占位。 */
export const HIDDEN = "•••";

export const privacy = {
  /** 设置版本号:读它的计算在设置变化后重算。 */
  rev: () => rev(),
  /** 演示模式:强制遮罩、不能显示原值,界面隐藏连接细节。 */
  demo: () => demo(),
};

/** 演示模式下把连接细节(主机、端口、用户名、文件路径)换成占位。 */
export function hideInDemo(text: string): string {
  return demo() && text ? HIDDEN : text;
}

function changed(f: () => void) {
  try {
    f();
  } catch (e) {
    toast.error(errorText(e));
  }
  rev.update((n) => n + 1);
}

export function setDemoMode(on: boolean) {
  changed(() => mask.setDemoMode(on));
  demo.set(readDemo());
}

export function toggleDemoMode() {
  setDemoMode(!demo());
}

/** 连接的遮罩设置(随版本号重读)。 */
export function maskSettings(connId: string): MaskSettingsView {
  rev();
  try {
    return mask.settings(connId);
  } catch {
    return { enabled: true, explicit: null, autoDetect: true, overrides: {}, demo: demo() };
  }
}

export function setMaskEnabled(connId: string, enabled: boolean | null) {
  changed(() => mask.setEnabled(connId, enabled));
}

export function setMaskAutoDetect(connId: string, on: boolean) {
  changed(() => mask.setAutoDetect(connId, on));
}

export function setMaskOverride(connId: string, key: string, mode: MaskOverride | null) {
  changed(() => mask.setOverride(connId, key, mode));
}

/**
 * 已知来源表的一批行(数据对比的两侧、写操作记录的改动前数据):按连接规则遮罩敏感列;随设置变化重算。
 * 行可以是 null(这一侧没有这行),原样保留。
 */
export function maskRowsOf(
  connId: string | null | undefined,
  table: string | null,
  columns: string[],
  rows: ((string | null)[] | null)[],
): ((string | null)[] | null)[] {
  rev();
  const present = rows.filter((r): r is (string | null)[] => r !== null);
  if (!connId || present.length === 0) return rows;
  try {
    const plan = mask.plan(connId, table, columns, present.slice(0, 100));
    if (!plan.some((k) => k !== null)) return rows;
    const masked = mask.maskRows(plan, present);
    let i = 0;
    return rows.map((r) => (r === null ? null : masked[i++]));
  } catch {
    return rows.map((r) => (r === null ? null : r.map((v) => (v === null ? null : "••••••"))));
  }
}

/** 连接上一份结果的展示值(对话里的执行结果表等):敏感列遮罩;随设置变化重算。 */
export function maskTable(
  connId: string | null | undefined,
  columns: string[],
  rows: (string | null)[][],
  sql?: string | null,
): (string | null)[][] {
  rev();
  if (!connId || rows.length === 0) return rows;
  try {
    const table = sql ? mask.sourceTable(sql) : null;
    const plan = mask.plan(connId, table, columns, rows.slice(0, 100));
    return plan.some((k) => k !== null) ? mask.maskRows(plan, rows) : rows;
  } catch {
    return rows.map((r) => r.map((v) => (v === null ? null : "••••••")));
  }
}
