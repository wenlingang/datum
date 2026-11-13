// AI 面板的动作:会话 key、绑定连接、发送(按需拉表结构后拼 system prompt)、
// SQL 卡片的直接执行(结果以隐藏消息回灌对话)与「编辑器运行」。对应 GPUI 版 chat/mod.rs 的逻辑部分。

import { signal } from "nsh";
import { t } from "nsh/i18n";
import { chat as chatNative, errorText, sql as sqlNative } from "../lib/native";
import { type Connection, supportsSql, type TableDetail, type ToolDetail, type UndoPrep } from "../lib/types";
import {
  aiStore,
  appendMessage,
  bindConvo,
  type BoundConn,
  deleteConvo,
  newConvo,
  selectConvo,
  sendMessage,
  setPendingRef,
  takeLastUserTurn,
} from "../store/ai";
import { toast } from "../store/toast";
import { setAiOpen } from "../store/workspace";
import { clearCards } from "./cards";
import { chatDeps } from "./deps";

/** 执行结果在对话里保留的行数。 */
const RESULT_ROWS = 200;
/** 按需拉取过的表结构(key = connId:table),对话期间复用;清空对话时丢弃。 */
const details = new Map<string, TableDetail>();
/** 发送前在拉表结构:此时不允许再发。 */
const preparing = signal(false);

/** 「连接名 / 库名」;SQLite 显示文件名。 */
export function connLabel(c: Connection): string {
  let db: string;
  if (c.kind === "sqlite") {
    const name = (c.filePath ?? "").split(/[\\/]/).pop();
    db = name || "—";
  } else {
    db = c.database || "—";
  }
  return `${c.name} / ${db}`;
}

/** 会话 key:当前连接正在进行的那段对话。切表、开新查询 tab 都不打断它。 */
export function convoKey(): string | null {
  const c = chatDeps.headConn();
  return c ? aiStore.activeKey(c.id) : null;
}

/** SQL 的目标连接:会话已绑定的连接,否则当前的关系库连接。 */
export function target(): BoundConn | null {
  const key = convoKey();
  if (!key) return null;
  const bound = aiStore.convo(key)?.bound;
  if (bound) return bound;
  const conn = chatDeps.headConn();
  if (!conn || !supportsSql(conn.kind)) return null;
  return { connId: conn.id, kind: conn.kind, label: connLabel(conn) };
}

export function canSend(): boolean {
  return aiStore.ready() && target() !== null && aiStore.streamingKey() === null && !preparing() && convoKey() !== null;
}

/** 消息正文:输入文本 + 引用的 SQL。 */
export function composeText(typed: string, quoted: string | null): string {
  if (quoted === null) return typed;
  return `${typed ? `${typed}\n\n` : ""}${t("chat.quoted_sql")}\n\`\`\`sql\n${quoted}\n\`\`\``;
}

/**
 * 发送:输入框文本(或快捷动作文本)+ 引用 SQL。schema 上下文按需拉取后再发。
 * 返回是否已受理(受理后调用方清空输入框)。
 */
export function sendText(typed: string): boolean {
  const text = typed.trim();
  const pending = aiStore.pendingRef();
  if ((!text && pending === null) || !canSend()) return false;
  const key = convoKey();
  const to = target();
  if (!key || !to) return false;
  const finalText = composeText(text, pending);
  if (!aiStore.convo(key)?.bound) bindConvo(key, to);
  setPendingRef(null);
  preparing.set(true);
  void prepareAndSend(key, to, finalText).finally(() => preparing.set(false));
  return true;
}

async function prepareAndSend(key: string, to: BoundConn, text: string) {
  let tables: string[] = [];
  try {
    tables = await chatDeps.tables(to.connId);
  } catch {
    tables = [];
  }
  const current = chatDeps.currentTable();
  const wanted = chatNative.mentionedTables(text, tables);
  if (current && current.connId === to.connId && !wanted.includes(current.table)) wanted.unshift(current.table);
  for (const name of wanted) {
    const k = `${to.connId}:${name}`;
    if (details.has(k)) continue;
    try {
      details.set(k, await chatDeps.tableDetail(to.kind, to.connId, name));
    } catch {
      // 拉不到的表结构跳过,不影响对话
    }
  }
  const prefix = `${to.connId}:`;
  const own: Record<string, TableDetail> = {};
  for (const [k, d] of details) if (k.startsWith(prefix)) own[k.slice(prefix.length)] = d;
  let prompt: string;
  try {
    prompt = chatDeps.systemPrompt({
      kind: to.kind,
      database: to.label,
      tables,
      details: own,
      columns: chatDeps.columns(to.connId),
    });
  } catch (e) {
    toast.error(errorText(e));
    return;
  }
  // 关系库:AI 可以自己列表、看表结构、跑只读查询
  await sendMessage(key, prompt, text, supportsSql(to.kind) ? { kind: to.kind, connId: to.connId } : undefined);
}

/** 「重新生成」/「重试」:去掉最后一轮,原样重发那条用户消息(引用的 SQL 已在正文里)。 */
export function regenerate() {
  const key = convoKey();
  const to = target();
  if (!key || !to || !canSend()) return;
  const text = takeLastUserTurn(key);
  if (text === null) return;
  preparing.set(true);
  void prepareAndSend(key, to, text).finally(() => preparing.set(false));
}

/** 开一段新对话:当前对话留在历史里;表结构缓存重新按需加载。 */
export function newConversation() {
  const c = chatDeps.headConn();
  if (!c || aiStore.streamingKey() !== null) return;
  newConvo(c.id);
  details.clear();
}

/** 切到历史里的某段对话。 */
export function openConversation(key: string) {
  const c = chatDeps.headConn();
  if (!c || aiStore.streamingKey() !== null) return;
  selectConvo(c.id, key);
  details.clear();
}

/** 删除一段对话(连同它的卡片状态)。 */
export function removeConversation(key: string) {
  const c = chatDeps.headConn();
  if (!c || aiStore.streamingKey() === key) return;
  deleteConvo(c.id, key);
  clearCards();
}


/** 引用编辑器当前选区。 */
export function quoteSelection() {
  const text = chatDeps.activeSelectionText().trim();
  if (!text) {
    toast.error(t("workspace.quote_select_first"));
    return;
  }
  sendToAiPanel(text);
}

/** 把一段 SQL 作为引用并打开 AI 面板。 */
export function sendToAiPanel(text: string) {
  setPendingRef(text);
  setAiOpen(true);
}

function truncateSql(s: string): string {
  const chars = Array.from(s);
  return chars.length > 200 ? `${chars.slice(0, 200).join("")}…` : s;
}

/**
 * SQL 卡片的执行入口:在会话绑定的连接上预切分逐条执行,任一条失败即中止;
 * 结果以隐藏消息回灌对话。整段只读时走只读会话。返回 (是否成功, 摘要)。
 */
export async function executeSql(sql: string): Promise<{ ok: boolean; summary: string; undo: UndoPrep | null }> {
  const key = convoKey();
  const bound = key ? aiStore.convo(key)?.bound : null;
  if (!key || !bound) return { ok: false, summary: t("chat.not_bound"), undo: null };
  const stmts = sqlNative.statements(sql);
  const readOnly = sqlNative.isReadOnly(sql);
  // 写语句:先抓会被改动的原始行,生成撤销用的反向语句
  const undo: UndoPrep | null = readOnly
    ? null
    : await chatDeps
        .undoPrepare(bound.kind, bound.connId, sql)
        .catch((e): UndoPrep => ({ statements: [], rows: 0, reason: errorText(e) }));
  const multi = stmts.length > 1;
  const parts: string[] = [];
  let ok = true;
  /** 最后一条带结果集的查询:在对话里直接显示成表 */
  let table: ToolDetail | undefined;
  for (let i = 0; i < stmts.length; i++) {
    const stmt = stmts[i];
    const label = multi ? t("chat.stmt_n", { n: i + 1 }) : "";
    try {
      const r = readOnly
        ? await chatDeps.queryReadOnly(bound.kind, bound.connId, stmt)
        : await chatDeps.query(bound.kind, bound.connId, stmt);
      if (r.columns.length > 0) table = { sql: stmt, columns: r.columns, rows: r.rows.slice(0, RESULT_ROWS) };
      const brief = sqlNative.isQuery(stmt)
        ? t("chat.returned_rows", { count: r.rows.length })
        : `${t("chat.succeeded")}${r.affected ? t("chat.affected_rows", { count: r.affected }) : ""}`;
      parts.push(label ? t("chat.labeled", { label, brief }) : brief);
    } catch (e) {
      parts.push(t("chat.failed", { label: label || t("chat.execution"), error: errorText(e) }));
      ok = false;
      break;
    }
  }
  const summary = parts.join(" · ");
  appendMessage(
    key,
    "user",
    `${t("chat.executed_tag")} ${truncateSql(sql)}\n${t("chat.result_label")}${ok ? "✓" : "✗"} ${summary}`,
    true,
    table ? { table } : undefined,
  );
  const lower = sql.toLowerCase();
  if (ok && (lower.includes("create") || lower.includes("drop") || lower.includes("alter"))) {
    chatDeps.refreshTables(bound.connId);
  }
  return { ok, summary, undo: ok ? undo : null };
}

/** 撤销某张卡片执行过的写操作:在一个事务里执行反向语句,并告知 AI。 */
export async function undoCard(cardKeyValue: string, undo: UndoPrep): Promise<boolean> {
  const key = convoKey();
  const bound = key ? aiStore.convo(key)?.bound : null;
  if (!key || !bound) return false;
  try {
    await chatDeps.importSql(bound.kind, bound.connId, undo.statements);
    appendMessage(key, "user", `${t("chat.undone_tag")} ${t("chat.undone_rows", { rows: undo.rows })}`, true);
    return true;
  } catch (e) {
    toast.error(t("undo.failed", { error: errorText(e) }));
    return false;
  }
}

/** 「编辑器运行」:只在会话绑定的连接上执行。`confirmed` 为卡片已做过破坏性确认。 */
export function runInEditor(sql: string, confirmed: boolean) {
  const key = convoKey();
  const bound = key ? aiStore.convo(key)?.bound : null;
  if (!bound) {
    toast.error(t("chat.not_bound"));
    return;
  }
  chatDeps.insertAndRun(sql, bound.connId, confirmed);
}

/** 「让 AI 修复」:上一条执行失败后,请 AI 修正。 */
export function askFix() {
  sendText(t("chat.fix_prompt"));
}

/** 交给 AI 的执行计划最多带多少字符(过长截断,避免撑爆上下文)。 */
const PLAN_MAX_CHARS = 8000;

/**
 * 把一段 SQL 作为引用、连同一段提问发给 AI(编辑器里的「让 AI 修复 / 解读」)。
 * 发不出去(未配置 AI、上一条还在回复等)时保留引用并打开面板,让用户自己发。
 */
function askAiAbout(sql: string, prompt: string) {
  setPendingRef(sql);
  setAiOpen(true);
  if (!sendText(prompt)) toast.info(t("chat.ask_not_sent"));
}

/** 编辑器执行报错:请 AI 说明原因并给出修正后的 SQL。 */
export function askAiFixError(sql: string, error: string) {
  askAiAbout(sql, `${t("chat.fix_error_prompt")}\n\n${t("chat.error_label")}\n\`\`\`\n${error}\n\`\`\``);
}

/** 执行计划(EXPLAIN 的结果表)转成文本:单列计划(PG / SQLite)逐行拼接,多列(MySQL)按表格。 */
export function planText(columns: string[], rows: (string | null)[][]): string {
  const cell = (v: string | null) => v ?? "NULL";
  let text =
    columns.length === 1
      ? rows.map((r) => cell(r[0])).join("\n")
      : [columns.join(" | "), ...rows.map((r) => r.map(cell).join(" | "))].join("\n");
  if (text.length > PLAN_MAX_CHARS) text = `${text.slice(0, PLAN_MAX_CHARS)}\n…`;
  return text;
}

/** Explain 的结果:请 AI 解读瓶颈并给出索引等改进建议。 */
export function askAiExplainPlan(sql: string, plan: string) {
  askAiAbout(sql, `${t("chat.explain_plan_prompt")}\n\n${t("chat.plan_label")}\n\`\`\`\n${plan}\n\`\`\``);
}
