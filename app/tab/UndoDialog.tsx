// 撤销最近一次写操作的确认框:列出将执行的反向语句(只读预览),提示期间的并发改动会被覆盖,
// 确认后在一个事务里执行。

import { Text, View } from "nsh";
import { Dialog, Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { Btn, Icon } from "../ui/kit";
import { FS } from "../ui/text";
import { type TabModel, undoLast } from "./model";

/** 预览里最多显示的语句数(太多时只看个开头)。 */
const SHOW = 50;

export function UndoDialog(props: { m: TabModel }) {
  const m = props.m;
  const close = () => m.undoOpen.set(false);
  const statements = () => m.undo()?.statements ?? [];
  const preview = () => statements().slice(0, SHOW).join(";\n") + ";";
  return (
    <Dialog
      testID="undo-dialog"
      open={m.undoOpen() && statements().length > 0}
      onOpenChange={(e) => !e.open && close()}
      closeOnEscape
      animation="fade"
      backdropClass="bg-overlay"
      class="m-auto w-[620px] max-w-[92%] gap-3 rounded-(--radius-lg) border border-border bg-popover p-5 shadow-lg"
    >
      <View class="flex-row items-center gap-2">
        <Icon name="undo-2" size={16} color="foreground" />
        <Text class={`${FS.base} font-bold text-popover-foreground`}>{t("undo.title")}</Text>
      </View>
      <Text class={`${FS.sm} text-popover-foreground`}>
        {t("undo.body", { rows: m.undo()?.rows ?? 0, count: statements().length })}
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
      <View class="flex-row items-start gap-1.5">
        <Icon name="triangle-alert" size={13} color="warning" class="mt-0.5" />
        <Text class={`${FS.xs} flex-1 text-muted-foreground`}>{t("undo.concurrent_warning")}</Text>
      </View>
      <View class="flex-row justify-end gap-2 pt-1">
        <Btn variant="outline" label={t("undo.cancel")} onPress={close} testID="undo-cancel" />
        <Btn variant="primary" label={t("undo.confirm")} onPress={() => void undoLast(m)} testID="undo-confirm" />
      </View>
    </Dialog>
  );
}
