// 模型不支持工具:带工具的对话一开始就报错,自动改用普通对话。
import { mountChat } from "./harness";

mountChat({ delay: 5, agentError: "tools are not supported by this model" });
