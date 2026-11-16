// 写操作记录(写日志):Datum 执行的每次写操作记一条,落盘在 Rust 侧(journal/<连接 id>.jsonl)。
// 记录失败静默:留痕绝不影响执行流程。写日志的内容从不交给 AI。

import { signal } from "nsh";
import { journal } from "../lib/native";
import type { JournalSettings, JournalSource, UndoPrep } from "../lib/types";
import { conns } from "./conns";

/** 每追加一条加一:写操作记录标签页、查询历史的「撤销」据此重读。 */
const rev = signal(0);
export const journalRev = () => rev();
export const bumpJournal = () => rev.update((n) => n + 1);

/** 新记录 id(先拿到 id,查询历史与撤销都能指向它)。 */
export function newJournalId(): string {
  try {
    return journal.newId();
  } catch {
    return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  }
}

export interface WriteRecord {
  connId: string;
  source: JournalSource;
  sql: string;
  ok: boolean;
  affected?: number | null;
  error?: string | null;
  elapsedMs?: number;
  /** 执行前抓的撤销(有反向语句的才存行数据) */
  undo?: UndoPrep | null;
  /** 这条是撤销:撤销的是哪几条记录 */
  undoes?: string[];
  /** 一次执行多条语句时同属一批 */
  batch?: string | null;
  /** 事先拿好的 id */
  id?: string;
}

/** 记一条写操作,返回记录 id(写日志关掉时这条不会落盘,id 照样返回)。撤销对象上记下记录 id。 */
export function recordWrite(r: WriteRecord): string {
  const id = r.id ?? newJournalId();
  const c = conns.get(r.connId);
  const usable = r.undo && r.undo.statements.length > 0 ? r.undo : null;
  if (usable) usable.journalId = id;
  const entry = {
    id,
    connId: r.connId,
    connName: c?.name ?? "",
    env: c?.env ?? "",
    kind: c?.kind ?? "",
    database: c ? (c.kind === "sqlite" ? (c.filePath ?? "") : c.database) : "",
    source: r.source,
    batch: r.batch ?? null,
    sql: r.sql,
    ok: r.ok,
    affected: r.affected ?? null,
    error: r.error ?? null,
    elapsedMs: Math.max(0, Math.round(r.elapsedMs ?? 0)),
    undo: usable ? { statements: usable.statements, rows: usable.rows, snapshots: usable.snapshots ?? [] } : null,
    undoReason: r.undo && !usable ? (r.undo.reason ?? null) : null,
    undoes: r.undoes ?? [],
  };
  void journal
    .record(entry)
    .then(() => bumpJournal())
    .catch(() => {});
  return id;
}

/** 撤销对应的记录 id(合成的整体撤销取各部分的)。 */
export function journalIdsOf(undo: UndoPrep): string[] {
  const parts = undo.parts && undo.parts.length > 0 ? undo.parts : [undo];
  return parts.map((p) => p.journalId).filter((x): x is string => !!x);
}

const DEFAULT_SETTINGS: JournalSettings = { enabled: true, storeRows: true, retentionDays: 90 };

export function journalSettings(): JournalSettings {
  try {
    return journal.settings();
  } catch {
    return DEFAULT_SETTINGS;
  }
}

export function saveJournalSettings(s: JournalSettings): JournalSettings {
  return journal.saveSettings(s);
}
