// 结果区:状态行(执行中 / 出错 / 成功 · 行数 · 耗时)与结果主体(错误、执行成功、空结果、
// 筛选无命中、行数上限提示、网格)。对应 GPUI 版 tab/mod.rs 的 render_status_line / render_results。

import { Scroll, Text, View } from "nsh";
import { Button } from "nsh/base";
import { t } from "nsh/i18n";
import { maxRows } from "../store/prefs";
import { supportsSql } from "../lib/types";
import { Btn, Dot, Hint, tip } from "../ui/kit";
import { FS } from "../ui/text";
import { ResultGrid } from "./Grid";
import { aiExplainPlan, aiFixError, selectResult, type StmtResult, viewIndices, type TabModel } from "./model";
import { ErrorText } from "../ui/error";

function okSummary(m: TabModel): string {
  const r = m.result();
  if (m.resultIsQuery()) return t("tab.query_ok_rows", { n: viewIndices(m).length });
  const affected = r?.affected ? ` · ${t("tab.affected_rows", { n: r.affected })}` : "";
  return `${t("tab.exec_ok")}${affected}`;
}

/** 查询 tab 结果区上方的状态行。 */
export function StatusLine(props: { m: TabModel }) {
  const m = props.m;
  const state = () => (m.running() ? "running" : m.err() !== null ? "error" : m.result() ? "ok" : "ready");
  return (
    <View class="h-[30px] shrink-0 flex-row items-center gap-2.5 border-b border-border bg-panel px-4">
      <If when={state() === "running"}>
        <Dot color="data" />
        <Text class={`${FS.xs} text-muted-foreground`}>{t("tab.running")}</Text>
      </If>
      <If when={state() === "error"}>
        <Dot color="destructive" />
        <Text class={`${FS.xs} font-bold text-destructive`}>{t("tab.query_error")}</Text>
      </If>
      <If when={state() === "ok"}>
        <Dot color="data" />
        <Text testID="status-summary" class={`${FS.xs} text-muted-foreground`}>
          {okSummary(m)}
        </Text>
        <If when={m.elapsedMs() !== null}>
          <Text class={`${FS.xs} font-mono text-fg-faint`}>{t("tab.elapsed_ms", { ms: m.elapsedMs() ?? 0 })}</Text>
        </If>
      </If>
      <If when={state() === "ready"}>
        <Text class={`${FS.xs} text-fg-faint`}>{t("tab.ready")}</Text>
      </If>
    </View>
  );
}

type Body = "error" | "execOk" | "noRows" | "noMatch" | "grid" | "running" | "none";

function bodyOf(m: TabModel): Body {
  if (m.err() !== null) return "error";
  const r = m.result();
  if (r) {
    const rows = r.rows.length;
    if (rows === 0 && !m.resultIsQuery()) return "execOk";
    if (rows === 0 && r.columns.length === 0) return "noRows";
    if (rows > 0 && viewIndices(m).length === 0) return "noMatch";
    return "grid";
  }
  return m.running() ? "running" : "none";
}

/** 多语句结果的一个标签:序号 + 行数 / 影响行数 / 错误 / 未执行。 */
function stmtLabel(x: StmtResult, i: number): string {
  const n = i + 1;
  if (x.skipped) return t("tab.stmt_tab_skipped", { n });
  if (x.error !== null) return t("tab.stmt_tab_error", { n });
  if (x.isQuery) return t("tab.stmt_tab_rows", { n, rows: x.result?.rows.length ?? 0 });
  return t("tab.stmt_tab_affected", { n, rows: x.result?.affected ?? 0 });
}

/** 一次执行多条语句时,结果区上方按条切换。 */
function StmtTabs(props: { m: TabModel; list: StmtResult[] }) {
  const m = props.m;
  return (
    <Scroll horizontal class="shrink-0 border-b border-border bg-panel">
      <View testID="stmt-tabs" class="flex-row items-center gap-1 px-2 py-1">
        <Each of={props.list}>
          {(x, i) => (
            <Button
              testID={`stmt-tab:${i() + 1}`}
              selected={m.multiIndex() === i()}
              onPress={() => selectResult(m, i())}
              tooltip={tip(x().sql.replace(/\s+/g, " ").slice(0, 200))}
              class="h-6 flex-row items-center gap-1.5 rounded-(--radius) px-2 hover:bg-list-hover selected:bg-raised"
            >
              <Dot color={x().error !== null ? "destructive" : x().skipped ? "fg-faint" : "data"} />
              <Text
                class={`${FS.xs}`}
                classList={{
                  "font-bold text-foreground": m.multiIndex() === i(),
                  "text-muted-foreground": m.multiIndex() !== i(),
                }}
              >
                {stmtLabel(x(), i())}
              </Text>
            </Button>
          )}
        </Each>
      </View>
    </Scroll>
  );
}

/** 结果主体。 */
export function Results(props: { m: TabModel }) {
  const m = props.m;
  const body = () => bodyOf(m);
  const limited = () =>
    m.browseTable() === null && m.resultIsQuery() && (m.result()?.rows.length ?? 0) >= maxRows();
  // 只对编辑器里自己写的 SQL 提供:表浏览拼出来的分页查询报错,交给 AI 也修不了什么
  const aiReady = () => supportsSql(m.dbKind) && m.kind === "query" && m.browseTable() === null && m.lastRun() !== null;
  const explained = () => aiReady() && m.lastRun()?.explain === true;
  return (
    <View class="min-h-0 flex-1 bg-background">
      <If when={(m.multi()?.length ?? 0) > 1 ? m.multi() : null}>{(list) => <StmtTabs m={m} list={list()} />}</If>
      <If when={m.multi()?.[m.multiIndex()]?.skipped}>
        <Hint class="p-3">{t("tab.stmt_skipped")}</Hint>
      </If>
      <If when={body() === "error"}>
        <ErrorText testID="query-error" mono tone="text-destructive" class="p-3" text={m.err() ?? ""} />
        <If when={aiReady()}>
          <View class="flex-row px-3">
            <Btn testID="ai-fix-error" small icon="sparkles" label={t("tab.ai_fix_error")} onPress={() => aiFixError(m)} />
          </View>
        </If>
      </If>
      <If when={body() === "execOk"}>
        <Text testID="exec-ok" class={`${FS.sm} p-3 text-data`}>
          ✓ {okSummary(m)}
        </Text>
        {/* 没有改到任何行:无需撤销,什么都不显示 */}
        <If when={m.undo() && (m.undo()!.statements.length > 0 || m.undo()!.reason) ? m.undo() : null}>
          {(u) => (
            <If
              when={u().statements.length > 0}
              else={<Text testID="undo-unavailable" class={`${FS.xs} px-3 text-fg-faint`}>{t("undo.unavailable", { reason: u().reason ?? "" })}</Text>}
            >
              <View class="flex-row items-center gap-2 px-3">
                <Btn small variant="outline" icon="undo-2" label={t("undo.button")} onPress={() => m.undoOpen.set(true)} testID="undo" />
                <Text class={`${FS.xs} text-muted-foreground`}>{t("undo.available", { rows: u().rows })}</Text>
              </View>
            </If>
          )}
        </If>
      </If>
      <If when={body() === "noRows"}>
        <Hint class="p-3">{t("tab.query_ok_no_rows")}</Hint>
      </If>
      <If when={body() === "noMatch"}>
        <Text class={`${FS.sm} p-3 text-muted-foreground`}>
          {t("tab.no_filter_match", { total: m.result()?.rows.length ?? 0 })}
        </Text>
      </If>
      <If when={body() === "running"}>
        <Hint class="p-3">{t("tab.running")}</Hint>
      </If>
      <If when={body() === "grid"}>
        <View class="min-h-0 flex-1">
          <If when={explained()}>
            <View class="flex-row items-center gap-2 border-b border-border px-3 py-1.5">
              <Text class={`${FS.xs} flex-1 font-bold text-muted-foreground`}>{t("tab.plan_title")}</Text>
              <Btn
                testID="ai-explain-plan"
                small
                icon="sparkles"
                label={t("tab.ai_explain_plan")}
                onPress={() => aiExplainPlan(m)}
              />
            </View>
          </If>
          <If when={limited()}>
            <Text class={`${FS.xs} border-b border-border px-3 py-1.5 text-warning`}>
              {t("tab.row_limit_reached", { n: maxRows() })}
            </Text>
          </If>
          <ResultGrid m={m} />
        </View>
      </If>
    </View>
  );
}
