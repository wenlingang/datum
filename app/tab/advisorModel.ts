// 索引建议 tab 的模型:一条语句、建议结果、对比哪个候选的执行计划。
// 分析在 core 里只读 / 回滚地进行,这里只管调用与状态;真正建索引一律放进编辑器,执行时走风险审查。

import { batch, signal, type Signal } from "nsh";
import { t } from "nsh/i18n";
import { db, errorText } from "../lib/native";
import { supportsSql, type DbKind, type IndexAdvice } from "../lib/types";
import { activate } from "../store/conns";
import { addTab, focusTab, newTabId, workspace, type WorkTab } from "../store/workspace";

export interface AdvisorModel {
  readonly connId: string;
  readonly dbKind: DbKind;
  /** 分析的语句(已代入参数)。不叫 sql:modelOf 按 "sql" 字段认查询 / 表浏览模型 */
  readonly statement: string;
  readonly busy: Signal<boolean>;
  /** 正在请 AI 补充候选 */
  readonly aiBusy: Signal<boolean>;
  readonly advice: Signal<IndexAdvice | null>;
  readonly err: Signal<string | null>;
  /** 计划对比选中的候选下标;-1 为组合 */
  readonly compare: Signal<number>;
  token: number;
}

export function createAdvisorModel(connId: string, dbKind: DbKind, sql: string): AdvisorModel {
  return {
    connId,
    dbKind,
    statement: sql,
    busy: signal(false),
    aiBusy: signal(false),
    advice: signal<IndexAdvice | null>(null),
    err: signal<string | null>(null),
    compare: signal(0),
    token: 0,
  };
}

export function advisorModelOf(tab: WorkTab | undefined | null): AdvisorModel | undefined {
  const s = tab?.state as AdvisorModel | undefined;
  return tab?.kind === "index" && s && typeof s === "object" && "compare" in s ? s : undefined;
}

/** 为一条(已代入参数的)语句打开索引建议 tab;同一连接、同一语句已开着就切过去。分析在 tab 显示后开始。 */
export function openIndexAdvisor(connId: string, dbKind: DbKind, sql: string) {
  if (!supportsSql(dbKind)) return;
  const text = sql.trim();
  const existing = workspace.tabs().find((x) => x.kind === "index" && x.connId === connId && advisorModelOf(x)?.statement === text);
  if (existing) {
    activate(connId);
    focusTab(existing);
    return;
  }
  addTab({
    id: newTabId(),
    kind: "index",
    connId,
    dbKind,
    table: null,
    title: signal(t("advisor.title")),
    state: createAdvisorModel(connId, dbKind, text),
  });
}

/** 跑一次建议;`withAi` 时把 SQL、表结构与当前计划发给 AI 补充候选(用户点了才发)。 */
export async function runAdvice(m: AdvisorModel, withAi = false) {
  const token = ++m.token;
  batch(() => {
    m.busy.set(true);
    m.aiBusy.set(withAi);
    m.err.set(null);
  });
  try {
    const advice = await db.indexAdvise(m.dbKind, m.connId, m.statement, withAi);
    if (token !== m.token) return;
    batch(() => {
      m.advice.set(advice);
      m.compare.set(0);
    });
  } catch (e) {
    if (token !== m.token) return;
    m.err.set(errorText(e));
  } finally {
    if (token === m.token) {
      batch(() => {
        m.busy.set(false);
        m.aiBusy.set(false);
      });
    }
  }
}

/** 成本的显示:千以上取整加分隔符。 */
export function fmtCost(n: number | null): string {
  if (n === null) return "—";
  return n >= 1000 ? Math.round(n).toLocaleString() : n.toFixed(n < 10 ? 2 : 1);
}
