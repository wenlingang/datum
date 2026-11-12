// 回复里有破坏性 SQL 与只读 SQL:风险标记、执行确认、只读语句直接进编辑器。
import { mountChat } from "./harness";

const REPLY = "删表:\n```sql\nDROP TABLE users;\n```\n查询:\n```sql\nSELECT * FROM orders;\n```";

mountChat({
  delay: 5,
  events: () => [
    { type: "delta", text: REPLY },
    { type: "done", full: REPLY },
  ],
  // SELECT 带回结果集(在对话里直接成表);其余语句只报影响行数
  queryResult: (sql) =>
    sql.trim().toUpperCase().startsWith("SELECT")
      ? { columns: ["id", "total"], rows: [["1", "9.5"], ["2", "12"]], affected: null }
      : { columns: [], rows: [], affected: 0 },
  // DROP TABLE:表中现有 7 行
  impact: (sql) =>
    sql.toUpperCase().includes("DROP")
      ? { count: 7, wholeTable: true, columns: ["id"], rows: [["1"], ["2"]] }
      : null,
});
