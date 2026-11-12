// AI 自己查数据时的一步:列出表 / 查看表结构 / 只读查询。头部一行(图标、标题、详情、状态),
// 点开看完整 SQL 与结果。布局参照 nsh gallery 的对话示例(ToolCard)。

import { Scroll, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";
import type { ToolStep } from "../store/ai";
import { Btn, Icon } from "../ui/kit";
import type { IconName } from "../ui/icons";
import { FS, MONO } from "../ui/text";

/** 展开后结果表最多显示的行与列(对话工作台里更多)。 */
const SHOW_ROWS = 20;
const SHOW_COLS = 8;
const WIDE_ROWS = 50;
const WIDE_COLS = 14;

function head(step: ToolStep): { icon: IconName; title: string; detail: string } {
  const d = step.detail;
  switch (step.name) {
    case "list_tables":
      return { icon: "list-tree", title: t("chat.tool_list_tables"), detail: "" };
    case "describe_table":
      return { icon: "table-2", title: t("chat.tool_describe_table"), detail: String(step.args.table ?? d?.table ?? "") };
    case "run_query":
      return {
        icon: "play",
        title: t("chat.tool_run_query"),
        detail: String(step.args.sql ?? d?.sql ?? "").replace(/\s+/g, " "),
      };
    default:
      return { icon: "terminal", title: step.name, detail: "" };
  }
}

/** 结果表:列名一行 + 若干数据行,横向可滚。 */
export function ResultTable(props: { columns: string[]; rows: (string | null)[][]; wide?: boolean }) {
  const cols = () => props.columns.slice(0, props.wide ? WIDE_COLS : SHOW_COLS);
  return (
    <Scroll horizontal class={props.wide ? "max-h-[420px]" : "max-h-[220px]"}>
      <View class="gap-0.5">
        <View class="flex-row gap-3">
          <Each of={cols()}>
            {(c) => <Text class={`${FS.xs} ${MONO} w-[96px] font-bold text-muted-foreground line-clamp-1`}>{c()}</Text>}
          </Each>
        </View>
        <Each of={props.rows.slice(0, props.wide ? WIDE_ROWS : SHOW_ROWS)}>
          {(r) => (
            <View class="flex-row gap-3">
              <Each of={cols()}>
                {(_, ci) => (
                  <Text class={`${FS.xs} ${MONO} w-[96px] text-foreground line-clamp-1`}>{r()[ci()] ?? "NULL"}</Text>
                )}
              </Each>
            </View>
          )}
        </Each>
      </View>
    </Scroll>
  );
}

/** 超出显示范围时的说明。 */
export function ShownNote(props: { columns: number; rows: number; wide?: boolean }) {
  const maxRows = () => (props.wide ? WIDE_ROWS : SHOW_ROWS);
  const maxCols = () => (props.wide ? WIDE_COLS : SHOW_COLS);
  return (
    <If when={props.rows > maxRows() || props.columns > maxCols()}>
      <Text class={`${FS.xs} text-fg-faint`}>
        {t("chat.tool_shown", { rows: Math.min(maxRows(), props.rows), cols: Math.min(maxCols(), props.columns) })}
      </Text>
    </If>
  );
}

function Body(props: { step: ToolStep; wide?: boolean; onOpenQuery?: (sql: string) => void }) {
  const d = () => props.step.detail;
  const columnNames = () => ((d()?.columns ?? []) as unknown[]).map((c) => (typeof c === "string" ? c : String((c as { name?: string }).name ?? "")));
  return (
    <View class="gap-2 border-t border-border px-3 py-2">
      <If when={props.step.name === "run_query" && (d()?.sql ?? props.step.args.sql)}>
        <Text class={`${FS.xs} ${MONO} select-text text-foreground`}>{String(d()?.sql ?? props.step.args.sql ?? "")}</Text>
      </If>
      <If when={d()?.error}>{(e) => <Text class={`${FS.xs} text-negative`}>{e()}</Text>}</If>
      <If when={props.step.name === "run_query" && (d()?.rows?.length ?? 0) > 0}>
        <ResultTable columns={columnNames()} rows={d()?.rows ?? []} wide={props.wide} />
        <ShownNote columns={columnNames().length} rows={d()?.rows?.length ?? 0} wide={props.wide} />
      </If>
      <If when={props.step.name === "run_query" && props.onOpenQuery && (d()?.sql ?? props.step.args.sql)}>
        <View class="flex-row">
          <Btn
            small
            variant="ghost"
            icon="square-play"
            label={t("chat.open_in_editor")}
            onPress={() => props.onOpenQuery?.(String(d()?.sql ?? props.step.args.sql ?? ""))}
          />
        </View>
      </If>
      <If when={props.step.name === "describe_table" && columnNames().length > 0}>
        <View class="gap-0.5">
          <Each of={(d()?.columns ?? []) as { name: string; dataType: string; isPk: boolean }[]}>
            {(c) => (
              <View class="flex-row gap-3">
                <Text class={`${FS.xs} ${MONO} min-w-0 flex-1 text-foreground line-clamp-1`}>
                  {c().name}
                  {c().isPk ? "  PK" : ""}
                </Text>
                <Text class={`${FS.xs} ${MONO} text-muted-foreground`}>{c().dataType}</Text>
              </View>
            )}
          </Each>
        </View>
      </If>
      <If when={props.step.name === "list_tables" && (d()?.tables?.length ?? 0) > 0}>
        <Text class={`${FS.xs} ${MONO} select-text text-muted-foreground`}>{(d()?.tables ?? []).join("  ")}</Text>
      </If>
    </View>
  );
}

export function ToolCard(props: {
  step: ToolStep;
  testID?: string;
  /** 对话工作台:查询结果默认展开、表格更大 */
  wide?: boolean;
  /** 「在编辑器中打开」 */
  onOpenQuery?: (sql: string) => void;
}) {
  // 用户没点过时:工作台里成功的查询默认展开
  const toggled = signal<boolean | null>(null);
  const open = () => toggled() ?? (!!props.wide && props.step.name === "run_query" && props.step.state === "ok");
  const h = () => head(props.step);
  return (
    <View
      testID={props.testID}
      class="overflow-hidden rounded-(--radius) border bg-background"
      classList={{
        "border-ai/40": props.step.state === "running",
        "border-destructive/40": props.step.state === "error",
        "border-border": props.step.state === "ok",
      }}
    >
      <View
        class="flex-row items-center gap-2.5 px-3 py-2 hover:bg-list-hover"
        onPress={() => toggled.set(!open())}
        accessibilityLabel={h().title}
      >
        <View class="h-6 w-6 items-center justify-center rounded-[6px] border border-border bg-raised">
          <Icon name={h().icon} size={12} color="foreground" />
        </View>
        <View class="min-w-0 flex-1">
          <Text class={`${FS.xs} font-bold text-foreground`}>{h().title}</Text>
          <If when={h().detail}>
            <Text class={`${FS.xs} ${MONO} text-muted-foreground line-clamp-1`}>{h().detail}</Text>
          </If>
        </View>
        <If
          when={props.step.state === "running"}
          else={
            <Text
              class={`${FS.xs} shrink-0`}
              classList={{ "text-negative": props.step.state === "error", "text-muted-foreground": props.step.state !== "error" }}
            >
              {props.step.state === "error" ? t("chat.tool_failed") : props.step.summary}
            </Text>
          }
        >
          <View class="shrink-0 flex-row items-center gap-1.5">
            <Icon name="loader-circle" size={12} color="ai" />
            <Text class={`${FS.xs} text-muted-foreground`}>{t("chat.tool_running")}</Text>
          </View>
        </If>
        <Icon name={open() ? "chevron-up" : "chevron-down"} size={12} color="fg-faint" />
      </View>
      <If when={open() && props.step.state !== "running"}>
        <Body step={props.step} wide={props.wide} onOpenQuery={props.onOpenQuery} />
      </If>
    </View>
  );
}
