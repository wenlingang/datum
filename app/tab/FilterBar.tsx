// 结果区筛选条:条件之间用统一的连接词(且/或),编辑只改草稿,
// 点「应用」或在值输入框回车才作用到结果。对应 GPUI 版 tab/filter.rs。
// 在 JSON 树上点字段生成的条件带字段路径,列名后面标出取值表达式(PG 下推成 WHERE)。

import { signal, Text, View } from "nsh";
import { Button } from "nsh/base";
import { t } from "nsh/i18n";
import { type DbKind, FILTER_OPS, type FilterOp, type JsonPath } from "../lib/types";
import { Btn, Icon, IconButton, TextField } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { grid } from "../lib/native";
import {
  filterAdd,
  filterApply,
  filterClear,
  filterDirty,
  filterFromText,
  filterOnServer,
  filterPreviewCount,
  removeDraft,
  updateDraft,
  type TabModel,
} from "./model";
import { SelectMenu } from "./ui";
import { ErrorText } from "../ui/error";

const OP_KEY: Record<FilterOp, string> = {
  contains: "filter.op.contains",
  eq: "filter.op.eq",
  ne: "filter.op.ne",
  starts: "filter.op.starts",
  ends: "filter.op.ends",
  gt: "filter.op.gt",
  lt: "filter.op.lt",
  gte: "filter.op.gte",
  lte: "filter.op.lte",
  empty: "filter.op.empty",
  notEmpty: "filter.op.not_empty",
};

const needsValue = (op: FilterOp) => op !== "empty" && op !== "notEmpty";

/** 字段路径去掉开头的列名:PG 为 `->'a'->>'b'`;MySQL / SQLite 的 `$.a.b` 本就不含列名。 */
function pathSuffix(kind: DbKind, col: string, path: JsonPath): string {
  const full = grid.jsonPathText(kind, col, path);
  const head = grid.jsonPathText(kind, col, []);
  return head !== "$" && full.startsWith(head) ? full.slice(head.length) : full;
}

export function FilterBar(props: { m: TabModel }) {
  const m = props.m;
  const columns = () => m.result()?.columns ?? [];
  const total = () => m.result()?.rows.length ?? 0;
  const canClear = () => m.drafts().length > 0 || m.applied().length > 0;
  const nl = signal("");
  return (
    <View class="max-h-[260px] shrink-0 gap-2 border-b border-border bg-panel px-5 py-3">
      {/* 自然语言筛选:一句话交给 AI,转成下面的且/或条件 */}
      <View class="flex-row items-center gap-2">
        <View class="w-9 shrink-0 items-center">
          <Icon name="sparkles" size={14} color="ai" />
        </View>
        <TextField
          testID="filter-nl"
          class="h-7 min-w-0 flex-1"
          placeholder={t("filter.nl_placeholder")}
          value={nl()}
          disabled={m.nlBusy()}
          onChange={({ value }) => nl.set(value)}
          onSubmit={() => void filterFromText(m, nl())}
        />
        <Btn
          small
          testID="filter-nl-go"
          variant="ai"
          icon={m.nlBusy() ? "loader-circle" : "wand-sparkles"}
          disabled={m.nlBusy() || nl().trim() === ""}
          label={m.nlBusy() ? t("filter.nl_busy") : t("filter.nl_go")}
          onPress={() => void filterFromText(m, nl())}
        />
      </View>
      <If when={m.nlNote()}>
        {(n) => (
          <If
            when={n().error}
            else={
              <Text testID="filter-nl-note" class={`${FS.xs} pl-11 text-muted-foreground`}>
                {n().text}
              </Text>
            }
          >
            <ErrorText testID="filter-nl-note" size="xs" class="pl-11" text={n().text} />
          </If>
        )}
      </If>
      <Each of={m.drafts()} key={(d) => d.id}>
        {(d, i) => (
          <View class="flex-row items-center gap-2">
            <If
              when={i() > 0}
              else={<Text class={`${FS.xs} w-9 shrink-0 text-muted-foreground`}>{t("filter.where")}</Text>}
            >
              <Button
                onPress={() => m.conj.update((c) => (c === "and" ? "or" : "and"))}
                class="h-7 w-9 shrink-0 items-center justify-center rounded-(--radius) bg-selection"
              >
                <Text class={`${FS.xs} font-bold text-data`}>
                  {m.conj() === "and" ? t("filter.and") : t("filter.or")}
                </Text>
              </Button>
            </If>
            <SelectMenu
              testID={`filter-col:${d().id}`}
              width={184}
              label={d().col === "" ? t("filter.pick_column") : d().col}
              options={columns().map((c) => ({ value: c, label: c }))}
              // 换了列:原来的 JSON 字段路径不再适用
              onSelect={(col) => updateDraft(m, d().id, { col, path: undefined, jsonType: undefined })}
            />
            {/* JSON 树上点字段生成的条件:列之后标出字段路径(PG 的取值写法 `->'a'->>'b'`,列名已在左边,不重复) */}
            <If when={d().path?.length ? d().path : null}>
              {(path) => (
                <Text
                  testID={`filter-path:${d().id}`}
                  class={`${FS.xs} ${MONO} h-7 min-w-0 max-w-[240px] shrink rounded-(--radius) bg-selection px-2 text-data line-clamp-1`}
                  style={{ lineHeight: 28 }}
                >
                  {pathSuffix(m.dbKind, d().col, path())}
                </Text>
              )}
            </If>
            <SelectMenu
              testID={`filter-op:${d().id}`}
              width={120}
              label={t(OP_KEY[d().op])}
              options={FILTER_OPS.map((op) => ({ value: op, label: t(OP_KEY[op]) }))}
              onSelect={(op) => updateDraft(m, d().id, { op })}
            />
            <If
              when={needsValue(d().op)}
              else={
                <Text class={`${FS.xs} min-w-0 flex-1 px-3 italic text-fg-faint`}>{t("filter.no_value_needed")}</Text>
              }
            >
              <TextField
                testID={`filter-value:${d().id}`}
                class="h-7 min-w-[96px] flex-1"
                placeholder={t("filter.value_placeholder")}
                value={d().value}
                onChange={({ value }) => updateDraft(m, d().id, { value })}
                onSubmit={() => filterApply(m)}
              />
            </If>
            <IconButton icon="x" size={13} tooltip={t("filter.remove_condition")} onPress={() => removeDraft(m, d().id)} />
          </View>
        )}
      </Each>
      <If when={m.drafts().length === 0}>
        <Text class={`${FS.sm} py-2 text-fg-faint`}>{t("filter.empty")}</Text>
      </If>
      <View class="flex-row items-center justify-between">
        <Btn small variant="ghost" icon="plus" label={t("filter.add_condition")} onPress={() => filterAdd(m)} />
        <View class="flex-row items-center gap-3">
          <If
            when={filterOnServer(m)}
            else={
              <View class="flex-row items-center gap-1">
                <Text class={`${FS.xs} text-fg-faint`}>{t("filter.preview_match")}</Text>
                <Text class={`${FS.xs} font-mono font-bold text-fg-soft`}>{String(filterPreviewCount(m))}</Text>
                <Text class={`${FS.xs} text-fg-faint`}>{t("filter.of_total_rows", { total: total() })}</Text>
              </View>
            }
          >
            <Text testID="filter-server-hint" class={`${FS.xs} text-fg-faint`}>{t("filter.whole_table")}</Text>
          </If>
          <If when={canClear()}>
            <Btn small variant="ghost" label={t("filter.clear")} onPress={() => filterClear(m)} />
          </If>
          <Btn
            small
            testID="filter-apply"
            icon="check"
            variant={filterDirty(m) ? "primary" : "outline"}
            disabled={!filterDirty(m)}
            label={filterDirty(m) ? t("filter.apply") : t("filter.applied")}
            onPress={() => filterApply(m)}
          />
        </View>
      </View>
    </View>
  );
}
