// 流式中途出错:错误追加在已收到的文本之后。
import { mountChat } from "./harness";

mountChat({
  delay: 5,
  events: () => [
    { type: "delta", text: "部分回复" },
    { type: "error", message: "network down" },
  ],
});
