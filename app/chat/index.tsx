// chat/ 对外接口。
// 【桩】由「AI 助手」子任务实现;签名是约定,改动需同步调用方。

import { View } from "nsh";

/** 右侧 AI 助手面板。 */
export function ChatPanel() {
  return <View class="flex-1" />;
}

/** 把一段 SQL 作为引用发给 AI(编辑器选中 ⌘⇧A、结果右键)。会打开 AI 面板。 */
export function sendToAi(text: string) {
  void text;
}
