// 测试与开发入口共用的假数据:AI 配置、连接、模拟的流式回复。不碰真实配置、数据库与 AI 服务。

import type { AgentEvent, AiSettings, ChatEvent, Connection, QueryResult, UndoPrep, WriteImpact } from "../lib/types";
import { aiBackend, refreshAi } from "../store/ai";
import { toast } from "../store/toast";
import { chatDeps } from "./deps";

export const FAKE_CONN: Connection = {
  id: "c1",
  name: "local",
  kind: "pg",
  env: "local",
  host: "127.0.0.1",
  port: 5432,
  user: "dev",
  database: "shop",
  sslMode: "disable",
};

export const FAKE_SETTINGS: AiSettings = {
  providers: [
    {
      id: "p1",
      name: "OpenAI",
      protocol: "openai",
      baseUrl: "http://127.0.0.1:9/v1",
      apiKey: "sk-test",
      enabled: true,
      models: [
        { id: "gpt-4o", enabled: true },
        { id: "gpt-4o-mini", enabled: true },
      ],
    },
  ],
  current: { providerId: "p1", model: "gpt-4o" },
  knowledgeName: "",
  knowledgePath: "",
};

export const REPLY =
  "可以这样建表:\n\n```sql\nCREATE TABLE users (\n  id serial PRIMARY KEY,\n  name text NOT NULL,\n  email text UNIQUE\n);\n```\n\n然后**初始化**几条数据。";

/** 把文本切成若干块,模拟流式到达。 */
export function chunks(text: string, size = 12): string[] {
  const out: string[] = [];
  for (let i = 0; i < text.length; i += size) out.push(text.slice(i, i + size));
  return out;
}

export interface FakeOptions {
  /** 以对话工作台(铺满)的方式挂载面板 */
  wide?: boolean;
  ready?: boolean;
  /** 模拟回复;函数形式可按请求决定事件序列 */
  events?: (messages: { role: string; content: string }[]) => ChatEvent[];
  /** 执行写语句前的撤销准备;不给则 DROP / TRUNCATE 不能撤销,其余可撤销 2 行 */
  undo?: (sql: string) => UndoPrep;
  /** 撤销时执行过的语句 */
  undone?: string[][];
  /** 写语句的影响范围预估(确认框用);不给则统一回 3 行 */
  impact?: (sql: string) => WriteImpact | null;
  /** 带工具对话的事件序列(含 toolStart / toolEnd);不给则与普通对话相同 */
  agentEvents?: (messages: { role: string; content: string }[]) => AgentEvent[];
  /** 带工具对话直接报错(模拟模型不支持工具):给出错误文本 */
  agentError?: string;
  /** 事件间隔(毫秒) */
  delay?: number;
  queries?: string[];
  inserted?: { sql: string; connId: string; confirmed: boolean }[];
  queryResult?: (sql: string) => QueryResult;
}

/** 装上假的 AI 后端与连接依赖。返回记录(发出的请求、执行过的 SQL)。 */
export function installFakes(o: FakeOptions = {}) {
  const sent: { role: string; content: string }[][] = [];
  const queries = o.queries ?? [];
  const inserted = o.inserted ?? [];
  let settings = o.ready === false ? { ...FAKE_SETTINGS, providers: [], current: null } : FAKE_SETTINGS;
  aiBackend.settings = () => settings;
  aiBackend.status = () =>
    settings.providers.length > 0
      ? { ready: true, model: settings.current?.model ?? null }
      : { ready: false, error: "AI provider not configured. Add one in Settings." };
  aiBackend.saveSettings = (s) => {
    settings = s;
    return s;
  };
  // 「停止」:正在播放的流不再推事件、直接结束(同原生侧中止)
  let stopped = false;
  aiBackend.stop = () => {
    stopped = true;
  };
  const play = <E,>(events: E[], onEvent: (e: E) => void) =>
    new Promise<void>((resolve) => {
      stopped = false;
      let i = 0;
      const step = () => {
        if (stopped || i >= events.length) return resolve();
        onEvent(events[i++]);
        setTimeout(step, o.delay ?? 10);
      };
      setTimeout(step, o.delay ?? 10);
    });
  const plainEvents = (messages: { role: string; content: string }[]): ChatEvent[] =>
    o.events
      ? o.events(messages)
      : [...chunks(REPLY).map((text) => ({ type: "delta" as const, text })), { type: "done" as const, full: REPLY }];
  aiBackend.chat = (messages, onEvent) => {
    sent.push(messages);
    return play(plainEvents(messages), onEvent);
  };
  aiBackend.agentChat = (_kind, _id, messages, onEvent) => {
    sent.push(messages);
    if (o.agentError) return play([{ type: "error" as const, message: o.agentError }], onEvent);
    return play(o.agentEvents ? o.agentEvents(messages) : plainEvents(messages), onEvent);
  };
  chatDeps.headConn = () => FAKE_CONN;
  chatDeps.currentTable = () => null;
  chatDeps.tables = async () => ["users", "orders"];
  chatDeps.columns = () => ({ orders: [{ name: "total", dataType: "numeric" }] });
  chatDeps.tableDetail = async () => ({ columns: [], indexes: [] });
  const run = async (sql: string) => {
    queries.push(sql);
    if (o.queryResult) return o.queryResult(sql);
    return { columns: [], rows: [], affected: 0 };
  };
  chatDeps.query = (_k, _id, sql) => run(sql);
  chatDeps.queryReadOnly = (_k, _id, sql) => run(sql);
  chatDeps.refreshTables = () => {};
  chatDeps.writeImpact = async (_k, _id, sql) =>
    o.impact
      ? o.impact(sql)
      : { count: 3, wholeTable: false, columns: ["id", "name"], rows: [["1", "a"], ["2", "b"], ["3", "c"]] };
  chatDeps.openQuery = (sql, connId) => toast.info(`openQuery ${connId} ${sql}`);
  const undone = o.undone ?? [];
  chatDeps.undoPrepare = async (_k, _id, sql) =>
    o.undo
      ? o.undo(sql)
      : /\b(drop|truncate)\b/i.test(sql)
        ? { statements: [], rows: 0, reason: "TRUNCATE and DROP TABLE can't be undone" }
        : { statements: ["INSERT INTO orders (id) VALUES ('1')", "INSERT INTO orders (id) VALUES ('2')"], rows: 2, reason: null };
  chatDeps.importSql = async (_k, _id, statements) => {
    undone.push(statements);
    return statements.length;
  };
  chatDeps.insertAndRun = (sql, connId, confirmed) => {
    inserted.push({ sql, connId, confirmed });
    toast.info(`insertAndRun ${connId} confirmed=${confirmed}`);
  };
  chatDeps.activeSelectionText = () => "SELECT * FROM users";
  chatDeps.openSettings = () => {};
  refreshAi();
  return { sent, queries, inserted };
}
