// 回复很慢(便于点「停止」);每次请求的回复带上第几次,用来验证「重新生成」。
import { chunks } from "../fixtures";
import { mountChat } from "./harness";

let n = 0;
mountChat({
  delay: 40,
  events: () => {
    n++;
    const text = `第 ${n} 次回答:${"内容".repeat(40)}`;
    return [...chunks(text, 4).map((t) => ({ type: "delta" as const, text: t })), { type: "done" as const, full: text }];
  },
  agentEvents: () => {
    n++;
    const text = `第 ${n} 次回答:${"内容".repeat(40)}`;
    return [...chunks(text, 4).map((t) => ({ type: "delta" as const, text: t })), { type: "done" as const, full: text }];
  },
});
