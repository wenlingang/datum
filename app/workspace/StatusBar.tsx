// 底部状态栏:连接状态 + 当前 tab 的统计 / 分页(TabStatus)+ 版本号;品牌洗色与上缘渐变发丝线。
// 对应 GPUI 版 workspace/chrome.rs 的 render_status_bar。

import { Text, View } from "nsh";
import { t } from "nsh/i18n";
import { prefsStore } from "../lib/native";
import { KIND_LABEL } from "../lib/types";
import { workspace } from "../store/workspace";
import { TabStatus } from "../tab";
import { BrandLine, BrandWash, Dot } from "../ui/kit";
import { FS, MONO } from "../ui/text";

const VERSION = prefsStore.appVersion();

export function StatusBar() {
  return (
    <View class="relative h-7 shrink-0 border-t border-border bg-status-bar">
      <BrandWash mirror />
      <View class="flex-1 flex-row items-center justify-between pl-3.5 pr-2.5">
        <If when={workspace.headConn()}>
          {(c) => (
            <View class="shrink-0 flex-row items-center gap-1.5 pr-2.5" testID="status-conn">
              <Dot color="success" />
              <Text class={`${FS.xs} text-muted-foreground`}>{t("workspace.connected")}</Text>
              <Text class={`${FS.xs} ${MONO} text-fg-faint`}>{KIND_LABEL[c().kind]}</Text>
            </View>
          )}
        </If>
        {/* 中段归当前 tab:左侧行列数 / 耗时,右侧分页(TabStatus 自己左右排布) */}
        <View class="min-w-0 flex-1 flex-row items-center overflow-hidden">
          <TabStatus />
        </View>
        <Text class={`${FS.xs} ${MONO} shrink-0 pl-2.5 text-fg-faint`}>v{VERSION}</Text>
      </View>
      <BrandLine mirror top />
    </View>
  );
}
