// 全局提示(右下角的通知条)。任何地方调用 toast.success / error / info。
// nsh/base 的 ToastManager 管生命周期(进场 → 显示倒计时 → 退场 → 移除),
// 这里用定时器推进它并把可见列表同步到 signal,App.tsx 的 ToastStack 渲染。

import { signal } from "nsh";
import { ToastManager, type ToastEntry } from "nsh/base";

export type ToastKind = "success" | "error" | "info";

export interface ToastItem {
  kind: ToastKind;
  text: string;
}

/** 一条挂载中的提示:id、生命周期阶段(给 Toast 的 transitionStatus)和内容。 */
export interface ShownToast extends ToastItem {
  id: string;
  status: ToastEntry["status"];
}

/** 同时显示的上限(更早的直接退场)。 */
const LIMIT = 5;
/** 推进生命周期的间隔(毫秒),只在有提示时运行。 */
const TICK = 50;
/** 显示时长:错误 10 秒,其余 3 秒(悬停在提示上时暂停计时)。 */
const TIMEOUT: Record<ToastKind, number> = { success: 3000, error: 10000, info: 3000 };

const manager = new ToastManager();
const shown = signal<ShownToast[]>([]);
const expanded = signal(false);
let ticking: ReturnType<typeof setInterval> | undefined;
let nextId = 1;

/** 当前挂载的提示(从旧到新,含正在退场的)。 */
export const toasts = () => shown();

function sync() {
  shown.set(manager.visible(LIMIT).map((e) => ({ id: e.id, status: e.status, ...(e.value as ToastItem) })));
}

function tick() {
  if (manager.advance(expanded()).changed) sync();
  if (manager.isEmpty() && ticking !== undefined) {
    clearInterval(ticking);
    ticking = undefined;
  }
}

function run() {
  sync();
  ticking ??= setInterval(tick, TICK);
}

function push(kind: ToastKind, text: string) {
  manager.push(String(nextId++), { kind, text } satisfies ToastItem, { timeout: TIMEOUT[kind] });
  // 超过上限时最早的退场
  const live = manager.entries().filter((e) => e.status !== "ending");
  for (const e of live.slice(0, Math.max(0, live.length - LIMIT))) manager.dismiss(e.id);
  run();
}

export function dismissToast(id: string) {
  manager.dismiss(id);
  run();
}

/** Esc:关掉最新的一条。 */
export function dismissNewestToast() {
  const newest = shown().filter((e) => e.status !== "ending").at(-1);
  if (newest) dismissToast(newest.id);
}

/** 栈展开(指针停在上面)时暂停计时,方便阅读和选中复制。 */
export function setToastsExpanded(v: boolean) {
  expanded.set(v);
}

/** 栈是否展开(指针停在提示上)。 */
export const toastsExpanded = () => expanded();

export const toast = {
  success: (text: string) => push("success", text),
  error: (text: string) => push("error", text),
  info: (text: string) => push("info", text),
};
