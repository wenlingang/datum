// 标签条:新建查询 + 当前连接的 tab 列表(双击重命名、右键菜单)+ 结果的过滤 / 导出。
// 对应 GPUI 版 workspace/chrome.rs 的 render_tab_strip。

import { effect, type InputRef, Scroll, Text, View, type ViewRef } from "nsh";
import { t } from "nsh/i18n";
import {
  canRename,
  closeOtherTabs,
  closeTab,
  closeTabsBeside,
  finishRename,
  focusTab,
  startRename,
  tabsBeside,
  type WorkTab,
  workspace,
} from "../store/workspace";
import { runTabCommand } from "../tab";
import type { IconName } from "../ui/icons";
import { Icon, IconButton, TextField } from "../ui/kit";
import { CtxMenu, DropMenu, MCheck, MItem, MSep } from "../ui/menu";
import { privacy } from "../store/privacy";
import { FS } from "../ui/text";
import { newQuery } from "./commands";
import { EXPORT_FORMATS, exportFiltered, exportMasked, exportMenuLabel, toggleExportMask } from "./ops";

/** 正在进行的重命名:点到输入框外面时由工作区根节点提交(见 renameOnPressAnywhere)。 */
let pendingRename: { commit: () => void; inside: boolean } | null = null;

/** 工作区根节点的按下(原始触摸,由内向外冒泡):不是点在重命名输入框里就提交当前名字。
 *  点到不能获得焦点的地方(空白、表格、标签条)时输入框不会失焦,onBlur 等不到,所以在这里收尾。 */
export function renameOnPressAnywhere() {
  const p = pendingRename;
  if (!p) return;
  if (p.inside) p.inside = false;
  else p.commit();
}

/** 重命名输入框:出现即聚焦并全选;回车、Esc、失焦或点到别处都按当前输入保存(空名字不改)。 */
function RenameField(props: { tab: WorkTab }) {
  let input: InputRef | undefined;
  const id = props.tab.id;
  const initial = props.tab.title();
  let value = initial;
  let done = false;
  const commit = () => {
    if (done) return;
    done = true;
    if (pendingRename === self) pendingRename = null;
    finishRename(id, value);
  };
  const self = { commit, inside: false };
  pendingRename = self;
  setTimeout(() => {
    input?.focus();
    input?.setSelection(0, initial.length);
  }, 0);
  return (
    <View
      class="w-[140px]"
      // 输入框里的按下先到这里(比根节点早),标记为「在里面」,根节点就不提交
      onTouchStart={() => (self.inside = true)}
      onKeyDown={(e) => {
        if (e.key === "Escape") commit();
      }}
    >
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

/** 每种 tab 的图标(查询页也有,和其他 tab 对齐)。 */
const TAB_ICON: Record<WorkTab["kind"], IconName> = {
  query: "terminal",
  table: "table-2",
  erd: "network",
  redis: "activity",
  rlive: "radio",
  rlua: "code-xml",
  diag: "stethoscope",
  diff: "chevrons-up-down",
  ddiff: "arrow-left-right",
  index: "scan-search",
  journal: "scroll-text",
};

function TabItem(props: { tab: () => WorkTab }) {
  const tab = () => props.tab();
  const active = () => workspace.activeTab()?.id === tab().id;
  const renaming = () => workspace.renamingId() === tab().id;
  // 新开 / 激活的 tab 滚进标签栏的可见范围(连同关闭按钮),标签多时不会被截在滚动区右端
  let box: ViewRef | undefined;
  effect(() => {
    if (active()) box?.reveal({ horizontal: "nearest" });
  });
  return (
    <View
      ref={box}
      class="relative h-full shrink-0 flex-row items-center border-r border-border pr-1.5"
      classList={{ "bg-tab-active": active() }}
    >
      {/* 选中的 tab:底部渐变下划线,与顶栏下缘的品牌渐变线同色系(主色 → AI 紫);
          放在顶部会和顶栏那条线贴着平行,粗细颜色都不一样,显得乱 */}
      <If when={active()}>
        <View class="pointer-events-none absolute bottom-0 left-0 right-0 h-0.5 bg-linear-to-r from-primary to-ai" />
      </If>
      {/* 菜单常驻,重命名只换标题区:避免选中「重命名」时卸载正在收起的菜单 */}
      <CtxMenu
        disabled={renaming()}
        trigger={
          // 整个 tab(左边距、图标、标题)都可点,关闭按钮除外。
          // 按下即激活(同浏览器标签):另一个 tab 正在改名时,这次按下会让它提交、标题宽度变化、
          // 后面的 tab 跟着挪位,等抬起时已不在指针下,点击落空,得再点一次。双击重命名(仅查询页)
          // 要点击次数,仍在 onPress 里判断(role 为 tab)
          <View
            onTouchStart={() => {
              if (!renaming()) focusTab(tab());
            }}
            onPress={(e) => {
              if (renaming()) return;
              if (e.clickCount >= 2 && canRename(tab())) startRename(tab().id);
              else focusTab(tab());
            }}
            role="tab"
            accessibilityLabel={tab().title()}
            accessibilityState={{ selected: active() }}
            testID={`tab-${tab().id}`}
            class="h-full flex-row items-center gap-1.5 pl-3"
          >
            <View testID={`tab-icon:${tab().title()}`}>
              <Icon name={TAB_ICON[tab().kind]} size={14} color={active() ? "data" : "fg-faint"} />
            </View>
            <If
              when={renaming()}
              else={
                // 字重不随选中变化(粗细一变宽度就变,tab 会挪位);选中靠颜色、底色和底部渐变下划线区分
                <Text
                  class={`line-clamp-1 ${FS.sm} max-w-40 font-medium`}
                  classList={{ "text-foreground": active(), "text-muted-foreground": !active() }}
                >
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
        <MItem
          label={t("workspace.close_tabs_left")}
          disabled={tabsBeside(tab().id, "left").length === 0}
          onSelect={() => closeTabsBeside(tab().id, "left")}
        />
        <MItem
          label={t("workspace.close_tabs_right")}
          disabled={tabsBeside(tab().id, "right").length === 0}
          onSelect={() => closeTabsBeside(tab().id, "right")}
        />
        <If when={canRename(tab())}>
          <MSep />
          <MItem label={t("workspace.rename")} onSelect={() => startRename(tab().id)} />
        </If>
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
            {/* 结果里有敏感列时:导出遮罩后的值(默认勾选,演示模式下强制) */}
            <If when={tools().exportData()?.maskedRows}>
              <MSep />
              <MCheck
                label={t("mask.export_masked")}
                checked={exportMasked()}
                disabled={privacy.demo()}
                onSelect={toggleExportMask}
              />
            </If>
          </DropMenu>
        </View>
      )}
    </If>
  );
}

export function TabStrip() {
  return (
    <View class="h-10 shrink-0 flex-row items-stretch border-b border-border bg-tab-bar">
      {/* 滚动区按内容宽度、放不下时收窄并在里面滚动;「+」在它外面紧跟其后:
          tab 少时挨着最后一个 tab,多到要滚动时固定在右侧,不跟着滚走 */}
      <Scroll horizontal class="min-w-0 shrink" contentClass="h-full flex-row" testID="tab-strip">
        <Each of={workspace.visibleTabs()} key={(tab) => tab.id}>
          {(tab) => <TabItem tab={tab} />}
        </Each>
      </Scroll>
      <View class="shrink-0 flex-row items-center px-1.5">
        <IconButton icon="plus" tooltip={t("workspace.new_query_tooltip")} onPress={newQuery} testID="new-query-tab" />
      </View>
      <View class="flex-1" />
      <ResultTools />
    </View>
  );
}
