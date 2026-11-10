// 开发入口:`DATUM_CONFIG_DIR=<临时目录> nsh run chat/dev.tsx`。装上假的 AI 与连接,
// 自动发两条消息,看面板(流式回复、SQL 卡片、引用、模型切换)。明暗跟随该目录下 ui_prefs.json
// 的 theme(缺省为 dark)。nsh 只在入口所在目录找 nsh.json,所以要从 app/ 根目录的入口导入本文件启动。

import { setPendingRef } from "../store/ai";
import { apply as applyPrefs } from "../store/prefs";
import { sendText } from "./actions";
import { REPLY } from "./fixtures";
import { mountChat } from "./testing/harness";

applyPrefs();

const DANGER = "也可以先清理旧表:\n\n```sql\nDROP TABLE IF EXISTS users_old;\nDELETE FROM sessions;\n```";

mountChat(
  {
    delay: 40,
    events: (messages) => {
      const last = messages[messages.length - 1]?.content ?? "";
      const text = last.includes("清理") ? DANGER : REPLY;
      const parts: { type: "delta"; text: string }[] = [];
      for (let i = 0; i < text.length; i += 6) parts.push({ type: "delta", text: text.slice(i, i + 6) });
      return [...parts, { type: "done", full: text }];
    },
  },
  () => {
    setTimeout(() => {
      sendText("新建一张 users 表,含 id、name、email");
      setTimeout(() => {
        sendText("再帮我清理旧数据");
        setTimeout(() => setPendingRef("SELECT id, name\nFROM users\nWHERE email IS NULL;"), 4000);
      }, 4000);
    }, 300);
  },
);
