// 索引建议 tab:候选索引(DDL、理由、来源)、验证结果(成本前后、是否用上索引),
// 以及选中候选的执行计划前后对比(复用 PlanView)。建索引只经编辑器,执行时照常过风险审查。

import { Clipboard, effect, Scroll, Text, untracked, View } from "nsh";
import { ResizablePanel, ResizablePanelGroup, Tab, Tabs } from "nsh/base";
import { t } from "nsh/i18n";
import type { IndexCandidate, IndexVerify, Plan, Verdict } from "../lib/types";
import { aiStore } from "../store/ai";
import { conns, loadTables } from "../store/conns";
import { toast } from "../store/toast";
import { ErrorText } from "../ui/error";
import { Btn, EnvBadge, Hint, Icon, IconButton } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { type AdvisorModel, fmtCost, runAdvice } from "./advisorModel";
import { openQueryOn } from "./index";
import { PlanView } from "./PlanView";

const VERDICT_CLASS: Record<Verdict, string> = {
  helps: "border-success text-success",
  no_gain: "border-border text-muted-foreground",
  not_used: "border-warning text-warning",
  skipped: "border-border text-muted-foreground",
  error: "border-negative text-negative",
};

function Badge(props: { text: string; class: string; testID?: string }) {
  return (
    <Text testID={props.testID} class={`${FS["2xs"]} rounded-full border px-1.5 font-bold ${props.class}`}>
      {props.text}
    </Text>
  );
}

function pct(v: IndexVerify): string | null {
  const p = v.improvementPct;
  if (p === null) return null;
  const n = Math.abs(p) >= 10 ? Math.round(Math.abs(p)) : Math.abs(p).toFixed(1);
  return p >= 0 ? `−${n}%` : `+${n}%`;
}

/** 验证结果的几行指标。 */
function Metrics(props: { v: IndexVerify; testID?: string }) {
  const v = () => props.v;
  return (
    <View testID={props.testID} class="flex-row flex-wrap items-center gap-x-4 gap-y-1">
      <If when={v().costBefore !== null && v().costAfter !== null}>
        <Text class={`${FS.xs} ${MONO} text-foreground`}>
          {t("advisor.cost", { before: fmtCost(v().costBefore), after: fmtCost(v().costAfter) })}
          <If when={pct(v())}>{(p) => <Text class={v().verdict === "helps" ? "font-bold text-success" : "text-muted-foreground"}>{`  ${p()}`}</Text>}</If>
        </Text>
      </If>
      <If when={v().verdict !== "skipped" && v().verdict !== "error"}>
        <Text class={`${FS.xs} text-muted-foreground`}>
          {t("advisor.uses_index", { answer: v().usesIndex ? t("advisor.answer_yes") : t("advisor.answer_no") })}
        </Text>
        <If when={v().fullScansBefore !== v().fullScansAfter}>
          <Text class={`${FS.xs} text-muted-foreground`}>{t("advisor.full_scans", { before: v().fullScansBefore, after: v().fullScansAfter })}</Text>
        </If>
        <If when={v().sortsBefore !== v().sortsAfter}>
          <Text class={`${FS.xs} text-muted-foreground`}>{t("advisor.sorts", { before: v().sortsBefore, after: v().sortsAfter })}</Text>
        </If>
      </If>
      <If when={v().message}>{(msg) => <Text class={`${FS.xs} text-muted-foreground`}>{msg()}</Text>}</If>
    </View>
  );
}

function CandidateCard(props: { m: AdvisorModel; c: IndexCandidate; i: number }) {
  const c = () => props.c;
  const openLabel = () => (props.m.dbKind === "mysql" ? t("advisor.open_alter") : t("advisor.open_create"));
  const open = () => {
    const notes = [t("advisor.editor_note")];
    if (props.m.dbKind === "pg") notes.push(t("advisor.editor_note_pg"));
    if (props.m.dbKind === "mysql") notes.push(t("advisor.editor_note_mysql"));
    openQueryOn(props.m.connId, `${notes.map((n) => `-- ${n}`).join("\n")}\n${c().editorSql}`);
  };
  const copy = async () => {
    await Clipboard.writeText(c().editorSql);
    toast.success(t("advisor.copied"));
  };
  return (
    <View testID={`advisor-candidate:${props.i}`} class="gap-1.5 rounded-(--radius) border border-border bg-panel px-3 py-2.5">
      <View class="flex-row flex-wrap items-center gap-2">
        <Badge
          text={c().source === "ai" ? t("advisor.source_ai") : t("advisor.source_rule")}
          class={c().source === "ai" ? "border-ai text-ai" : "border-border text-muted-foreground"}
        />
        <Text class={`${FS.sm} flex-1 font-bold text-foreground line-clamp-1`}>{c().table}</Text>
        <If when={c().verify} else={<Badge testID={`advisor-verdict:${props.i}`} text={t("advisor.verdict.unverified")} class="border-border text-muted-foreground" />}>
          {(v) => <Badge testID={`advisor-verdict:${props.i}`} text={t(`advisor.verdict.${v().verdict}`)} class={VERDICT_CLASS[v().verdict]} />}
        </If>
      </View>
      <Text testID={`advisor-ddl:${props.i}`} class={`${FS.xs} ${MONO} text-foreground`}>{c().ddl}</Text>
      <Text class={`${FS.xs} text-muted-foreground`}>{c().reason}</Text>
      <If when={c().verify}>{(v) => <Metrics v={v()} testID={`advisor-metrics:${props.i}`} />}</If>
      <View class="flex-row items-center gap-1.5 pt-0.5">
        <Btn testID={`advisor-open:${props.i}`} small variant="outline" icon="square-play" label={openLabel()} tooltip={t("advisor.open_tooltip")} onPress={open} />
        <IconButton testID={`advisor-copy:${props.i}`} icon="copy" tooltip={t("advisor.copy")} onPress={() => void copy()} />
      </View>
    </View>
  );
}

/** 计划对比:左边现在的计划,右边加上所选候选(或组合)后的计划。 */
function Compare(props: { m: AdvisorModel }) {
  const advice = () => props.m.advice();
  const verified = () => (advice()?.candidates ?? []).map((c, i) => ({ c, i })).filter((x) => x.c.verify?.plan);
  const after = (): { plan: Plan; label: string } | null => {
    const a = advice();
    if (!a) return null;
    const k = props.m.compare();
    if (k === -1 && a.combo?.verify.plan) return { plan: a.combo.verify.plan, label: t("advisor.combo_tab") };
    const c = a.candidates[k];
    return c?.verify?.plan ? { plan: c.verify.plan, label: c.name } : null;
  };
  // 选中的候选没有计划(未验证)时,换到第一个有计划的
  effect(() => {
    const list = verified();
    const k = props.m.compare();
    untracked(() => {
      if (k === -1 || list.length === 0 || list.some((x) => x.i === k)) return;
      props.m.compare.set(list[0].i);
    });
  });
  return (
    <View class="min-h-0 flex-1">
      <View class="h-9 shrink-0 flex-row items-center gap-3 border-b border-border px-4">
        <Text class={`${FS.xs} font-bold text-muted-foreground`}>{t("advisor.compare")}</Text>
        <If when={verified().length > 0}>
          <Scroll horizontal class="min-w-0 flex-1">
            <Tabs class="flex-row gap-1" indicatorClass="bottom-0 h-[2px] bg-primary">
              <Each of={verified()} key={(x) => x.c.name}>
                {(x) => (
                  <Tab
                    testID={`advisor-compare:${x().i}`}
                    selected={props.m.compare() === x().i}
                    onPress={() => props.m.compare.set(x().i)}
                    class="h-9 justify-center px-2"
                  >
                    <Text class={`${FS.xs} ${MONO}`} classList={{ "font-bold text-foreground": props.m.compare() === x().i, "text-muted-foreground": props.m.compare() !== x().i }}>
                      {x().c.name}
                    </Text>
                  </Tab>
                )}
              </Each>
              <If when={advice()?.combo?.verify.plan}>
                <Tab testID="advisor-compare:combo" selected={props.m.compare() === -1} onPress={() => props.m.compare.set(-1)} class="h-9 justify-center px-2">
                  <Text class={FS.xs} classList={{ "font-bold text-foreground": props.m.compare() === -1, "text-muted-foreground": props.m.compare() !== -1 }}>
                    {t("advisor.combo_tab")}
                  </Text>
                </Tab>
              </If>
            </Tabs>
          </Scroll>
        </If>
      </View>
      <View class="min-h-0 flex-1 flex-row">
        <View testID="advisor-plan-before" class="min-w-0 flex-1 border-r border-border">
          <Text class={`${FS.xs} px-4 pt-2 font-bold text-muted-foreground`}>{t("advisor.plan_before")}</Text>
          <If
            when={advice()?.baseline?.tree ? advice()!.baseline : null}
            else={<Hint class={`${FS.xs} px-4 py-2`}>{advice()?.baselineError ? t("advisor.baseline_failed", { error: advice()!.baselineError! }) : t("advisor.no_plan_tree")}</Hint>}
          >
            {(p) => <PlanView plan={p()} />}
          </If>
        </View>
        <View testID="advisor-plan-after" class="min-w-0 flex-1">
          <Text class={`${FS.xs} px-4 pt-2 font-bold text-muted-foreground line-clamp-1`}>
            {after() ? t("advisor.plan_after", { name: after()!.label }) : t("advisor.plan_after_empty")}
          </Text>
          <If when={after()?.plan.tree ? after()!.plan : null} else={<Hint class={`${FS.xs} px-4 py-2`}>{t("advisor.no_after_plan")}</Hint>}>
            {(p) => <PlanView plan={p()} />}
          </If>
        </View>
      </View>
    </View>
  );
}

export function IndexAdvisor(props: { m: AdvisorModel }) {
  const m = props.m;
  const conn = () => conns.get(m.connId);
  const advice = () => m.advice();
  const verifiedMode = () => advice()?.mode === "hypopg" || advice()?.mode === "sqlite_txn";

  // 连接还没建立时先连上,连上后分析一次(之后「重新分析」手动触发)
  let started = false;
  let triedConnect = false;
  effect(() => {
    if (!conns.isConnected(m.connId)) {
      if (!triedConnect && !conns.isLoading(m.connId)) {
        triedConnect = true;
        void loadTables(m.connId);
      }
      return;
    }
    if (started) return;
    started = true;
    untracked(() => void runAdvice(m));
  });

  return (
    <View testID="index-advisor" class="min-h-0 flex-1 bg-background">
      <View class="shrink-0 gap-2 border-b border-border px-5 py-3">
        <View class="flex-row flex-wrap items-center gap-2">
          <Icon name="scan-search" size={16} color="data" />
          <Text class={`${FS.base} font-bold text-foreground`}>{t("advisor.title")}</Text>
          <If when={conn()}>{(c) => <EnvBadge env={c().env} />}</If>
          <Text class={`${FS.sm} text-muted-foreground`}>{t("advisor.subtitle")}</Text>
          <View class="flex-1" />
          <Btn
            testID="advisor-ai"
            small
            variant="ai"
            icon={m.aiBusy() ? "loader-circle" : "sparkles"}
            label={t("advisor.ask_ai")}
            tooltip={aiStore.ready() ? t("advisor.ask_ai_tooltip") : t("advisor.ai_not_ready")}
            disabled={!aiStore.ready() || m.busy()}
            onPress={() => void runAdvice(m, true)}
          />
          <Btn
            testID="advisor-rerun"
            small
            icon={m.busy() && !m.aiBusy() ? "loader-circle" : "refresh-cw"}
            label={t("advisor.rerun")}
            disabled={m.busy()}
            onPress={() => void runAdvice(m)}
          />
        </View>
        {/* 多行语句压成一行再截断:直接截断会吞掉超出部分的换行,词与词粘在一起 */}
        <Text testID="advisor-sql" class={`${FS.xs} ${MONO} text-muted-foreground line-clamp-3`}>{m.statement.replace(/\s+/g, " ").trim()}</Text>
        <If when={advice()}>
          {(a) => (
            <View class="gap-1.5">
              <View class="flex-row flex-wrap items-center gap-2">
                <Badge
                  testID="advisor-mode"
                  text={verifiedMode() ? t("advisor.mode_verified") : t("advisor.mode_unverified")}
                  class={verifiedMode() ? "border-success text-success" : "border-warning text-warning"}
                />
                <Text testID="advisor-note" class={`${FS.xs} min-w-0 flex-1 text-muted-foreground`}>{a().note}</Text>
                <If when={a().installSql}>
                  {(sql) => (
                    <Btn
                      testID="advisor-install"
                      small
                      variant="outline"
                      icon="square-play"
                      label={t("advisor.install_hypopg")}
                      onPress={() => openQueryOn(m.connId, `-- ${t("advisor.install_note")}\n${sql()}`)}
                    />
                  )}
                </If>
              </View>
              <If when={a().ai}>
                {(ai) => (
                  <View testID="advisor-ai-outcome" class="gap-0.5">
                    <If when={ai().error} else={<Text class={`${FS.xs} text-muted-foreground`}>{t("advisor.ai_added", { n: ai().added })}</Text>}>
                      {(e) => <ErrorText text={t("advisor.ai_failed", { error: e() })} />}
                    </If>
                    <If when={ai().dropped.length > 0}>
                      <Text class={`${FS.xs} text-muted-foreground`}>{t("advisor.ai_dropped", { list: ai().dropped.join(", ") })}</Text>
                    </If>
                    <If when={ai().masked}>
                      <Text class={`${FS.xs} text-muted-foreground`}>{t("advisor.ai_masked")}</Text>
                    </If>
                  </View>
                )}
              </If>
            </View>
          )}
        </If>
      </View>

      <If when={m.err()}>{(e) => <ErrorText testID="advisor-error" class="px-5 pt-3" text={e()} />}</If>
      <If
        when={advice()}
        else={
          <View class="flex-1 items-center justify-center p-6">
            <Hint>{m.busy() ? t("advisor.running") : ""}</Hint>
          </View>
        }
      >
        {(a) => (
          <ResizablePanelGroup axis="vertical" class="min-h-0 flex-1" handleClass="bg-border">
            <ResizablePanel size={300} minSize={120}>
              <Scroll class="flex-1" contentClass="gap-2.5 px-5 py-3">
                <If when={a().candidates.length === 0}>
                  <Text testID="advisor-empty" class={`${FS.sm} text-muted-foreground`}>{t("advisor.no_candidates")}</Text>
                </If>
                <Each of={a().candidates} key={(c) => `${c.source}:${c.ddl}`}>
                  {(c, i) => <CandidateCard m={m} c={c()} i={i()} />}
                </Each>
                <If when={a().combo}>
                  {(combo) => (
                    <View testID="advisor-combo" class="gap-1.5 rounded-(--radius) border border-border bg-panel px-3 py-2.5">
                      <View class="flex-row items-center gap-2">
                        <Text class={`${FS.sm} flex-1 font-bold text-foreground`}>{t("advisor.combo_title")}</Text>
                        <Badge text={t(`advisor.verdict.${combo().verify.verdict}`)} class={VERDICT_CLASS[combo().verify.verdict]} />
                      </View>
                      <Text class={`${FS.xs} ${MONO} text-muted-foreground`}>{combo().names.join(" + ")}</Text>
                      <Metrics v={combo().verify} />
                      <If when={combo().used.length > 0}>
                        <Text class={`${FS.xs} text-muted-foreground`}>{t("advisor.combo_used", { names: combo().used.join(", ") })}</Text>
                      </If>
                    </View>
                  )}
                </If>
              </Scroll>
            </ResizablePanel>
            <ResizablePanel minSize={120}>
              <Compare m={m} />
            </ResizablePanel>
          </ResizablePanelGroup>
        )}
      </If>
    </View>
  );
}
