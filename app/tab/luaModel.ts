// Redis Lua 脚本标签页的状态:脚本、KEYS / ARGV、执行状态与结果。
//
// 标签页换走时组件整个重建(见 index.tsx 的 TabView),状态都放在这里(挂在 WorkTab.state 上),
// 换回来时脚本、上次填的 KEYS / ARGV、进行中的执行与结果都还在。会话恢复时脚本与参数也一并放回(session.ts)。
//
// 执行在 core 里开专用连接(见 rds::lua):脚本跑着的时候换 tab 不影响;关掉标签页时放弃等待(关专用连接)。
// 关 tab 没有回调,由执行期间的计时器发现标签页已不在。

import { signal, type Signal } from "nsh";
import { t } from "nsh/i18n";
import { db, errorText } from "../lib/native";
import type { LuaOutcome } from "../lib/types";
import { conns } from "../store/conns";
import { addTab, newTabId, workspace, type WorkTab } from "../store/workspace";

/** 计时器间隔(显示已运行时长、检查标签页是否已关)。 */
const TICK_MS = 250;

export interface LuaModel {
  readonly lua: true;
  connId: string;
  script: Signal<string>;
  keys: Signal<string[]>;
  args: Signal<string[]>;
  /** 进行中的执行 id(null 为没有在执行) */
  run: Signal<string | null>;
  startedAt: Signal<number>;
  now: Signal<number>;
  outcome: Signal<LuaOutcome | null>;
  /** 连不上、放弃等待等(服务器回的错误在 outcome 里) */
  error: Signal<string | null>;
  /** 是否集群模式;没查过 / 查不到为 null */
  cluster: Signal<boolean | null>;
  /** SCRIPT KILL 进行中 */
  killing: Signal<boolean>;
  /** SCRIPT KILL 回了 UNKILLABLE 时的提示(到下次执行为止) */
  killNote: Signal<string | null>;
}

export function createLuaModel(connId: string, init?: { script?: string; keys?: string[]; args?: string[] }): LuaModel {
  return {
    lua: true,
    connId,
    script: signal(init?.script ?? ""),
    keys: signal(init?.keys ?? []),
    args: signal(init?.args ?? []),
    run: signal<string | null>(null),
    startedAt: signal(0),
    now: signal(0),
    outcome: signal<LuaOutcome | null>(null),
    error: signal<string | null>(null),
    cluster: signal<boolean | null>(null),
    killing: signal(false),
    killNote: signal<string | null>(null),
  };
}

export function luaModelOf(tab: WorkTab | undefined | null): LuaModel | undefined {
  const s = tab?.state as LuaModel | undefined;
  return s && typeof s === "object" && "lua" in s && s.lua === true ? s : undefined;
}

/** 在连接上新开 Lua 脚本标签页,可预填脚本(从片段打开时)。只对 Redis 连接。 */
export function openRedisLua(connId: string, script?: string): WorkTab | undefined {
  const conn = conns.get(connId);
  if (!conn || conn.kind !== "redis") return undefined;
  const n = workspace.tabs().filter((x) => x.kind === "rlua" && x.connId === connId).length;
  const base = t("rlua.title");
  const tab: WorkTab = {
    id: newTabId(),
    kind: "rlua",
    connId,
    dbKind: conn.kind,
    table: null,
    title: signal(n === 0 ? base : `${base} ${n + 1}`),
    state: createLuaModel(connId, { script }),
  };
  addTab(tab);
  return tab;
}

const alive = (m: LuaModel) => workspace.tabs().some((x) => x.state === m);

/** 执行(调用方已做过确认)。参数逐个传给 EVAL,不拼字符串。 */
export async function runLua(m: LuaModel) {
  if (m.run() !== null) return;
  const run = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  m.run.set(run);
  m.error.set(null);
  m.killNote.set(null);
  m.startedAt.set(Date.now());
  m.now.set(Date.now());
  const timer = setInterval(() => {
    m.now.set(Date.now());
    // 标签页已关:放弃等待,关掉专用连接
    if (!alive(m)) {
      clearInterval(timer);
      void db.redisEvalAbandon(run).catch(() => {});
    }
  }, TICK_MS);
  try {
    const out = await db.redisEval(m.connId, run, { script: m.script(), keys: m.keys(), args: m.args() });
    if (m.run() === run) m.outcome.set(out);
  } catch (e) {
    if (m.run() === run) {
      m.outcome.set(null);
      m.error.set(errorText(e));
    }
  } finally {
    clearInterval(timer);
    if (m.run() === run) m.run.set(null);
  }
}

/** 放弃等待:关掉专用连接,结果不要了(服务器上的脚本不会因此停下)。 */
export function abandonLua(m: LuaModel) {
  const run = m.run();
  if (run) void db.redisEvalAbandon(run).catch(() => {});
}
