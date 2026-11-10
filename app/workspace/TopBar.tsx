// 顶栏:侧栏开关 + 连接路径(连接 / 库 / 表 + 复制)+ AI 助手 + 设置(有更新时红点)。
// 对应 GPUI 版 workspace/chrome.rs 的 render_top_bar。

import { Text, View } from "nsh";
import { Button } from "nsh/base";
import { titleBar } from "nsh/desktop";
import { t } from "nsh/i18n";
import { openSettings } from "../dialogs";
import type { Connection } from "../lib/types";
import { toggleAi, toggleSidebar, workspace } from "../store/workspace";
import { BrandLine, BrandWash, Dot, Icon, IconButton, tip, VSep } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { copyText } from "./ops";

/** 面包屑里的库名:SQLite 显示文件名,Redis 显示 db 编号。 */
export function headDbLabel(c: Connection): string {
  if (c.kind === "sqlite") {
    const p = c.filePath ?? "";
    const name = p.split(/[\\/]/).pop();
    return name || "—";
  }
  if (c.kind === "redis") return `db${c.database}`;
  return c.database;
}

function PathPill() {
  const head = () => workspace.headConn();
  const table = () => workspace.activeTab()?.table ?? null;
  return (
    <View
      testID="conn-path"
      class="h-7 min-w-0 shrink flex-row items-center gap-2 overflow-hidden rounded-full border border-border bg-raised px-3"
    >
      <Dot color={head() ? "success" : "fg-faint"} />
      <If
        when={head()}
        else={<Text class={`line-clamp-1 ${FS.sm} text-muted-foreground`}>{t("workspace.not_connected")}</Text>}
      >
        {(c) => (
          <>
            <Text class={`line-clamp-1 ${FS.sm} shrink-0 font-bold text-foreground`}>
              {c().name}
            </Text>
            <Text class={`${FS.sm} text-fg-faint`}>/</Text>
            <Text class={`line-clamp-1 ${FS.xs} ${MONO} shrink text-muted-foreground`}>
              {headDbLabel(c())}
            </Text>
            <If when={table()}>
              {(name) => (
                <>
                  <Text class={`${FS.sm} text-fg-faint`}>/</Text>
                  <Text class={`line-clamp-1 ${FS.xs} ${MONO} shrink font-bold text-foreground`}>
                    {name()}
                  </Text>
                  <IconButton
                    icon="copy"
                    size={12}
                    class="p-1"
                    tooltip={t("workspace.copy_table_name")}
                    onPress={() => void copyText(name())}
                  />
                </>
              )}
            </If>
          </>
        )}
      </If>
    </View>
  );
}

export function TopBar() {
  // 隐藏标题栏时给 macOS 红绿灯让位(其他平台为 0)
  const left = () => Math.max(titleBar().left, 0);
  return (
    <View class="relative h-[46px] shrink-0 bg-title-bar" desktop={{ windowDrag: true }}>
      <BrandWash />
      <View class="flex-1 flex-row items-center justify-between gap-3" style={{ paddingLeft: left() + 8 }}>
        <View class="min-w-0 shrink flex-row items-center gap-3">
          <IconButton
            icon="panel-left"
            tooltip={workspace.sidebarOpen() ? t("workspace.hide_sidebar") : t("workspace.show_sidebar")}
            onPress={toggleSidebar}
          />
          <VSep />
          <PathPill />
        </View>
        <View class="shrink-0 flex-row items-center gap-2 pr-3">
          <Button
            onPress={toggleAi}
            selected={workspace.aiOpen()}
            testID="toggle-ai"
            tooltip={tip(() => (workspace.aiOpen() ? t("workspace.hide_ai") : t("workspace.ai_assistant")))}
            class="h-7 flex-row items-center gap-1.5 rounded-(--radius) px-2 hover:bg-list-hover active:bg-secondary-active selected:bg-secondary"
          >
            <Icon name="sparkles" size={15} color={workspace.aiOpen() ? "ai" : "foreground"} />
            <Text class={`${FS.sm} font-medium text-foreground`}>{t("workspace.ai_assistant")}</Text>
          </Button>
          <View class="relative">
            <IconButton
              icon="settings"
              tooltip={workspace.updateAvailable() ? t("workspace.settings_update") : t("workspace.settings")}
              onPress={() => openSettings()}
            />
            <If when={workspace.updateAvailable()}>
              <View class="pointer-events-none absolute right-1 top-1 size-1.5 rounded-full bg-negative" />
            </If>
          </View>
        </View>
      </View>
      <BrandLine />
    </View>
  );
}
