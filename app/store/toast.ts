// 全局提示(右下角的通知条)。任何地方调用 toast.success / error / info。

import { signal } from "nsh";

export type ToastKind = "success" | "error" | "info";

export interface ToastItem {
  id: number;
  kind: ToastKind;
  text: string;
}

const items = signal<ToastItem[]>([]);
let nextId = 1;

/** 当前显示的提示(最新在后)。 */
export const toasts = () => items();

function push(kind: ToastKind, text: string) {
  const id = nextId++;
  items.update((l) => [...l.slice(-4), { id, kind, text }]);
  // 错误多停留一会儿,便于读完
  setTimeout(() => dismissToast(id), kind === "error" ? 6000 : 3000);
}

export function dismissToast(id: number) {
  items.update((l) => l.filter((t) => t.id !== id));
}

export const toast = {
  success: (text: string) => push("success", text),
  error: (text: string) => push("error", text),
  info: (text: string) => push("info", text),
};
