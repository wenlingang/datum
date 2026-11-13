// 查询参数弹框:SQL 里的每个 `:name` 一个输入框,下方实时预览代入后的 SQL,确认后执行。
// 数字原样代入,NULL 为空值,其余作为字符串(单引号自动转义)。

import { Text, View } from "nsh";
import { Dialog } from "nsh/base";
import { t } from "nsh/i18n";
import { sql as sqlText } from "../lib/native";
import { Btn, Icon, TextField } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { confirmParams, type TabModel } from "./model";

/** 预览最多显示的字符数。 */
const PREVIEW_CHARS = 600;

export function ParamsDialog(props: { m: TabModel }) {
  const m = props.m;
  const gate = () => m.params();
  const close = () => m.params.set(null);
  const preview = () => {
    const g = gate();
    if (!g) return "";
    const s = sqlText.bindParams(g.sql, g.values(), m.dbKind);
    return s.length > PREVIEW_CHARS ? `${s.slice(0, PREVIEW_CHARS)}…` : s;
  };
  return (
    <Dialog
      testID="params-dialog"
      open={gate() !== null}
      onOpenChange={(e) => !e.open && close()}
      closeOnEscape
      animation="fade"
      backdropClass="bg-overlay"
      class="m-auto w-[560px] max-w-[92%] gap-3 rounded-(--radius-lg) border border-border bg-popover p-5 shadow-lg"
    >
      <View class="flex-row items-center gap-2">
        <Icon name="braces" size={16} color="foreground" />
        <Text class={`${FS.base} font-bold text-popover-foreground`}>{t("params.title")}</Text>
      </View>
      <Text class={`${FS.xs} text-muted-foreground`}>{t("params.hint")}</Text>
      <View class="gap-2">
        <Each of={gate()?.names ?? []}>
          {(name, i) => (
            <View class="flex-row items-center gap-3">
              <Text class={`${FS.sm} ${MONO} w-[140px] text-foreground line-clamp-1`}>:{name()}</Text>
              <TextField
                testID={`param:${name()}`}
                class="h-8 min-w-0 flex-1"
                mono
                autoFocus={i() === 0}
                value={gate()?.values()[name()] ?? ""}
                onChange={({ value }) => gate()?.values.update((v) => ({ ...v, [name()]: value }))}
                onSubmit={() => confirmParams(m)}
              />
            </View>
          )}
        </Each>
      </View>
      <View class="max-h-[140px] overflow-hidden rounded-(--radius) border border-border bg-editor px-3 py-2">
        <Text testID="params-preview" class={`${FS.xs} ${MONO} text-foreground line-clamp-6`}>{preview()}</Text>
      </View>
      <View class="flex-row justify-end gap-2 pt-1">
        <Btn variant="outline" label={t("params.cancel")} onPress={close} testID="params-cancel" />
        <Btn variant="primary" icon="play" label={t("params.run")} onPress={() => confirmParams(m)} testID="params-run" />
      </View>
    </Dialog>
  );
}
