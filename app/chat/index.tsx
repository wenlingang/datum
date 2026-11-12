// chat/ 对外接口:AI 助手面板与「发给 AI」。实现见同目录的 ChatPanel / SqlCard / actions。

import { sendToAiPanel } from "./actions";
import { ChatPanel as Panel } from "./ChatPanel";

export { askAiExplainPlan, askAiFixError, planText } from "./actions";

/** 右侧 AI 助手面板。 */
export function ChatPanel(props: { wide?: boolean } = {}) {
  return <Panel wide={props.wide} />;
}

/** 把一段 SQL 作为引用发给 AI(编辑器选中 ⌘⇧A、结果右键)。会打开 AI 面板。 */
export function sendToAi(text: string) {
  sendToAiPanel(text);
}
