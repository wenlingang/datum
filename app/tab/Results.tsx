// 结果区:状态行(执行中 / 出错 / 成功 · 行数 · 耗时)与结果主体(错误、执行成功、空结果、
// 筛选无命中、行数上限提示、网格)。对应 GPUI 版 tab/mod.rs 的 render_status_line / render_results。

import { Text, View } from "nsh";
import { t } from "nsh/i18n";
import { maxRows } from "../store/prefs";
import { Dot, Hint } from "../ui/kit";
import { FS } from "../ui/text";
import { ResultGrid } from "./Grid";
import { viewIndices, type TabModel } from "./model";

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

/** 结果主体。 */
export function Results(props: { m: TabModel }) {
  const m = props.m;
  const body = () => bodyOf(m);
  const limited = () =>
    m.browseTable() === null && m.resultIsQuery() && !m.mongo && (m.result()?.rows.length ?? 0) >= maxRows();
  return (
    <View class="min-h-0 flex-1 bg-background">
      <If when={body() === "error"}>
        <Text testID="query-error" class={`${FS.sm} p-3 font-mono text-destructive`}>
          {m.err() ?? ""}
        </Text>
      </If>
      <If when={body() === "execOk"}>
        <Text testID="exec-ok" class={`${FS.sm} p-3 text-data`}>
          ✓ {okSummary(m)}
        </Text>
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
