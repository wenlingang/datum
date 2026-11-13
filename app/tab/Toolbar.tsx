// 查询工作区的工具条(编辑器下缘):运行(含行数上限)/ 格式化 / Explain,右侧查询历史。
// 对应 GPUI 版 tab/toolbar.rs。

import { signal, Text, View } from "nsh";
import { Button, Popover } from "nsh/base";
import { t } from "nsh/i18n";
import { supportsSql } from "../lib/types";
import { MAX_ROWS_OPTIONS, maxRows, normalizeMaxRows, updatePrefs } from "../store/prefs";
import { Btn, Icon, TextField, tip } from "../ui/kit";
import { FS } from "../ui/text";
import { HistoryPanel } from "./History";
import { SnippetPanel } from "./Snippets";
import { beautify, explain, run, runAll, type TabModel } from "./model";

const POPUP = "rounded-(--radius-lg) border border-border bg-popover shadow-lg";

function LimitMenu(props: { m: TabModel }) {
  const m = props.m;
  const custom = signal("");
  const choose = (n: number) => {
    updatePrefs({ maxRows: n });
    m.limitOpen.set(false);
  };
  return (
    <View class="w-[180px] gap-0.5 p-1.5">
      <Button
        testID="run-all"
        onPress={() => {
          m.limitOpen.set(false);
          runAll(m);
        }}
        class="h-[26px] flex-row items-center gap-2 rounded-(--radius) px-2.5 hover:bg-list-hover"
      >
        <Text class={`${FS.xs} flex-1 text-foreground`}>{t("toolbar.run_all")}</Text>
        <Text class={`${FS.xs} text-fg-faint`}>⌘⇧↵</Text>
      </Button>
      <View class="my-1 border-t border-border" />
      <Text class={`${FS.xs} px-2 pb-1 font-bold text-fg-faint`}>{t("toolbar.limit_title")}</Text>
      <Each of={MAX_ROWS_OPTIONS}>
        {(n) => (
          <Button
            testID={`limit:${n()}`}
            onPress={() => choose(n())}
            selected={n() === maxRows()}
            class="h-[26px] flex-row items-center rounded-(--radius) px-2.5 hover:bg-list-hover selected:bg-raised"
          >
            <Text
              class={`${FS.xs} flex-1`}
              classList={{ "font-bold text-foreground": n() === maxRows(), "text-muted-foreground": n() !== maxRows() }}
            >
              {t("toolbar.n_rows", { n: n() })}
            </Text>
          </Button>
        )}
      </Each>
      <View class="mt-1 flex-row items-center gap-1.5 border-t border-border pt-1.5">
        <TextField
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
    </View>
  );
}

export function Toolbar(props: { m: TabModel }) {
  const m = props.m;
  const relational = () => supportsSql(m.dbKind);
  return (
    <View class="h-[38px] shrink-0 flex-row items-center gap-1.5 border-t border-border bg-panel px-2.5">
      <View class="flex-row items-center gap-px">
        <Btn
          small
          variant="primary"
          testID="run"
          icon={m.running() ? "loader-circle" : "play"}
          label={relational() ? t("toolbar.run_with_limit", { n: maxRows() }) : t("toolbar.run")}
          tooltip={t("toolbar.run_tooltip")}
          onPress={() => run(m)}
        />
        <If when={relational()}>
          <Popover
            open={m.limitOpen()}
            onOpenChange={(e) => m.limitOpen.set(e.open)}
            placement="top"
            align="start"
            offset={4}
            popupClass={POPUP}
            trigger={
              <Button
                testID="run-limit"
                tooltip={tip(t("toolbar.limit_tooltip"))}
                class="h-6 flex-row items-center rounded-(--radius) bg-primary px-1.5 hover:bg-primary-hover active:bg-primary-active"
              >
                <Icon name="chevron-down" size={13} color="primary-foreground" />
              </Button>
            }
          >
            <LimitMenu m={m} />
          </Popover>
        </If>
      </View>
      <If when={relational()}>
        <Btn
          small
          variant="ghost"
          icon="wand-sparkles"
          label={t("toolbar.format")}
          kbd="⌘⇧F"
          tooltip={t("toolbar.format_tooltip")}
          onPress={() => beautify(m)}
        />
        <Btn
          small
          variant="ghost"
          icon="list-tree"
          label="Explain"
          kbd="⌘E"
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
    </View>
  );
}
