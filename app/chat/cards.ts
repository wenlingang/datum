// SQL 卡片的状态:按 (消息 id, 段序号) 存在数据里,而不是卡片组件里 ——
// 消息列表是虚拟化的,单元格会被复用到别的消息上。

import { signal } from "nsh";
import type { UndoPrep } from "../lib/types";

export type CardRun =
  | { kind: "idle" }
  | { kind: "running" }
  | { kind: "done"; summary: string }
  | { kind: "failed"; error: string };

export interface CardState {
  /** 用户编辑后的文本;未编辑时为 undefined(显示 AI 原文) */
  readonly value?: string;
  readonly run: CardRun;
  /** 执行前准备的撤销(写语句);执行成功后可用 */
  readonly undo?: UndoPrep | null;
  /** 撤销的进度:询问确认 / 执行中 / 已撤销 */
  readonly undoState?: "confirm" | "running" | "done";
}

const IDLE: CardState = { run: { kind: "idle" } };
const cards = signal<Record<string, CardState>>({});

export const cardKey = (msgId: number, index: number) => `${msgId}:${index}`;

export function cardState(key: string): CardState {
  return cards()[key] ?? IDLE;
}

export function updateCard(key: string, patch: Partial<CardState>) {
  cards.update((m) => ({ ...m, [key]: { ...(m[key] ?? IDLE), ...patch } }));
}

export function clearCards() {
  cards.set({});
}

/** 待确认的执行:确认框挂在面板上(不随列表单元格回收)。 */
export interface ConfirmRequest {
  readonly cardKey: string;
  readonly sql: string;
  /** true 为「编辑器运行」,false 为直接执行 */
  readonly insert: boolean;
  readonly target: string;
  readonly statements: number;
  readonly destructive: boolean;
}

const confirm = signal<ConfirmRequest | null>(null);

export const confirmRequest = () => confirm();
export const askConfirm = (req: ConfirmRequest) => confirm.set(req);
export const closeConfirm = () => confirm.set(null);
