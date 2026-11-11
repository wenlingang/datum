// 回复里有破坏性 SQL 与只读 SQL:风险标记、执行确认、只读语句直接进编辑器。
import { mountChat } from "./harness";

const REPLY = "删表:\n```sql\nDROP TABLE users;\n```\n查询:\n```sql\nSELECT * FROM orders;\n```";

mountChat({
  delay: 5,
  events: () => [
    { type: "delta", text: REPLY },
    { type: "done", full: REPLY },
  ],
  queryResult: () => ({ columns: [], rows: [], affected: 0 }),
});
