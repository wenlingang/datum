// AI 助手:配置状态、按连接分的会话、流式回复。对应 GPUI 版 store.rs 的 AiStore。
//
// 会话 key 按连接区分(`conn:<id>`),切表、开新查询 tab 都不打断当前对话。
// 同一时刻只允许一个流(streamingKey),流式写入锁定在发送时的会话上。

import { batch, signal } from "nsh";
import { t } from "nsh/i18n";
import { ai, errorText } from "../lib/native";
import type { AiSettings, AiStatus, ChatEvent, DbKind } from "../lib/types";
import { toast } from "./toast";

export type AiRole = "user" | "assistant";

export interface AiMessage {
  readonly id: number;
  readonly role: AiRole;
  readonly content: string;
  /** 合成消息(如执行结果回灌):发给 AI,但以系统条样式渲染 */
  readonly hidden: boolean;
}

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
};

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
export function appendMessage(key: string, role: AiRole, content: string, hidden: boolean): number {
  const id = nextId++;
  updateConvo(key, (c) => ({ ...c, messages: [...c.messages, { id, role, content, hidden }] }));
  return id;
}

export function clearConvo(key: string) {
  convos.update((m) => {
    const next = { ...m };
    delete next[key];
    return next;
  });
}

function patchMessage(key: string, id: number, content: string) {
  convos.update((m) => {
    const c = m[key];
    if (!c) return m;
    return { ...m, [key]: { ...c, messages: c.messages.map((x) => (x.id === id ? { ...x, content } : x)) } };
  });
}

/** 发给 AI 的报文:system + 最近 MAX_HISTORY 条历史(不含正在生成的空回复)。 */
export function wireMessages(systemPrompt: string, history: readonly AiMessage[]): WireMessage[] {
  const tail = history.slice(Math.max(0, history.length - MAX_HISTORY));
  return [{ role: "system", content: systemPrompt }, ...tail.map((m) => ({ role: m.role, content: m.content }))];
}

/** 在会话 `key` 发送一条消息并流式接收回复。已有流在跑或文本为空时忽略。 */
export async function sendMessage(key: string, systemPrompt: string, text: string) {
  if (streamingKey() !== null || !text.trim()) return;
  let assistantId = 0;
  batch(() => {
    appendMessage(key, "user", text, false);
    assistantId = appendMessage(key, "assistant", "", false);
    streamingKey.set(key);
  });
  const history = (convos()[key]?.messages ?? []).filter((m) => m.id !== assistantId);
  const wire = wireMessages(systemPrompt, history);

  let acc = "";
  let finished = false;
  const onEvent = (e: ChatEvent) => {
    if (finished) return;
    switch (e.type) {
      case "delta":
        acc += e.text;
        patchMessage(key, assistantId, acc);
        break;
      case "done":
        // 全文兜底:个别 Delta 丢失也能校正最终文本
        patchMessage(key, assistantId, e.full || acc || t("store.ai_empty_reply"));
        finished = true;
        break;
      case "error":
        patchMessage(key, assistantId, acc ? `${acc}\n\n⚠️ ${e.message}` : `⚠️ ${e.message}`);
        finished = true;
        break;
    }
  };
  try {
    await aiBackend.chat(wire, onEvent);
  } catch (e) {
    onEvent({ type: "error", message: errorText(e) });
  }
  // 原生侧的事件经 post 排到 JS 的下一轮,让它们先到;仍没有终止事件时以累计文本收尾
  if (!finished) await new Promise((r) => setTimeout(r, 0));
  if (!finished) onEvent({ type: "done", full: "" });
  streamingKey.set(null);
}

refreshAi();
