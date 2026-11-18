// 顶栏:侧栏开关 + 连接路径(连接 / 库 / 表 + 复制)+ AI 助手 + 设置(有更新时红点)。
// 对应 GPUI 版 workspace/chrome.rs 的 render_top_bar。

import { Text, View } from "nsh";
import { Button, Toggle } from "nsh/base";
import { titleBar } from "nsh/desktop";
import { t } from "nsh/i18n";
import { openSettings } from "../dialogs";
import type { Connection } from "../lib/types";
import { privacy, hideInDemo, toggleDemoMode } from "../store/privacy";
import { toggleAi, toggleSidebar, workspace } from "../store/workspace";
import { BrandLine, BrandWash, Dot, Icon, IconButton, tip, VSep } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { copyText } from "./ops";

/** 面包屑里的库名:SQLite 显示文件名,Redis 显示 db 编号。 */
export function headDbLabel(c: Connection): string {
  if (c.kind === "sqlite") {
    const p = c.filePath ?? "";
    const name = p.split(/[\\/]/).pop();
    // 演示模式:不露出文件名(常带着用户名、项目名)
    return name ? hideInDemo(name) : "—";
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
                    small
                    icon="copy"
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
          {/* 演示模式:醒目的 DEMO 徽章,点它即退出 */}
          <If
            when={privacy.demo()}
            else={
              <IconButton
                icon="monitor"
                testID="demo-toggle"
                tooltip={t("mask.demo_enter")}
                onPress={toggleDemoMode}
              />
            }
          >
            <Button
              testID="demo-badge"
              onPress={toggleDemoMode}
              tooltip={tip(t("mask.demo_exit"))}
              accessibilityLabel={t("mask.demo_exit")}
              class="h-6 flex-row items-center gap-1.5 rounded-full bg-warning px-2.5 hover:opacity-90"
            >
              <Icon name="eye-off" size={12} color="warning-foreground" />
              <Text class={`${FS.xs} font-bold text-warning-foreground`}>{t("mask.demo_badge")}</Text>
            </Button>
          </If>
          {/* 开关语义用 Toggle:按下样式 aria-pressed: 优先级高于 hover:,悬停不盖掉打开时的底色 */}
          <Toggle
            pressed={workspace.aiOpen()}
            onChange={() => toggleAi()}
            testID="toggle-ai"
            accessibilityLabel={t("workspace.ai_assistant")}
            tooltip={tip(() => (workspace.aiOpen() ? t("workspace.hide_ai") : t("workspace.ai_assistant")))}
            class="h-7 flex-row items-center gap-1.5 rounded-(--radius) px-2 hover:bg-list-hover active:bg-secondary-active aria-pressed:bg-secondary"
          >
            <Icon name="sparkles" size={15} color={workspace.aiOpen() ? "ai" : "foreground"} />
            <Text class={`${FS.sm} font-medium text-foreground`}>{t("workspace.ai_assistant")}</Text>
          </Toggle>
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
