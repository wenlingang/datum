// 查询历史(最新在前),落盘在 Rust 侧。记录失败静默:历史绝不影响查询流程。

import { signal } from "nsh";
import { store } from "../lib/native";
import type { HistoryEntry } from "../lib/types";

const entries = signal<HistoryEntry[]>(store.history());

export const history = () => entries();

export function recordHistory(entry: Omit<HistoryEntry, "id">) {
  try {
    const list = store.recordHistory(entry);
    if (list) entries.set(list);
  } catch {
    // 历史写盘失败不打扰用户
  }
}

/** `connId` 给定时只清该连接的历史。 */
export function clearHistory(connId: string | null) {
  try {
    entries.set(store.clearHistory(connId));
  } catch {
    // 同上
  }
}
