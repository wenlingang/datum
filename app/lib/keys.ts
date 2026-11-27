// 组件聚焦期间接管快捷键:压过聚焦组件自己的同名键位(如 Editor 的 mod+enter 下方插入新行)。
// 用 nsh 的 keymap.bind `overFocused`:这类绑定排在视图自己的绑定之后,聚焦组件绑了同一个键也是它赢。
// 只在 `focused()` 为 true 时绑,失焦或组件卸载即解绑,组件在别处仍保留这些键。

import { effect, keymap } from "nsh";

/** 在组件函数里调用一次:`focused()` 期间按下 `keys` 里的键时调用 `onKey(keystroke)`(按写法原样给出)。 */
export function bindWhileFocused(focused: () => boolean, keys: readonly string[], onKey: (keystroke: string) => void) {
  effect(() => {
    if (!focused()) return;
    keymap.bind(Object.fromEntries(keys.map((k) => [k, () => onKey(k)])), {
      when: "always",
      repeat: false,
      overFocused: true,
    });
  });
}
