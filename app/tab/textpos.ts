// 编辑器位置(行 + UTF-16 列)与文本偏移(UTF-16)互转;编辑器的 `\r\n` 已归一为 `\n`。

import type { EditorPosition, EditorRange, EditorSelection } from "nsh/base";

export function offsetOf(text: string, pos: EditorPosition): number {
  let line = 0;
  let i = 0;
  while (line < pos.line) {
    const nl = text.indexOf("\n", i);
    if (nl < 0) return text.length;
    i = nl + 1;
    line++;
  }
  const end = text.indexOf("\n", i);
  const lineEnd = end < 0 ? text.length : end;
  return Math.min(i + pos.column, lineEnd);
}

export function positionOf(text: string, offset: number): EditorPosition {
  const o = Math.max(0, Math.min(offset, text.length));
  let line = 0;
  let start = 0;
  for (let i = 0; i < o; i++) {
    if (text.charCodeAt(i) === 10) {
      line++;
      start = i + 1;
    }
  }
  return { line, column: o - start };
}

export function rangeOf(text: string, start: number, end: number): EditorRange {
  return { start: positionOf(text, start), end: positionOf(text, end) };
}

/** 主选区(最后一个)的 [起, 止) 偏移,起 ≤ 止。 */
export function primaryRange(text: string, selections: EditorSelection[]): [number, number] | null {
  const s = selections[selections.length - 1];
  if (!s) return null;
  const a = offsetOf(text, s.anchor);
  const h = offsetOf(text, s.head);
  return a <= h ? [a, h] : [h, a];
}

/** 主选区光标(head)的偏移。 */
export function caretOf(text: string, selections: EditorSelection[]): number {
  const s = selections[selections.length - 1];
  return s ? offsetOf(text, s.head) : text.length;
}
