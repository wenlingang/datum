// 中间省略文本(GPUI 版 ui.rs 的 MiddleEllipsis):头部超宽时截断显示 …,尾部 `tail` 个字符始终可见。
// 例如 very_long_table_name_suffix → very_long_t…suffix。

import { Text, View } from "nsh";

/** 把文本拆成「可截断的头部」与「始终可见的尾部」;短文本尾部为空。 */
export function splitTail(text: string, tail = 6): [string, string] {
  const chars = Array.from(text);
  if (chars.length <= tail + 1) return [text, ""];
  return [chars.slice(0, chars.length - tail).join(""), chars.slice(chars.length - tail).join("")];
}

/** 中间省略的单行文本。`class` 给两段文字共用的字号 / 颜色 / 字重。 */
export function MiddleEllipsis(props: { text: string; tail?: number; class?: string }) {
  const parts = () => splitTail(props.text, props.tail ?? 6);
  return (
    <View class="min-w-0 flex-1 flex-row items-center overflow-hidden">
      <Text class={`line-clamp-1 min-w-0 shrink ${props.class ?? ""}`}>
        {parts()[0]}
      </Text>
      <If when={parts()[1]}>
        <Text class={`line-clamp-1 shrink-0 ${props.class ?? ""}`}>
          {parts()[1]}
        </Text>
      </If>
    </View>
  );
}
