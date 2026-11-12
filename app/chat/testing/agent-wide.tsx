// 对话工作台(wide)里的 AI 自己查数据:回复里先说一句、调用一次只读查询、再给出结论与一条待确认的写语句。
import { mountChat } from "./harness";

const SQL = "SELECT status, COUNT(*) AS n FROM orders GROUP BY status";

mountChat({
  wide: true,
  delay: 5,
  agentEvents: () => [
    { type: "delta", text: "我先查一下订单状态分布。" },
    { type: "toolStart", id: "c1", name: "run_query", args: { sql: SQL } },
    {
      type: "toolEnd",
      id: "c1",
      ok: true,
      summary: "2 行",
      detail: { sql: SQL, columns: ["status", "n"], rows: [["paid", "42"], ["cancelled", "3"]], capped: false },
    },
    { type: "toolStart", id: "c2", name: "run_query", args: { sql: "SELECT * FROM nope" } },
    { type: "toolEnd", id: "c2", ok: false, summary: "no such table: nope", detail: { sql: "SELECT * FROM nope", error: "no such table: nope" } },
    { type: "delta", text: "\n已支付 42 单,取消 3 单。要清理取消的订单可以执行:\n\n```sql\nDELETE FROM orders WHERE status = 'cancelled';\n```" },
    { type: "done", full: "" },
  ],
});
