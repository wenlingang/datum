// 已配置:发送后流式回复(含 SQL 卡片)。Done 不带全文,最终文本靠 Delta 拼接。
import { chunks, REPLY } from "../fixtures";
import { mountChat } from "./harness";

mountChat({
  delay: 5,
  events: () => [...chunks(REPLY).map((text) => ({ type: "delta" as const, text })), { type: "done" as const, full: "" }],
});
