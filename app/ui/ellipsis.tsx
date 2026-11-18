// 中间省略文本(GPUI 版 ui.rs 的 MiddleEllipsis):超宽时在中间截断显示 …,开头和结尾都可见。
// 例如 very_long_table_name_suffix → very_lon…e_suffix。截断交给 Text 的 textOverflow: "ellipsis-middle"。

import { Text } from "nsh";

/** 中间省略的单行文本。`class` 给文字的字号 / 颜色 / 字重。 */
export function MiddleEllipsis(props: { text: string; class?: string }) {
  return (
    <Text class={`min-w-0 flex-1 line-clamp-1 ${props.class ?? ""}`} style={{ textOverflow: "ellipsis-middle" }}>
      {props.text}
    </Text>
  );
}
