// 破坏性语句执行前的审查弹窗:逐条列出命中(风险等级 + 语句 + 影响范围预估),
// 标明目标连接与数据库,点「确认执行」放行。所有编辑器执行入口共用。对应 GPUI 版 dialogs/risk.rs。

import { Clipboard, Scroll, Text, View } from "nsh";
import { Button, Dialog, Editor } from "nsh/base";
import { t } from "nsh/i18n";
import type { AlterAlgorithm, AlterAssessment, AlterOpVerdict, AlterRisk } from "../lib/types";
import { toast } from "../store/toast";
import { Icon } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import type { AlterState, ImpactState, RiskGate, TabModel } from "./model";
import { formatBytes } from "./RedisInsight";

/** 预览表格最多显示的列数(其余列折叠,避免弹窗被宽表撑开)。 */
const PREVIEW_COLS = 6;

/** 影响范围:行数一句话 + 前几行预览。 */
function Impact(props: { s: ImpactState | undefined }) {
  const s = () => props.s;
  const line = () => {
    const v = s();
    if (!v || v.state === "loading") return t("risk.impact_loading");
    if (v.state === "none") return t("risk.impact_none");
    if (v.state === "error") return t("risk.impact_failed", { error: v.error });
    const n = v.impact.count.toLocaleString();
    if (v.impact.count === 0) return v.impact.wholeTable ? t("risk.impact_table_empty") : t("risk.impact_zero");
    return v.impact.wholeTable ? t("risk.impact_table", { count: n }) : t("risk.impact_rows", { count: n });
  };
  const ok = () => {
    const v = s();
    return v?.state === "ok" ? v.impact : null;
  };
  const cols = () => (ok()?.columns ?? []).slice(0, PREVIEW_COLS);
  const hidden = () => Math.max(0, (ok()?.columns.length ?? 0) - PREVIEW_COLS);
  return (
    <View testID="risk-impact" class="gap-1.5 border-t border-border bg-background px-2.5 py-2">
      <View class="flex-row items-center gap-1.5">
        <Icon
          name={s()?.state === "loading" ? "loader-circle" : s()?.state === "ok" ? "table" : "info"}
          size={12}
          color={s()?.state === "error" ? "negative" : "muted-foreground"}
        />
        <Text
          class={`${FS.xs} min-w-0 flex-1`}
          classList={{
            "font-bold text-foreground": s()?.state === "ok" && (ok()?.count ?? 0) > 0,
            "text-negative": s()?.state === "error",
            "text-muted-foreground": s()?.state !== "error" && !((ok()?.count ?? 0) > 0),
          }}
        >
          {line()}
        </Text>
      </View>
      <If when={(ok()?.rows.length ?? 0) > 0}>
        <Scroll horizontal class="max-h-[150px]">
          <View class="gap-0.5">
            <View class="flex-row gap-3">
              <Each of={cols()}>
                {(c) => (
                  <Text class={`${FS.xs} ${MONO} w-[110px] font-bold text-muted-foreground line-clamp-1`}>{c()}</Text>
                )}
              </Each>
              <If when={hidden() > 0}>
                <Text class={`${FS.xs} text-fg-faint`}>{t("risk.impact_more_cols", { count: hidden() })}</Text>
              </If>
            </View>
            <Each of={ok()?.rows ?? []}>
              {(r) => (
                <View class="flex-row gap-3">
                  <Each of={cols()}>
                    {(_, ci) => (
                      <Text class={`${FS.xs} ${MONO} w-[110px] text-foreground line-clamp-1`}>
                        {r()[ci()] ?? "NULL"}
                      </Text>
                    )}
                  </Each>
                </View>
              )}
            </Each>
          </View>
        </Scroll>
        <If when={(ok()?.count ?? 0) > (ok()?.rows.length ?? 0)}>
          <Text class={`${FS.xs} text-fg-faint`}>
            {t("risk.impact_preview_of", { shown: ok()?.rows.length ?? 0, count: ok()?.count.toLocaleString() ?? "" })}
          </Text>
        </If>
      </If>
    </View>
  );
}

const ALGORITHM: Record<AlterAlgorithm, string> = { instant: "INSTANT", inplace: "INPLACE", copy: "COPY" };

/** 风险档位的底色(主题 token)。 */
const RISK_BG: Record<AlterRisk, string> = {
  low: "bg-success",
  medium: "bg-warning",
  high: "bg-destructive",
  unknown: "bg-muted-foreground",
};

const RISK_FG: Record<AlterRisk, string> = {
  low: "text-success-foreground",
  medium: "text-warning-foreground",
  high: "text-destructive-foreground",
  unknown: "text-background",
};

/** 估算行数只给两位有效数字:TABLE_ROWS 本来就是估计值,精确到个位反而误导。 */
function approxRows(n: number): string {
  if (n < 10000) return n.toLocaleString();
  const p = 10 ** (Math.floor(Math.log10(n)) - 1);
  return (Math.round(n / p) * p).toLocaleString();
}

/** 整条语句的结论:算法 · 是否重建 · 是否阻塞写入。 */
function summary(a: AlterAssessment): string {
  if (a.fails) return t("risk.alter_summary_fails", { requested: a.requested ?? "" });
  if (!a.algorithm) return t("risk.alter_summary_unknown");
  const parts = [ALGORITHM[a.algorithm]];
  if (a.rebuild !== null) parts.push(a.rebuild ? t("risk.alter_rebuild") : t("risk.alter_no_rebuild"));
  if (a.blocksWrites !== null) parts.push(a.blocksWrites ? t("risk.alter_blocks_writes") : t("risk.alter_online"));
  return parts.join(" · ");
}

/** 表大小与耗时量级(都是估计)。 */
function sizeLine(a: AlterAssessment): string {
  const parts = [a.server];
  if (a.rows !== null) parts.push(t("risk.alter_rows", { count: approxRows(a.rows) }));
  if (a.bytes !== null) parts.push(formatBytes(a.bytes));
  return parts.join(" · ");
}

function durationLine(a: AlterAssessment): string | null {
  if (!a.duration || !a.algorithm) return null;
  return t(`risk.alter_duration.${a.duration}`, { algorithm: ALGORITHM[a.algorithm] });
}

/** 一个子操作:算法标签 + 子句 + 依据。 */
function OpRow(props: { op: AlterOpVerdict }) {
  const o = () => props.op;
  const detail = () => {
    const what = t(`risk.alter_op.${o().op}`) + (o().target ? ` ${o().target}` : "");
    return o().note ? `${what} — ${t(`risk.alter_note.${o().note}`)}` : what;
  };
  return (
    <View class="flex-row items-start gap-2">
      <View
        class="mt-0.5 w-[58px] items-center rounded-(--radius) border px-1"
        classList={{
          "border-success": o().algorithm === "instant",
          "border-warning": o().algorithm === "inplace",
          "border-destructive": o().algorithm === "copy",
          "border-border": o().algorithm === null,
        }}
      >
        <Text
          class="font-bold"
          classList={{
            "text-success": o().algorithm === "instant",
            "text-warning": o().algorithm === "inplace",
            "text-destructive": o().algorithm === "copy",
            "text-muted-foreground": o().algorithm === null,
          }}
          style={{ fontSize: 10 }}
        >
          {o().algorithm ? ALGORITHM[o().algorithm!] : "?"}
        </Text>
      </View>
      <View class="min-w-0 flex-1 gap-0.5">
        <Text class={`${FS.xs} ${MONO} text-foreground line-clamp-1`}>{o().clause}</Text>
        <Text class={`${FS.xs} text-muted-foreground`}>{detail()}</Text>
      </View>
      <If when={o().concurrentDml === false}>
        <Icon name="lock" size={12} color="destructive" class="mt-0.5" />
      </If>
    </View>
  );
}

/** 改表结构的风险评估:每个子操作的判定、整体结论、表大小与耗时量级、建议。 */
function Alter(props: { s: AlterState | undefined }) {
  const s = () => props.s;
  const ok = () => {
    const v = s();
    return v?.state === "ok" ? v.assessment : null;
  };
  const copyGuarded = async (text: string) => {
    await Clipboard.writeText(text);
    toast.info(t("risk.alter_copied"));
  };
  return (
    <View testID="risk-alter" class="gap-2 border-t border-border bg-background px-2.5 py-2">
      <If when={s()?.state !== "ok"}>
        <View class="flex-row items-center gap-1.5">
          <Icon
            name={s()?.state === "loading" || !s() ? "loader-circle" : "info"}
            size={12}
            color={s()?.state === "error" ? "negative" : "muted-foreground"}
          />
          <Text
            class={`${FS.xs} min-w-0 flex-1`}
            classList={{ "text-negative": s()?.state === "error", "text-muted-foreground": s()?.state !== "error" }}
          >
            {(() => {
              const v = s();
              if (!v || v.state === "loading") return t("risk.alter_loading");
              if (v.state === "error") return t("risk.alter_failed", { error: v.error });
              return t("risk.alter_none");
            })()}
          </Text>
        </View>
      </If>
      <If when={ok()}>
        {(a) => (
          <View class="gap-2">
            <View class="flex-row items-center gap-2">
              <View testID="risk-alter-risk" class={`rounded-full px-2 ${RISK_BG[a().risk]}`}>
                <Text class={`font-bold ${RISK_FG[a().risk]}`} style={{ fontSize: 10.5 }}>
                  {t(`risk.alter_risk.${a().risk}`)}
                </Text>
              </View>
              <Text class={`${FS.xs} min-w-0 flex-1 font-bold text-foreground`}>{summary(a())}</Text>
            </View>
            <View class="gap-0.5">
              <Text class={`${FS.xs} text-muted-foreground`}>{sizeLine(a())}</Text>
              <If when={durationLine(a())}>{(d) => <Text class={`${FS.xs} text-muted-foreground`}>{d()}</Text>}</If>
              <Each of={a().notes}>
                {(n) => <Text class={`${FS.xs} text-muted-foreground`}>{t(`risk.alter_note.${n()}`)}</Text>}
              </Each>
            </View>
            <View class="gap-1.5">
              <Each of={a().ops}>{(o) => <OpRow op={o()} />}</Each>
            </View>
            <If when={a().fails}>
              {(f) => <Text class={`${FS.xs} text-muted-foreground`}>{t(`risk.alter_fails.${f()}`)}</Text>}
            </If>
            <If when={a().suggestOsc}>
              <View class="flex-row items-start gap-1.5">
                <Icon name="triangle-alert" size={12} color="negative" class="mt-0.5" />
                <Text class={`${FS.xs} min-w-0 flex-1 text-negative`}>{t("risk.alter_suggest_osc")}</Text>
              </View>
            </If>
            <If when={a().risk !== "low" ? a().guarded : null}>
              {(g) => (
                <View class="gap-1">
                  <View class="flex-row items-center gap-2">
                    <Text class={`${FS.xs} min-w-0 flex-1 text-muted-foreground`}>{t("risk.alter_guarded")}</Text>
                    <Button
                      testID="risk-alter-copy"
                      onPress={() => void copyGuarded(g())}
                      class="h-6 flex-row items-center gap-1 rounded-(--radius) border border-input px-2 hover:bg-list-hover"
                    >
                      <Icon name="copy" size={11} />
                      <Text class={`${FS.xs} text-foreground`}>{t("risk.alter_copy")}</Text>
                    </Button>
                  </View>
                  <Text class={`${FS.xs} ${MONO} rounded-(--radius) bg-editor px-2 py-1 text-foreground`}>{g()}</Text>
                </View>
              )}
            </If>
            <Text class={`${FS.xs} text-fg-faint`}>{t("risk.alter_mdl")}</Text>
          </View>
        )}
      </If>
    </View>
  );
}

function Body(props: { gate: RiskGate; onCancel: () => void; onConfirm: () => void }) {
  const g = props.gate;
  // 有高危语句才说「具有破坏性」;只是要核对的写操作(如 prod 上带 WHERE 的 UPDATE)换个说法
  const danger = g.findings.some((f) => f.level === "danger");
  const count = g.findings.length;
  const target = g.database
    ? t(danger ? "risk.target_db" : "risk.target_db_review", { db: g.database, count })
    : t(danger ? "risk.target_conn" : "risk.target_conn_review", { target: g.targetLabel, count });
  return (
    <View class="gap-3">
      <View class="flex-row items-start gap-3">
        <Icon name="triangle-alert" size={18} color="destructive" class="mt-0.5" />
        <View class="min-w-0 flex-1 gap-1.5">
          <Text class={`${FS.base} font-bold text-popover-foreground`}>{t("risk.title")}</Text>
          <Text class={`${FS.sm} text-muted-foreground`}>{target}</Text>
          <If when={g.note}>{(n) => <Text class={`${FS.xs} text-muted-foreground`}>{n()}</Text>}</If>
        </View>
      </View>
      <Scroll class="min-h-0 shrink pl-[30px]" style={{ maxHeight: 460 }} contentClass="gap-2">
        <Each of={g.findings}>
          {(f, i) => (
            <View class="overflow-hidden rounded-(--radius) border border-border">
              {/* 等级与类型放在语句上方,不压住 SQL 末尾 */}
              <View class="flex-row border-b border-border bg-background px-2 py-1">
                <View
                  class="flex-row gap-1.5 self-start rounded-full px-2"
                  classList={{ "bg-destructive": f().level === "danger", "bg-warning": f().level !== "danger" }}
                >
                  <Text class="font-bold text-destructive-foreground" style={{ fontSize: 10.5 }}>
                    {f().level === "danger" ? t("risk.danger") : t("risk.warning")}
                  </Text>
                  <Text class="text-destructive-foreground" style={{ fontSize: 10.5 }}>
                    {f().kind}
                  </Text>
                </View>
              </View>
              <Editor
                class="bg-editor"
                style={{ height: Math.min(8, Math.max(1, f().sql.split("\n").length)) * 20 + 16 }}
                value={f().sql}
                readOnly
                lineNumbers={false}
                folding={false}
                softWrap
                activeLine={false}
                language={g.relational ? "sql" : "text"}
                fontSize={13}
              />
              <If when={f().schemaChange}>
                <Alter s={g.alters()[i()]} />
              </If>
              <If when={g.relational && !f().schemaChange && !g.noImpact}>
                <Impact s={g.impacts()[i()]} />
              </If>
            </View>
          )}
        </Each>
        <View class="flex-row items-center gap-1.5">
          <Icon name="triangle-alert" size={13} color="negative" />
          <Text class={`${FS.xs} font-bold text-negative`}>{t("risk.irreversible")}</Text>
        </View>
      </Scroll>
      <View class="flex-row justify-end gap-2 pt-1">
        <Button
          testID="risk-cancel"
          onPress={props.onCancel}
          class="h-8 items-center justify-center rounded-(--radius) border border-input px-3.5 hover:bg-list-hover"
        >
          <Text class={`${FS.sm} font-medium text-foreground`}>{t("risk.cancel")}</Text>
        </Button>
        <Button
          testID="risk-confirm"
          onPress={props.onConfirm}
          class="h-8 items-center justify-center rounded-(--radius) bg-destructive/20 px-3.5 hover:bg-destructive/30"
        >
          <Text class={`${FS.sm} font-medium text-destructive`}>{t("risk.confirm")}</Text>
        </Button>
      </View>
    </View>
  );
}

/** 风险审查弹窗:`gate` 非空时显示。编辑器之外的执行入口(如 SQLite 改表结构)也用它。 */
export function RiskGateDialog(props: { gate: RiskGate | null; onCancel: () => void; onConfirm: () => void }) {
  return (
    <Dialog
      testID="risk-dialog"
      open={props.gate !== null}
      onOpenChange={(e) => !e.open && props.onCancel()}
      closeOnEscape
      animation="fade"
      backdropClass="bg-overlay"
      class="m-auto w-[560px] max-w-[92%] rounded-(--radius-lg) border border-border bg-popover p-5 shadow-lg"
    >
      <If when={props.gate}>{(g) => <Body gate={g()} onCancel={props.onCancel} onConfirm={props.onConfirm} />}</If>
    </Dialog>
  );
}

/** 风险审查弹窗宿主:模型的 `risk` 非空时显示。 */
export function RiskDialog(props: { m: TabModel }) {
  const m = props.m;
  const cancel = () => m.risk.set(null);
  const confirm = () => {
    const gate = m.risk();
    m.risk.set(null);
    gate?.onConfirm();
  };
  return <RiskGateDialog gate={m.risk()} onCancel={cancel} onConfirm={confirm} />;
}
