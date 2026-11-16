// 查询历史(最新在前),落盘在 Rust 侧。记录失败静默:历史绝不影响查询流程。
// 写语句的撤销:本次运行期间的放在内存里、跟着历史条目走;撤销数据同时进写操作记录(写日志),
// 历史条目记着那条记录的 id —— 重启之后从写操作记录里找回撤销数据,照样能从历史里撤销。

import { signal } from "nsh";
import { journal, store } from "../lib/native";
import type { HistoryEntry, UndoPrep } from "../lib/types";

const entries = signal<HistoryEntry[]>(store.history());
/** 历史条目 id → 可用的撤销 */
const undos = signal<Map<string, UndoPrep>>(new Map());

export const history = () => entries();

/** 写操作记录里还能撤销的条目:连接 id → (记录 id → 会恢复的行数)。 */
const persisted = signal<Map<string, Map<string, number>>>(new Map());
/** 本次运行里已经撤销过的记录 id(写日志的撤销记录还没落盘时也不再提供撤销) */
const undoneIds = signal<Set<string>>(new Set());

/** 历史条目还能撤销时返回撤销内容(本次运行期间的)。 */
export const historyUndo = (id: string): UndoPrep | undefined => undos().get(id);

/** 历史条目在写操作记录里还能撤销(之前运行的):返回会恢复的行数。 */
export function historyJournalUndo(e: HistoryEntry): number | undefined {
  if (undos().has(e.id) || !e.journalId || undoneIds().has(e.journalId)) return undefined;
  return persisted().get(e.connId)?.get(e.journalId);
}

/** 重读某连接在写操作记录里还能撤销的条目(打开历史浮层时)。 */
export async function loadJournalUndo(connId: string) {
  try {
    const list = await journal.undoable(connId);
    const next = new Map(persisted());
    next.set(connId, new Map(list));
    persisted.set(next);
  } catch {
    // 写日志读不了:只是不显示重启前的撤销
  }
}

/** 撤销执行过了:从历史里拿掉(同一份撤销不能做两次)。按对象或写操作记录 id 认。 */
export function forgetUndo(undo: UndoPrep) {
  const gone = new Set([undo, ...(undo.parts ?? [])]);
  const ids = new Set([...gone].map((u) => u.journalId).filter((x): x is string => !!x));
  const next = new Map(undos());
  for (const [id, u] of next) {
    const uids = [u, ...(u.parts ?? [])].map((x) => x.journalId);
    if (gone.has(u) || uids.some((x) => x && ids.has(x))) next.delete(id);
  }
  undos.set(next);
  if (ids.size > 0) {
    undoneIds.set(new Set([...undoneIds(), ...ids]));
    const p = new Map(persisted());
    for (const [conn, m] of p) {
      if ([...ids].some((x) => m.has(x))) p.set(conn, new Map([...m].filter(([x]) => !ids.has(x))));
    }
    persisted.set(p);
  }
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
