// 破坏性语句执行前的审查弹窗:逐条列出命中(风险等级 + 语句),标明目标连接与数据库,
// 点「确认执行」放行。所有编辑器执行入口共用。对应 GPUI 版 dialogs/risk.rs。

import { Text, View } from "nsh";
import { Button, Dialog, Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { Icon } from "../ui/kit";
import { FS } from "../ui/text";
import type { RiskGate, TabModel } from "./model";

function Body(props: { gate: RiskGate; onCancel: () => void; onConfirm: () => void }) {
  const g = props.gate;
  const target = g.database
    ? t("risk.target_db", { db: g.database, count: g.findings.length })
    : t("risk.target_conn", { target: g.targetLabel, count: g.findings.length });
  return (
    <View class="gap-3">
      <View class="flex-row items-start gap-3">
        <Icon name="triangle-alert" size={18} color="destructive" class="mt-0.5" />
        <View class="min-w-0 flex-1 gap-1.5">
          <Text class={`${FS.base} font-bold text-popover-foreground`}>{t("risk.title")}</Text>
          <Text class={`${FS.sm} text-muted-foreground`}>{target}</Text>
        </View>
      </View>
      <View class="max-h-[360px] gap-2 pl-[30px]">
        <Each of={g.findings}>
          {(f) => (
            <View class="relative overflow-hidden rounded-(--radius) border border-border">
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
              <View
                class="absolute right-1.5 top-1 rounded-full px-2"
                classList={{ "bg-destructive": f().level === "danger", "bg-warning": f().level !== "danger" }}
              >
                <Text class="font-bold text-destructive-foreground" style={{ fontSize: 10.5 }}>
                  {f().level === "danger" ? t("risk.danger") : t("risk.warning")}
                </Text>
              </View>
            </View>
          )}
        </Each>
        <View class="flex-row items-center gap-1.5">
          <Icon name="triangle-alert" size={13} color="negative" />
          <Text class={`${FS.xs} font-bold text-negative`}>{t("risk.irreversible")}</Text>
        </View>
      </View>
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
