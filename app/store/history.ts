// 查询历史(最新在前),落盘在 Rust 侧。记录失败静默:历史绝不影响查询流程。
// 写语句的撤销(反向语句)只留在内存里、跟着历史条目走:本次运行期间都能从历史里撤销;
// 不落盘——里面是改动前的整行数据,写进配置目录既占地方又多一份敏感数据。

import { signal } from "nsh";
import { store } from "../lib/native";
import type { HistoryEntry, UndoPrep } from "../lib/types";

const entries = signal<HistoryEntry[]>(store.history());
/** 历史条目 id → 可用的撤销 */
const undos = signal<Map<string, UndoPrep>>(new Map());

export const history = () => entries();

/** 历史条目还能撤销时返回撤销内容。 */
export const historyUndo = (id: string): UndoPrep | undefined => undos().get(id);

/** 撤销执行过了:从历史里拿掉(同一份撤销不能做两次)。 */
export function forgetUndo(undo: UndoPrep) {
  const gone = new Set([undo, ...(undo.parts ?? [])]);
  const next = new Map(undos());
  for (const [id, u] of next) if (gone.has(u)) next.delete(id);
  undos.set(next);
}

export function recordHistory(entry: Omit<HistoryEntry, "id">, undo?: UndoPrep | null) {
  try {
    const list = store.recordHistory(entry);
    if (!list) return;
    entries.set(list);
    // 新条目在最前(重复执行同一条时原地更新,id 不变):撤销跟最近这次走
    const head = list[0];
    if (head && head.sql === entry.sql) {
      const next = new Map(undos());
      if (undo && undo.statements.length > 0) next.set(head.id, undo);
      else next.delete(head.id);
      // 被挤出历史的条目不再保留撤销
      for (const id of next.keys()) if (!list.some((e) => e.id === id)) next.delete(id);
      undos.set(next);
    }
  } catch {
    // 历史写盘失败不打扰用户
  }
}

/** `connId` 给定时只清该连接的历史。 */
export function clearHistory(connId: string | null) {
  try {
    const list = store.clearHistory(connId);
    entries.set(list);
    undos.set(new Map([...undos()].filter(([id]) => list.some((e: HistoryEntry) => e.id === id))));
  } catch {
    // 同上
  }
}
