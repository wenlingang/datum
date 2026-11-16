// 连接的「数据与隐私」:敏感数据遮罩的开关、自动识别,以及识别出的敏感列与手动覆盖
// (强制遮罩 / 永不遮罩)。规则与落盘都在原生侧,这里改完即生效(网格随版本号重算)。

import { Scroll, Text, View } from "nsh";
import { t } from "nsh/i18n";
import { errorText, mask } from "../lib/native";
import type { DetectedColumn, MaskKind, MaskOverride } from "../lib/types";
import { conns } from "../store/conns";
import { maskSettings, privacy, setMaskAutoDetect, setMaskEnabled, setMaskOverride } from "../store/privacy";
import { Btn, EnvBadge, Hint } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { DialogFrame, Segmented, Toggle } from "./controls";

type Choice = "auto" | MaskOverride;

const KIND_KEY: Record<MaskKind, string> = {
  secret: "mask.kind_secret",
  email: "mask.kind_email",
  phone: "mask.kind_phone",
  idNumber: "mask.kind_id_number",
  card: "mask.kind_card",
  address: "mask.kind_address",
  name: "mask.kind_name",
};

export const maskKindLabel = (k: MaskKind) => t(KIND_KEY[k]);

function detected(connId: string): DetectedColumn[] {
  privacy.rev();
  try {
    return mask.detectColumns(connId, conns.columns(connId) ?? null);
  } catch (e) {
    return [{ table: "", column: errorText(e), kind: null, override: null }];
  }
}

function ColumnRow(props: { connId: string; col: DetectedColumn }) {
  const key = () => (props.col.table ? `${props.col.table}.${props.col.column}` : props.col.column);
  const choice = (): Choice => props.col.override ?? "auto";
  return (
    <View testID={`privacy-col:${key()}`} class="flex-row items-center gap-3 border-b border-border py-2">
      <View class="min-w-0 flex-1 gap-0.5">
        <Text class={`${FS.sm} ${MONO} text-foreground line-clamp-1`}>{key()}</Text>
        <Text class={`${FS.xs} text-muted-foreground`}>
          {props.col.kind ? maskKindLabel(props.col.kind) : t("mask.kind_manual")}
        </Text>
      </View>
      <View class="w-[260px] shrink-0">
        <Segmented<Choice>
          testID={`privacy-choice:${key()}`}
          value={choice()}
          onChange={(v) => setMaskOverride(props.connId, key(), v === "auto" ? null : v)}
          options={[
            { value: "auto", label: t("mask.choice_auto") },
            { value: "mask", label: t("mask.choice_mask") },
            { value: "plain", label: t("mask.choice_plain") },
          ]}
        />
      </View>
    </View>
  );
}

export function PrivacyDialog(props: { open: boolean; connId: string; onClose: () => void }) {
  const conn = () => conns.get(props.connId);
  const s = () => maskSettings(props.connId);
  const cols = () => detected(props.connId);
  return (
    <DialogFrame
      open={props.open}
      onClose={props.onClose}
      title={t("mask.dialog_title", { name: conn()?.name ?? "" })}
      width={640}
      testID="privacy-dialog"
    >
      <View class="gap-3">
        <View class="flex-row items-center gap-3">
          <View class="flex-1">
            <Toggle
              testID="privacy-enabled"
              checked={s().enabled}
              onChange={(v) => setMaskEnabled(props.connId, v)}
              label={t("mask.enabled")}
            />
          </View>
          <If when={conn()}>{(c) => <EnvBadge env={c().env} />}</If>
          <If when={s().explicit !== null && !s().demo}>
            <Btn small variant="ghost" label={t("mask.follow_env")} onPress={() => setMaskEnabled(props.connId, null)} />
          </If>
        </View>
        <Hint>{s().demo ? t("mask.forced_by_demo") : t("mask.enabled_hint")}</Hint>
        <Toggle
          testID="privacy-auto"
          checked={s().autoDetect}
          onChange={(v) => setMaskAutoDetect(props.connId, v)}
          label={t("mask.auto_detect")}
        />
        <Text class={`${FS.xs} pt-2 font-bold text-muted-foreground`}>{t("mask.columns_title")}</Text>
        <If
          when={cols().length > 0}
          else={
            <Hint>{conns.columns(props.connId) ? t("mask.no_columns") : t("mask.columns_not_loaded")}</Hint>
          }
        >
          <Scroll class="max-h-[340px]" testID="privacy-columns">
            <Each of={cols()} key={(c) => `${c.table}.${c.column}:${c.override ?? ""}`}>
              {(c) => <ColumnRow connId={props.connId} col={c()} />}
            </Each>
          </Scroll>
        </If>
        <Hint>{t("mask.columns_hint")}</Hint>
      </View>
    </DialogFrame>
  );
}
