// nsh/base 的 Editor 作为「可编辑、由应用持有文本」时的接法。
//
// 不要用受控写法(`value={x()}` + `onChange` 里 `x.set(value)`):原生侧打字快于 JS 处理 onChange 时,
// 写回去的是过时的文本,Editor 会把它当新值替换掉已经打出来的内容、光标跳回去,后面的字插错位置
// (真实窗口里可复现,无头测试测不出:见 *.native.test.tsx)。
// 这里改为不受控:初值经 defaultValue 给出,用户的输入只经 onChange 回报;应用这边的文本
// 与编辑器最近回报的不同(程序改的:格式化、恢复、流式增长)时,才经 setValue 推给编辑器。

import { effect, untracked } from "nsh";
import type { EditorRef } from "nsh/base";
import { rangeOf } from "../tab/textpos";

const SMART_QUOTES: Record<string, string> = { "\u2018": "'", "\u2019": "'", "\u201C": '"', "\u201D": '"' };

/** macOS 开着「智能引号」时,编辑器跟随系统把刚敲的 ' " 换成弯引号,SQL 里会被当成标识符而出错。
 *  `prev` → `value` 恰好多了一个弯引号(逐键输入,不是粘贴)时,返回它的位置与要换成的直引号。 */
export function typedSmartQuote(prev: string, value: string): { at: number; text: string } | null {
  if (value.length !== prev.length + 1) return null;
  let i = 0;
  while (i < prev.length && prev.charCodeAt(i) === value.charCodeAt(i)) i++;
  const straight = SMART_QUOTES[value[i] ?? ""];
  if (!straight || value.slice(i + 1) !== prev.slice(i)) return null;
  return { at: i, text: straight };
}

/** 把刚敲进编辑器的弯引号改回直引号(只改这一个字符,光标不动)。改了返回 true。 */
export function straightenTypedQuote(editor: EditorRef | undefined, prev: string, value: string): boolean {
  const q = typedSmartQuote(prev, value);
  if (!q || !editor) return false;
  editor.edit([{ range: rangeOf(value, q.at, q.at + 1), text: q.text }]);
  return true;
}

/** 在组件里调用,把返回的 `ref` / `defaultValue` / `onChange` 交给 `<Editor>`。
 *  `sql`:编辑的是 SQL,刚敲进去的弯引号改回直引号。 */
export function editorText(value: () => string, onChange: (value: string) => void, opts: { sql?: boolean } = {}) {
  let ref: EditorRef | undefined;
  let reported = untracked(value);
  effect(() => {
    const v = value();
    if (v !== reported) {
      reported = v;
      ref?.setValue(v);
    }
  });
  return {
    ref: (r: EditorRef) => {
      ref = r;
    },
    defaultValue: reported,
    onChange: ({ value: v }: { value: string }) => {
      const prev = reported;
      reported = v;
      onChange(v);
      if (opts.sql) straightenTypedQuote(ref, prev, v);
    },
  };
}
