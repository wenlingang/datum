// 查询工作区的工具条(编辑器下缘):运行(含行数上限)/ 格式化 / Explain,右侧查询历史。
// 对应 GPUI 版 tab/toolbar.rs。

import { keymap, signal, Text, View, type JSX } from "nsh";
import { Button, Menu, MenuItem, MenuSeparator, Popover, Toolbar as ToolbarBar } from "nsh/base";
import { t } from "nsh/i18n";
import { supportsCancel, supportsSql } from "../lib/types";
import { MAX_ROWS_OPTIONS, maxRows, normalizeMaxRows, updatePrefs } from "../store/prefs";
import { Btn, Icon, SectionLabel, TextField, tip } from "../ui/kit";
import { FS } from "../ui/text";
import { HistoryPanel } from "./History";
import { SnippetPanel } from "./Snippets";
import { beautify, explain, run, runAll, stop, suggestIndexes, type TabModel } from "./model";

// 与 ui/menu 的 MItem 同一套:行高 h-7、FS.sm、hover / focus 用 accent。这里的项要带快捷键提示、单选勾、
// 自定义行数输入框,MItem 装不下,所以仍用 nsh 的 MenuItem,只对齐样式
const POPUP = "rounded-(--radius-lg) border border-border bg-popover shadow-lg";
const ITEM = "group h-7 flex-row items-center gap-2 rounded-(--radius) px-2 hover:bg-accent focus:bg-accent active:bg-secondary-active";

/** 运行按钮旁的下拉菜单:运行全部、索引建议、行数上限(含自定义行数)。 */
function LimitMenu(props: { m: TabModel; trigger: JSX.Element }) {
  const m = props.m;
  const custom = signal("");
  const choose = (n: number) => {
    updatePrefs({ maxRows: n });
    m.limitOpen.set(false);
  };
  return (
    <Menu
      open={m.limitOpen()}
      onOpenChange={(e) => {
        if (e.open) custom.set("");
        m.limitOpen.set(e.open);
      }}
      placement="top"
      align="start"
      offset={4}
      popupClass={`w-[220px] gap-0.5 p-1 ${POPUP}`}
      trigger={props.trigger}
    >
      <MenuItem testID="run-all" accessibilityLabel={t("toolbar.run_all")} onSelect={() => runAll(m)} class={ITEM}>
        <Text class={`${FS.sm} flex-1 text-popover-foreground`}>{t("toolbar.run_all")}</Text>
        <Text class={`${FS.xs} text-fg-faint`}>{keymap.label("mod+shift+enter")}</Text>
      </MenuItem>
      <MenuItem
        testID="suggest-indexes"
        accessibilityLabel={t("advisor.suggest")}
        tooltip={tip(t("advisor.suggest_tooltip"))}
        onSelect={() => suggestIndexes(m)}
        class={ITEM}
      >
        <Text class={`${FS.sm} flex-1 text-popover-foreground`}>{t("advisor.suggest")}</Text>
      </MenuItem>
      <MenuSeparator class="my-1 h-px bg-border" />
      <SectionLabel class="px-2 pb-1">{t("toolbar.limit_title")}</SectionLabel>
      <Each of={MAX_ROWS_OPTIONS}>
        {(n) => (
          <MenuItem
            testID={`limit:${n()}`}
            role="menuitemradio"
            checked={n() === maxRows()}
            accessibilityLabel={t("toolbar.n_rows", { n: n() })}
            onSelect={() => choose(n())}
            class={ITEM}
            indicatorClass="size-3.5 items-center justify-center"
            indicator={<Icon name="check" size={13} color="foreground" class="opacity-0 group-checked:opacity-100" />}
          >
            <Text class={`${FS.sm} flex-1 text-popover-foreground`}>{t("toolbar.n_rows", { n: n() })}</Text>
          </MenuItem>
        )}
      </Each>
      <View class="mt-1 flex-row items-center gap-1.5 border-t border-border pt-1.5">
        <TextField
          testID="limit-custom"
          class="h-6 flex-1"
          placeholder={t("toolbar.limit_custom")}
          value={custom()}
          onChange={({ value }) => custom.set(value)}
          onSubmit={() => {
            const n = normalizeMaxRows(custom());
            if (n !== null) choose(n);
          }}
        />
        <Text class={`${FS.xs} text-muted-foreground`}>{t("toolbar.rows_unit")}</Text>
      </View>
    </Menu>
  );
}

export function Toolbar(props: { m: TabModel }) {
  const m = props.m;
  const relational = () => supportsSql(m.dbKind);
  return (
    <ToolbarBar
      axis="horizontal"
      accessibilityLabel={t("toolbar.label")}
      class="h-[38px] shrink-0 flex-row items-center gap-1.5 border-t border-border bg-panel px-2.5"
    >
      {/* 运行 + 行数下拉合成一个分段按钮:共用主色底和外圆角,中间一条细分隔线(以前是两个各带圆角的按钮,中间有缝) */}
      <View class="h-6 shrink-0 flex-row items-stretch overflow-hidden rounded-(--radius) bg-primary">
        <Button
          testID="run"
          tooltip={tip(t("toolbar.run_tooltip"))}
          onPress={() => run(m)}
          class="flex-row items-center gap-1.5 px-2 hover:bg-primary-hover active:bg-primary-active"
        >
          <Icon name={m.running() ? "loader-circle" : "play"} size={13} color="primary-foreground" />
          <Text class={`${FS.xs} font-medium text-primary-foreground`}>
            {relational() ? t("toolbar.run_with_limit", { n: maxRows() }) : t("toolbar.run")}
          </Text>
        </Button>
        <If when={relational()}>
          <View class="my-1 w-px bg-primary-foreground/25" />
          <LimitMenu
            m={m}
            trigger={
              <Button
                testID="run-limit"
                tooltip={tip(t("toolbar.limit_tooltip"))}
                accessibilityLabel={t("toolbar.limit_tooltip")}
                class="flex-row items-center px-1.5 hover:bg-primary-hover active:bg-primary-active"
              >
                <Icon name="chevron-down" size={13} color="primary-foreground" />
              </Button>
            }
          />
        </If>
      </View>
      <If when={m.running() && supportsCancel(m.dbKind)}>
        <Btn
          small
          variant="outline"
          testID="stop"
          icon="square"
          label={t("toolbar.stop")}
          tooltip={t("toolbar.stop_tooltip")}
          onPress={() => stop(m)}
        />
      </If>
      <If when={relational()}>
        <Btn
          small
          variant="ghost"
          icon="text-align-start"
          label={t("toolbar.format")}
          kbd={keymap.label("mod+shift+f")}
          tooltip={t("toolbar.format_tooltip")}
          onPress={() => beautify(m)}
        />
        <Btn
          small
          variant="ghost"
          testID="explain"
          icon="list-tree"
          label="Explain"
          kbd={keymap.label("mod+e")}
          tooltip={t("toolbar.explain_tooltip")}
          onPress={() => explain(m)}
        />
      </If>
      <View class="flex-1" />
      <Popover
        open={m.snippetsOpen()}
        onOpenChange={(e) => m.snippetsOpen.set(e.open)}
        placement="top"
        align="end"
        offset={4}
        popupClass={POPUP}
        trigger={
          <Button
            testID="snippets"
            tooltip={tip(t("snippets.tooltip"))}
            class="h-6 flex-row items-center gap-1.5 rounded-(--radius) px-2 hover:bg-list-hover active:bg-secondary-active"
          >
            <Icon name="bookmark" size={13} />
            <Text class={`${FS.xs} font-medium text-foreground`}>{t("snippets.button")}</Text>
          </Button>
        }
      >
        <SnippetPanel m={m} />
      </Popover>
      <Popover
        open={m.historyOpen()}
        onOpenChange={(e) => m.historyOpen.set(e.open)}
        placement="top"
        align="end"
        offset={4}
        popupClass={POPUP}
        trigger={
          <Button
            testID="history"
            tooltip={tip(t("toolbar.history_tooltip"))}
            class="h-6 flex-row items-center gap-1.5 rounded-(--radius) px-2 hover:bg-list-hover active:bg-secondary-active"
          >
            <Icon name="rotate-ccw-clock" size={13} />
            <Text class={`${FS.xs} font-medium text-foreground`}>{t("toolbar.history")}</Text>
          </Button>
        }
      >
        <HistoryPanel m={m} />
      </Popover>
    </ToolbarBar>
  );
}
