// 从写操作记录撤销:取回完整记录里的反向语句,组成一份撤销,再在后台核对现在的行
// (之前运行的记录、之后又改了同一张表、要恢复的行还在不在 / 有没有被插回来)。

import { t } from "nsh/i18n";
import { errorText, journal } from "../lib/native";
import type { DbKind, UndoCheck, UndoPrep } from "../lib/types";
import { toast } from "../store/toast";

/** 取回记录的撤销;没有撤销数据或已撤销时提示并返回 null。 */
export async function journalUndoPrep(connId: string, id: string): Promise<UndoPrep | null> {
  try {
    const d = await journal.get(connId, id);
    if (!d) {
      toast.error(t("journal.gone"));
      return null;
    }
    if (d.undoneBy) {
      toast.info(t("journal.already_undone"));
      return null;
    }
    const u = d.entry.undo;
    if (!u || u.statements.length === 0) {
      toast.info(t("undo.unavailable", { reason: d.entry.undoReason ?? "" }));
      return null;
    }
    return {
      statements: u.statements,
      rows: u.rows,
      reason: null,
      snapshots: u.snapshots,
      journalId: id,
      journal: { check: null, checking: true },
    };
  } catch (e) {
    toast.error(errorText(e));
    return null;
  }
}

/** 后台核对现在的行,结果写回(`set` 收到带核对结果的新撤销)。 */
export function checkJournalUndo(kind: DbKind, connId: string, u: UndoPrep, set: (next: UndoPrep) => void) {
  const id = u.journalId;
  if (!id) return;
  const failed = (error: string): UndoCheck => ({
    previousSession: false,
    laterWrites: 0,
    undoneBy: null,
    checked: false,
    missing: 0,
    existing: 0,
    error,
  });
  journal
    .undoCheck(kind, connId, id)
    .then((check) => set({ ...u, journal: { check, checking: false } }))
    .catch((e) => set({ ...u, journal: { check: failed(errorText(e)), checking: false } }));
}
