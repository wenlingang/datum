// AI 助手:配置状态、按连接分的会话、流式回复。对应 GPUI 版 store.rs 的 AiStore。
//
// 会话 key 按连接区分(`conn:<id>`),切表、开新查询 tab 都不打断当前对话。
// 同一时刻只允许一个流(streamingKey),流式写入锁定在发送时的会话上。

import { batch, signal } from "nsh";
import { t } from "nsh/i18n";
import { ai, errorText } from "../lib/native";
import type { AgentEvent, AiSettings, AiStatus, ChatEvent, DbKind, ToolDetail } from "../lib/types";
import { toast } from "./toast";

export type AiRole = "user" | "assistant";

export interface AiMessage {
  readonly id: number;
  readonly role: AiRole;
  /** 回复的全部文字(不含工具步骤) */
  readonly content: string;
  /** 合成消息(如执行结果回灌):发给 AI,但以系统条样式渲染 */
  readonly hidden: boolean;
  /** AI 自己查数据时:文字段与工具步骤按发生顺序排列;普通回复为 undefined */
  readonly parts?: readonly MessagePart[];
  /** 回复所用的模型(显示在回复上方) */
  readonly model?: string;
  /** 开始等待回复的时刻(毫秒,算「思考中… N 秒」) */
  readonly startedAt?: number;
  /** 回复出错:显示错误行与「重试」;不进发给 AI 的历史 */
  readonly error?: string;
  /** 用户点了「停止」 */
  readonly stopped?: boolean;
  /** 执行结果回灌(隐藏消息)附带的查询结果,在对话里直接显示成表 */
  readonly table?: ToolDetail;
}

/** 一次工具调用(列表、看表结构、只读查询)。 */
export interface ToolStep {
  readonly id: string;
  readonly name: string;
  readonly args: Record<string, unknown>;
  readonly state: "running" | "ok" | "error";
  readonly summary: string;
  readonly detail: ToolDetail | null;
}

export type MessagePart = { readonly kind: "text"; readonly text: string } | ({ readonly kind: "tool" } & ToolStep);

/** 对话绑定的连接:SQL 卡片永远在此连接上执行,防止切换连接后误打新库。 */
export interface BoundConn {
  readonly connId: string;
  readonly kind: DbKind;
  /** 「连接名 / 库名」 */
  readonly label: string;
}

export interface Convo {
  readonly messages: readonly AiMessage[];
  readonly bound: BoundConn | null;
}

/** 发送给 AI 的历史上限(system 每次重建,不占额度)。 */
export const MAX_HISTORY = 30;

type WireMessage = { role: string; content: string };

/** 原生调用的入口;测试替换成假的实现(不碰真实配置与 AI 服务)。 */
export const aiBackend = {
  status: (): AiStatus => ai.status(),
  settings: (): AiSettings => ai.settings(),
  saveSettings: (s: AiSettings): AiSettings => ai.saveSettings(s),
  chat: (messages: WireMessage[], onEvent: (e: ChatEvent) => void): Promise<void> => ai.chat(messages, onEvent),
  agentChat: (kind: DbKind, connId: string, messages: WireMessage[], onEvent: (e: AgentEvent) => void): Promise<void> =>
    ai.agentChat(kind, connId, messages, onEvent),
  stop: () => ai.stopChat(),
};

/** 不支持工具调用的模型(本次运行内记住,之后直接走普通对话)。 */
const noTools = new Set<string>();

function modelKey(): string {
  const c = settings()?.current;
  return c ? `${c.providerId}/${c.model}` : "";
}

const status = signal<AiStatus | null>(null);
const settings = signal<AiSettings | null>(null);
const convos = signal<Record<string, Convo>>({});
const streamingKey = signal<string | null>(null);
const pendingRef = signal<string | null>(null);
let nextId = 1;

export const aiStore = {
  /** 配置状态;null 为尚未读取。 */
  status: () => status(),
  ready: () => !!status()?.ready,
  settings: () => settings(),
  convo: (key: string | null | undefined): Convo | undefined => (key ? convos()[key] : undefined),
  /** 正在流式接收的会话 key。 */
  streamingKey: () => streamingKey(),
  /** 编辑器选中后待发送的引用(发送时拼进消息正文)。 */
  pendingRef: () => pendingRef(),
};

/** 从磁盘重读 AI 配置与状态(启动、设置保存后)。 */
export function refreshAi() {
  try {
    const s = aiBackend.settings();
    const st = aiBackend.status();
    batch(() => {
      settings.set(s);
      status.set(st);
    });
  } catch (e) {
    status.set({ ready: false, error: errorText(e) });
  }
}

/** 保存整份配置(当前模型失效时原生侧自动改选),并刷新状态。失败抛出。 */
export function saveAiSettings(next: AiSettings): AiSettings {
  const saved = aiBackend.saveSettings(next);
  settings.set(saved);
  try {
    status.set(aiBackend.status());
  } catch (e) {
    status.set({ ready: false, error: errorText(e) });
  }
  return saved;
}

/** AI 面板切换模型(全局,写回配置,下一条消息起生效)。 */
export function setCurrentModel(providerId: string, model: string) {
  const s = settings();
  if (!s) return;
  try {
    saveAiSettings({ ...s, current: { providerId, model } });
  } catch (e) {
    toast.error(errorText(e));
  }
}

function updateConvo(key: string, f: (c: Convo) => Convo) {
  convos.update((m) => ({ ...m, [key]: f(m[key] ?? { messages: [], bound: null }) }));
}

export function bindConvo(key: string, bound: BoundConn) {
  updateConvo(key, (c) => ({ ...c, bound }));
}

export function setPendingRef(sql: string | null) {
  pendingRef.set(sql);
}

/** 向会话追加消息但不请求 AI(执行结果回灌用)。返回消息 id。 */
export function appendMessage(
  key: string,
  role: AiRole,
  content: string,
  hidden: boolean,
  extra?: Partial<Pick<AiMessage, "model" | "startedAt" | "table">>,
): number {
  const id = nextId++;
  updateConvo(key, (c) => ({ ...c, messages: [...c.messages, { id, role, content, hidden, ...extra }] }));
  return id;
}

/** 给消息补上字段(出错、停止)。 */
function markMessage(key: string, id: number, fields: Partial<Pick<AiMessage, "error" | "stopped">>) {
  updateConvo(key, (c) => ({ ...c, messages: c.messages.map((x) => (x.id === id ? { ...x, ...fields } : x)) }));
}

/** 用户点了「停止」:当前回复以已收到的内容结束。 */
let stopRequested = false;
export function stopStreaming() {
  if (streamingKey() === null) return;
  stopRequested = true;
  try {
    aiBackend.stop();
  } catch {
    // 原生侧已结束
  }
}

/**
 * 「重新生成」:去掉会话里最后一条用户消息及其后的回复,返回那条用户消息的正文(交给调用方重发)。
 * 正在回复或没有可重发的消息时返回 null。
 */
export function takeLastUserTurn(key: string): string | null {
  if (streamingKey() !== null) return null;
  const msgs = convos()[key]?.messages ?? [];
  let i = msgs.length - 1;
  while (i >= 0 && !(msgs[i].role === "user" && !msgs[i].hidden)) i--;
  if (i < 0) return null;
  const text = msgs[i].content;
  updateConvo(key, (c) => ({ ...c, messages: c.messages.slice(0, i) }));
  return text;
}

export function clearConvo(key: string) {
  convos.update((m) => {
    const next = { ...m };
    delete next[key];
    return next;
  });
}

/** 改写一条消息。`parts`:不给则保留原样,null 去掉分段(退回普通回复)。 */
function patchMessage(key: string, id: number, content: string, parts?: readonly MessagePart[] | null) {
  convos.update((m) => {
    const c = m[key];
    if (!c) return m;
    const apply = (x: AiMessage): AiMessage => {
      if (x.id !== id) return x;
      if (parts === undefined) return { ...x, content };
      if (parts === null) {
        const { parts: _drop, ...rest } = x;
        return { ...rest, content };
      }
      return { ...x, content, parts };
    };
    return { ...m, [key]: { ...c, messages: c.messages.map(apply) } };
  });
}

/** 历史里每次查询带给 AI 的行数(之后的轮次还记得查到过什么)。 */
const HISTORY_ROWS = 10;

/** 工具步骤 → 历史里的一段文字摘要。 */
export function stepText(s: ToolStep): string {
  const d = s.detail;
  const head =
    s.name === "run_query" && d?.sql ? `[run_query] ${d.sql}` : `[${s.name}] ${JSON.stringify(s.args)}`;
  if (s.state === "error") return `${head}\n→ ${s.summary}`;
  const lines = [`${head}\n→ ${s.summary}`];
  if (d?.columns && d.rows) {
    lines.push((d.columns as string[]).join("\t"));
    for (const r of d.rows.slice(0, HISTORY_ROWS)) lines.push(r.map((v) => v ?? "NULL").join("\t"));
    if (d.rows.length > HISTORY_ROWS) lines.push(`… ${d.rows.length - HISTORY_ROWS} more`);
  }
  return lines.join("\n");
}

/** 发给 AI 的正文:带工具步骤的回复把步骤摘要按顺序嵌进去。 */
function historyContent(m: AiMessage): string {
  // 用户在卡片上执行的查询:把前几行也带给 AI,追问时它知道结果
  if (m.table?.columns && m.table.rows) {
    const rows = m.table.rows.slice(0, HISTORY_ROWS).map((r) => r.map((v) => v ?? "NULL").join("\t"));
    const more = m.table.rows.length > HISTORY_ROWS ? [`… ${m.table.rows.length - HISTORY_ROWS} more`] : [];
    return [m.content, (m.table.columns as string[]).join("\t"), ...rows, ...more].join("\n");
  }
  if (!m.parts) return m.content;
  return m.parts.map((p) => (p.kind === "text" ? p.text : stepText(p))).join("\n\n");
}

/** 发给 AI 的报文:system + 最近 MAX_HISTORY 条历史(不含正在生成的空回复)。 */
export function wireMessages(systemPrompt: string, history: readonly AiMessage[]): WireMessage[] {
  const tail = history.slice(Math.max(0, history.length - MAX_HISTORY));
  return [{ role: "system", content: systemPrompt }, ...tail.map((m) => ({ role: m.role, content: historyContent(m) }))];
}

/** 让 AI 自己查数据的连接(关系库);不给时走普通对话。 */
export interface AgentTarget {
  readonly kind: DbKind;
  readonly connId: string;
}

/**
 * 在会话 `key` 发送一条消息并流式接收回复。已有流在跑或文本为空时忽略。
 * 给了 `agent` 且当前模型支持工具时,AI 可自己列表、看表结构、跑只读查询;
 * 模型不支持工具(还没输出就报错)时自动改用普通对话,并在本次运行内记住。
 */
export async function sendMessage(key: string, systemPrompt: string, text: string, agent?: AgentTarget) {
  if (streamingKey() !== null || !text.trim()) return;
  let assistantId = 0;
  batch(() => {
    appendMessage(key, "user", text, false);
    assistantId = appendMessage(key, "assistant", "", false, {
      model: settings()?.current?.model ?? undefined,
      startedAt: Date.now(),
    });
    streamingKey.set(key);
    stopRequested = false;
  });
  const history = (convos()[key]?.messages ?? []).filter((m) => m.id !== assistantId);
  const wire = wireMessages(systemPrompt, history);
  const model = modelKey();
  const useAgent = !!agent && !noTools.has(model);

  let acc = "";
  let parts: MessagePart[] = [];
  let produced = false;
  let finished = false;
  let errored = false;
  let agentMode = useAgent;
  const flush = () => patchMessage(key, assistantId, acc, agentMode ? parts : undefined);
  const appendText = (t: string) => {
    acc += t;
    const last = parts[parts.length - 1];
    parts = last?.kind === "text" ? [...parts.slice(0, -1), { kind: "text", text: last.text + t }] : [...parts, { kind: "text", text: t }];
  };
  const onEvent = (e: AgentEvent) => {
    if (finished) return;
    switch (e.type) {
      case "delta":
        produced = true;
        appendText(e.text);
        flush();
        break;
      case "toolStart":
        produced = true;
        parts = [...parts, { kind: "tool", id: e.id, name: e.name, args: e.args, state: "running", summary: "", detail: null }];
        flush();
        break;
      case "toolEnd":
        parts = parts.map((p) =>
          p.kind === "tool" && p.id === e.id
            ? { ...p, state: e.ok ? "ok" : "error", summary: e.summary, detail: e.detail }
            : p,
        );
        flush();
        break;
      case "done":
        // 全文兜底:个别 Delta 丢失也能校正最终文本(工具模式下以分段为准)
        if (!agentMode) acc = e.full || acc;
        if (!acc && !parts.some((p) => p.kind === "tool") && !stopRequested) acc = t("store.ai_empty_reply");
        if (agentMode && !parts.length) parts = [{ kind: "text", text: acc }];
        flush();
        finished = true;
        break;
      case "error":
        errored = true;
        flush();
        markMessage(key, assistantId, { error: e.message });
        finished = true;
        break;
    }
  };
  const runPlain = async () => {
    try {
      await aiBackend.chat(wire, onEvent);
    } catch (e) {
      onEvent({ type: "error", message: errorText(e) });
    }
  };
  if (useAgent && agent) {
    // 还没输出就失败:多半是模型不支持工具,改用普通对话重试一次
    let failedEarly = false;
    const guard = (e: AgentEvent) => {
      if (e.type === "error" && !produced) {
        failedEarly = true;
        return;
      }
      onEvent(e);
    };
    try {
      await aiBackend.agentChat(agent.kind, agent.connId, wire, guard);
    } catch (e) {
      if (!produced) failedEarly = true;
      else onEvent({ type: "error", message: errorText(e) });
    }
    if (failedEarly && !finished) {
      parts = [];
      agentMode = false;
      await runPlain();
      // 普通对话的回复不带分段
      patchMessage(key, assistantId, acc, null);
      // 普通对话能成功,说明是工具不被支持(而不是网络 / 鉴权问题):记住,之后不再尝试
      if (!errored) noTools.add(model);
    }
  } else {
    await runPlain();
  }
  // 原生侧的事件经 post 排到 JS 的下一轮,让它们先到;仍没有终止事件时以累计文本收尾
  if (!finished) await new Promise((r) => setTimeout(r, 0));
  if (!finished) onEvent({ type: "done", full: "" });
  if (stopRequested) markMessage(key, assistantId, { stopped: true });
  stopRequested = false;
  streamingKey.set(null);
}

refreshAi();
