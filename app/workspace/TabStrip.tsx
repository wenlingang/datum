// 标签条:新建查询 + 当前连接的 tab 列表(双击重命名、右键菜单)+ 结果的过滤 / 导出。
// 对应 GPUI 版 workspace/chrome.rs 的 render_tab_strip。

import { type InputRef, Scroll, Text, View } from "nsh";
import { t } from "nsh/i18n";
import {
  closeOtherTabs,
  closeTab,
  finishRename,
  focusTab,
  startRename,
  type WorkTab,
  workspace,
} from "../store/workspace";
import { runTabCommand } from "../tab";
import { Icon, IconButton, TextField } from "../ui/kit";
import { CtxMenu, DropMenu, MItem, MSep } from "../ui/menu";
import { FS } from "../ui/text";
import { newQuery } from "./commands";
import { EXPORT_FORMATS, exportFiltered, exportMenuLabel } from "./ops";

/** 重命名输入框:出现即聚焦并全选;回车或失焦提交。 */
function RenameField(props: { tab: WorkTab }) {
  let input: InputRef | undefined;
  const id = props.tab.id;
  const initial = props.tab.title();
  let value = initial;
  let done = false;
  const commit = () => {
    if (done) return;
    done = true;
    finishRename(id, value);
  };
  setTimeout(() => {
    input?.focus();
    input?.setSelection(0, initial.length);
  }, 0);
  return (
    <View class="w-[140px]">
      <TextField
        ref={input}
        defaultValue={initial}
        testID="tab-rename"
        onChange={({ value: v }) => (value = v)}
        onSubmit={commit}
        onBlur={commit}
        class="h-6 px-1.5"
      />
    </View>
  );
}

function TabItem(props: { tab: () => WorkTab }) {
  const tab = () => props.tab();
  const active = () => workspace.activeTab()?.id === tab().id;
  const renaming = () => workspace.renamingId() === tab().id;
  return (
    <View
      class="relative h-full shrink-0 flex-row items-center border-r border-border pl-3 pr-1.5"
      classList={{ "bg-tab-active": active() }}
    >
      <If when={active()}>
        <View class="absolute left-0 right-0 top-0 h-0.5 bg-primary" />
      </If>
      <If when={tab().kind === "table"}>
        <View class="mr-1.5">
          <Icon name="table-2" size={14} color={active() ? "data" : "fg-faint"} />
        </View>
      </If>
      {/* 菜单常驻,重命名只换标题区:避免选中「重命名」时卸载正在收起的菜单 */}
      <CtxMenu
        disabled={renaming()}
        trigger={
          // 单击激活、双击重命名:需要点击次数,所以用带 onPress 事件的 View(role 为 tab)
          <View
            onPress={(e) => {
              if (renaming()) return;
              if (e.clickCount >= 2) startRename(tab().id);
              else focusTab(tab());
            }}
            role="tab"
            accessibilityLabel={tab().title()}
            accessibilityState={{ selected: active() }}
            testID={`tab-${tab().id}`}
            class="h-full justify-center"
          >
            <If
              when={renaming()}
              else={
                <Text class={`line-clamp-1 ${FS.sm} max-w-40 text-foreground`} classList={{ "font-semibold": active() }}>
                  {tab().title()}
                </Text>
              }
            >
              <RenameField tab={tab()} />
            </If>
          </View>
        }
      >
        <MItem label={t("workspace.close_tab")} onSelect={() => closeTab(tab().id)} />
        <MItem label={t("workspace.close_other_tabs")} onSelect={() => closeOtherTabs(tab().id)} />
        <MSep />
        <MItem label={t("workspace.rename")} onSelect={() => startRename(tab().id)} />
      </CtxMenu>
      <IconButton
        icon="x"
        size={12}
        class="ml-1 p-1"
        tooltip={t("workspace.close")}
        label={`${t("workspace.close")} ${tab().title()}`}
        onPress={() => closeTab(tab().id)}
      />
    </View>
  );
}

/** 结果的过滤 / 导出(当前 tab 有结果时显示)。 */
function ResultTools() {
  return (
    <If when={workspace.resultTools()}>
      {(tools) => (
        <View class="h-full shrink-0 flex-row items-center gap-2 border-l border-border px-3">
          <If when={tools().filterCount > 0}>
            <View class="h-[22px] flex-row items-center gap-1.5 rounded-full bg-selection px-2.5">
              <Icon name="funnel" size={12} color="data" />
              <Text class={`${FS.xs} font-bold text-data`}>{t("workspace.filtered_n", { count: tools().filterCount })}</Text>
            </View>
          </If>
          <IconButton
            icon="funnel"
            active={tools().filterOpen || tools().filterCount > 0}
            tooltip={t("workspace.filter_tooltip")}
            onPress={() => runTabCommand("filter")}
          />
          <DropMenu
            align="end"
            testID="export-menu"
            trigger={
              <IconButton icon="download" tooltip={t("workspace.export")} disabled={!tools().exportData()} />
            }
          >
            {EXPORT_FORMATS.map((f) => (
              <MItem label={exportMenuLabel(f)} onSelect={() => void exportFiltered(f)} />
            ))}
          </DropMenu>
        </View>
      )}
    </If>
  );
}

export function TabStrip() {
  return (
    <View class="h-10 shrink-0 flex-row items-stretch border-b border-border bg-tab-bar">
      <View class="shrink-0 flex-row items-center px-2">
        <IconButton icon="terminal" color="data" tooltip={t("workspace.new_query_tooltip")} onPress={newQuery} />
      </View>
      <Scroll horizontal class="min-w-0 flex-1" contentClass="h-full flex-row" testID="tab-strip">
        <Each of={workspace.visibleTabs()} key={(tab) => tab.id}>
          {(tab) => <TabItem tab={tab} />}
        </Each>
      </Scroll>
      <ResultTools />
    </View>
  );
}
