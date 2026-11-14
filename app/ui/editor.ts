// nsh/base 的 Editor 作为「可编辑、由应用持有文本」时的接法。
//
// 不要用受控写法(`value={x()}` + `onChange` 里 `x.set(value)`):原生侧打字快于 JS 处理 onChange 时,
// 写回去的是过时的文本,Editor 会把它当新值替换掉已经打出来的内容、光标跳回去,后面的字插错位置
// (真实窗口里可复现,无头测试测不出:见 *.native.test.tsx)。
// 这里改为不受控:初值经 defaultValue 给出,用户的输入只经 onChange 回报;应用这边的文本
// 与编辑器最近回报的不同(程序改的:格式化、恢复、流式增长)时,才经 setValue 推给编辑器。

import { effect, untracked } from "nsh";
import type { EditorRef } from "nsh/base";

/** 在组件里调用,把返回的 `ref` / `defaultValue` / `onChange` 交给 `<Editor>`。
 *  macOS 智能引号 / 破折号:nsh 0.5.1 起 Editor(autoCorrect 关闭)不再替换敲入的 ' " --,这里不用再纠正。 */
export function editorText(value: () => string, onChange: (value: string) => void) {
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
      reported = v;
      onChange(v);
    },
  };
}
