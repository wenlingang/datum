// 已配置 + 对话持久化:和应用入口一样,启动时读回保存的对话、之后有变化就保存。
import { loadConversations, persistConversations } from "../../store/ai";
import { chunks, REPLY } from "../fixtures";
import { mountChat } from "./harness";

mountChat({
  delay: 5,
  events: () => [...chunks(REPLY).map((text) => ({ type: "delta" as const, text })), { type: "done" as const, full: "" }],
});
loadConversations();
persistConversations();
