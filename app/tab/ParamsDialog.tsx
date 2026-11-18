// 查询参数弹框:SQL 里的每个 `:name` 一个输入框,下方实时预览代入后的 SQL,确认后执行。
// 数字原样代入,NULL 为空值,其余作为字符串(单引号自动转义)。

import { Text, View } from "nsh";
import { t } from "nsh/i18n";
import { sql as sqlText } from "../lib/native";
import { TextField } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { confirmParams, type TabModel } from "./model";
import { ConfirmFrame } from "../dialogs/controls";

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
    <ConfirmFrame
      testID="params-dialog"
      open={gate() !== null}
      onClose={close}
      title={t("params.title")}
      icon="braces"
      cancelLabel={t("params.cancel")}
      cancelTestID="params-cancel"
      confirmLabel={t("params.run")}
      confirmIcon="play"
      confirmTestID="params-run"
      onConfirm={() => confirmParams(m)}
    >
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
    </ConfirmFrame>
  );
}
