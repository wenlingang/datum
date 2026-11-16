// 撤销的确认框:列出将执行的反向语句(只读预览),提示期间的并发改动会被覆盖,确认后在一个事务里执行。
// 查询标签页撤销最近一次写操作、查询历史与写操作记录里的撤销都用它;来自写操作记录的撤销
// 多一块核对结果(之前运行的、之后又改了同一张表、要恢复的行还在不在)。

import { Text, View } from "nsh";
import { Dialog, Editor } from "nsh/base";
import { t } from "nsh/i18n";
import type { UndoCheck, UndoPrep } from "../lib/types";
import { Btn, Icon } from "../ui/kit";
import { FS } from "../ui/text";
import { type TabModel, undoLast } from "./model";

/** 预览里最多显示的语句数(太多时只看个开头)。 */
const SHOW = 50;

export function UndoDialog(props: { m: TabModel }) {
  const m = props.m;
  return (
    <UndoConfirm
      open={m.undoOpen()}
      undo={m.undo()}
      onClose={() => m.undoOpen.set(false)}
      onConfirm={() => void undoLast(m)}
    />
  );
}

/** 撤销确认框本体。 */
export function UndoConfirm(props: { open: boolean; undo: UndoPrep | null; busy?: boolean; onClose: () => void; onConfirm: () => void }) {
  const statements = () => props.undo?.statements ?? [];
  const preview = () => statements().slice(0, SHOW).join(";\n") + ";";
  const fromJournal = () => props.undo?.journal ?? null;
  const blocked = () => !!fromJournal()?.check?.undoneBy;
  return (
    <Dialog
      testID="undo-dialog"
      open={props.open && statements().length > 0}
      onOpenChange={(e) => !e.open && props.onClose()}
      closeOnEscape
      animation="fade"
      backdropClass="bg-overlay"
      class="m-auto w-[620px] max-w-[92%] gap-3 rounded-(--radius-lg) border border-border bg-popover p-5 shadow-lg"
    >
      <View class="flex-row items-center gap-2">
        <Icon name="undo-2" size={16} color="foreground" />
        <Text class={`${FS.base} font-bold text-popover-foreground`}>
          {fromJournal() ? t("journal.undo_title") : t("undo.title")}
        </Text>
      </View>
      <Text class={`${FS.sm} text-popover-foreground`}>
        {t("undo.body", { rows: props.undo?.rows ?? 0, count: statements().length })}
      </Text>
      <View class="max-h-[320px] overflow-hidden rounded-(--radius) border border-border">
        <Editor
          class="bg-editor"
          style={{ height: Math.min(14, Math.max(2, preview().split("\n").length)) * 20 + 16 }}
          value={preview()}
          readOnly
          lineNumbers={false}
          folding={false}
          softWrap
          activeLine={false}
          language="sql"
          fontSize={12}
        />
      </View>
      <If when={statements().length > SHOW}>
        <Text class={`${FS.xs} text-fg-faint`}>{t("undo.more", { count: statements().length, shown: SHOW })}</Text>
      </If>
      <If when={fromJournal()}>
        {(j) => (
          <If
            when={j().checking ? null : j().check}
            else={<Notice tone="muted" text={t("journal.check_running")} testID="undo-check-running" />}
          >
            {(c) => <CheckNotices check={c()} />}
          </If>
        )}
      </If>
      <View class="flex-row items-start gap-1.5">
        <Icon name="triangle-alert" size={13} color="warning" class="mt-0.5" />
        <Text class={`${FS.xs} flex-1 text-muted-foreground`}>{t("undo.concurrent_warning")}</Text>
      </View>
      <View class="flex-row justify-end gap-2 pt-1">
        <Btn variant="outline" label={t("undo.cancel")} onPress={props.onClose} testID="undo-cancel" />
        <Btn
          variant="primary"
          label={t("undo.confirm")}
          disabled={!!props.busy || blocked()}
          onPress={props.onConfirm}
          testID="undo-confirm"
        />
      </View>
    </Dialog>
  );
}

function Notice(props: { tone: "warning" | "muted" | "ok"; text: string; testID?: string }) {
  const icon = () => (props.tone === "warning" ? "triangle-alert" : props.tone === "ok" ? "check" : "info");
  const color = () => (props.tone === "warning" ? "warning" : props.tone === "ok" ? "success" : "muted-foreground");
  return (
    <View testID={props.testID} class="flex-row items-start gap-1.5">
      <Icon name={icon()} size={13} color={color()} class="mt-0.5" />
      <Text class={`${FS.xs} flex-1`} classList={{ "text-foreground": props.tone === "warning", "text-muted-foreground": props.tone !== "warning" }}>
        {props.text}
      </Text>
    </View>
  );
}

/** 写操作记录撤销前的核对结果。 */
function CheckNotices(props: { check: UndoCheck }) {
  const c = () => props.check;
  const clean = () =>
    c().checked && c().missing === 0 && c().existing === 0 && c().laterWrites === 0 && !c().previousSession && !c().error;
  return (
    <View testID="undo-check" class="gap-1.5 rounded-(--radius) border border-border bg-panel p-2.5">
      <If when={c().undoneBy}>
        <Notice tone="warning" text={t("journal.already_undone")} />
      </If>
      <If when={c().previousSession}>
        <Notice tone="warning" text={t("journal.check_previous_session")} testID="undo-check-previous" />
      </If>
      <If when={c().laterWrites > 0}>
        <Notice tone="warning" text={t("journal.check_later_writes", { count: c().laterWrites })} />
      </If>
      <If when={c().missing > 0}>
        <Notice tone="warning" text={t("journal.check_missing", { count: c().missing })} />
      </If>
      <If when={c().existing > 0}>
        <Notice tone="warning" text={t("journal.check_existing", { count: c().existing })} />
      </If>
      <If when={c().error}>
        {(e) => <Notice tone="warning" text={t("journal.check_error", { error: e() })} />}
      </If>
      <If when={!c().checked && !c().error}>
        <Notice tone="warning" text={t("journal.check_unverified")} />
      </If>
      <If when={c().checked && c().missing === 0 && c().existing === 0}>
        <Notice tone={clean() ? "ok" : "muted"} text={t("journal.check_rows_ok")} testID="undo-check-ok" />
      </If>
    </View>
  );
}
