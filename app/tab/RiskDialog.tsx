// 破坏性语句执行前的审查弹窗:逐条列出命中(风险等级 + 语句 + 影响范围预估),
// 标明目标连接与数据库,点「确认执行」放行。所有编辑器执行入口共用。对应 GPUI 版 dialogs/risk.rs。

import { Scroll, Text, View } from "nsh";
import { Button, Dialog, Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { Icon } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import type { ImpactState, RiskGate, TabModel } from "./model";

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
              <If when={g.relational}>
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

/** 风险审查弹窗宿主:模型的 `risk` 非空时显示。 */
export function RiskDialog(props: { m: TabModel }) {
  const m = props.m;
  const cancel = () => m.risk.set(null);
  const confirm = () => {
    const gate = m.risk();
    m.risk.set(null);
    gate?.onConfirm();
  };
  return (
    <Dialog
      testID="risk-dialog"
      open={m.risk() !== null}
      onOpenChange={(e) => !e.open && cancel()}
      closeOnEscape
      animation="fade"
      backdropClass="bg-overlay"
      class="m-auto w-[560px] max-w-[92%] rounded-(--radius-lg) border border-border bg-popover p-5 shadow-lg"
    >
      <If when={m.risk()}>{(g) => <Body gate={g()} onCancel={cancel} onConfirm={confirm} />}</If>
    </Dialog>
  );
}
