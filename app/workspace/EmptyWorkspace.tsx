// 无标签页时的空白工作区。对应 GPUI 版 workspace/chrome.rs 的 empty_workspace。

import { Image, Text, View } from "nsh";
import { t } from "nsh/i18n";
import appIcon from "../assets/app-icon.png";
import { Btn } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { newQuery } from "./commands";

export function EmptyWorkspace() {
  return (
    <View class="flex-1 items-center justify-center gap-1 bg-editor p-6" testID="empty-workspace">
      <Image src={appIcon} style={{ width: 112, height: 112, opacity: 0.92 }} />
      <Text class={`${FS.sm} ${MONO} mt-6 font-bold text-fg-soft`}>{t("workspace.ready")}</Text>
      <Text class={`${FS.sm} mt-1.5 text-center text-fg-faint`}>{t("workspace.empty_hint")}</Text>
      <View class="mt-4">
        <Btn variant="primary" small icon="code-xml" label={t("workspace.new_sql_query")} onPress={newQuery} testID="empty-new-query" />
      </View>
    </View>
  );
}
