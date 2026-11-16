// 数据对比 tab:源表(本连接)对比另一个同类型库(或本库)里的表,按键逐行比较数据;
// 上面是选择与进度,中间是差异行(按类别筛选,值不同的格子标出 源 → 目标),下面是让目标表与源表一致的同步脚本。

import { Clipboard, type JSX, Text, View } from "nsh";
import { DataTable, type DataTableColumn, Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { connLabel } from "../chat/actions";
import { Dropdown, Segmented, Toggle } from "../dialogs/controls";
import type { DataDiffRow } from "../lib/types";
import { conns } from "../store/conns";
import { maskRowsOf } from "../store/privacy";
import { toast } from "../store/toast";
import { ErrorText } from "../ui/error";
import { Btn, DbIcon, EnvBadge, Hint, Icon, IconButton, TextField } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import {
  canRun,
  cancelCompare,
  type DataDiffFilter,
  type DataDiffModel,
  keyColumns,
  runCompare,
  setIncludeDeletes,
  setSourceTable,
  setTargetConn,
  setTargetTable,
  swap,
} from "./dataDiffModel";
import { openQueryOn } from "./index";

const KIND_CLASS: Record<DataDiffRow["kind"], string> = {
  onlySource: "text-success border-success",
  onlyTarget: "text-negative border-negative",
  different: "text-warning border-warning",
};

const fmt = (n: number) => n.toLocaleString();

function cellText(v: string | null | undefined): string {
  if (v === null || v === undefined) return "NULL";
  return v.replace(/[\r\n]/g, " ");
}

function Side(props: { label: string; children: JSX.Element }) {
  return (
    <View class="flex-row flex-wrap items-center gap-2 rounded-(--radius) border border-border bg-panel px-3 py-1.5">
      <Text class={`${FS.xs} text-muted-foreground`}>{props.label}</Text>
      {props.children}
    </View>
  );
}

function tableItems(id: string | null) {
  return (id ? (conns.tables(id) ?? []) : []).map((x) => ({ value: x, title: x, icon: "table" as const }));
}

/** 选择区:源 → 目标、键、筛选与上限、开始 / 停止。 */
function Setup(props: { m: DataDiffModel }) {
  const m = props.m;
  const source = () => conns.get(m.sourceConn());
  const targets = () => conns.list().filter((c) => c.kind === source()?.kind);
  const target = () => conns.get(m.targetConn());
  const ins = () => m.inspection();
  return (
    <View class="shrink-0 gap-3 border-b border-border px-5 py-4">
      <View class="flex-row flex-wrap items-center gap-2">
        <Icon name="arrow-left-right" size={16} color="data" />
        <Text class={`${FS.base} font-bold text-foreground`}>{t("ddiff.title")}</Text>
        <Text class={`${FS.sm} text-muted-foreground`}>{t("ddiff.subtitle")}</Text>
      </View>
      <View class="flex-row flex-wrap items-center gap-3">
        <Side label={t("ddiff.source")}>
          <If when={source()}>
            {(c) => (
              <>
                <DbIcon kind={c().kind} size={13} />
                <Text class={`${FS.sm} font-bold text-foreground`}>{connLabel(c())}</Text>
                <EnvBadge env={c().env} />
              </>
            )}
          </If>
          <Dropdown
            testID="ddiff-source-table"
            small
            items={tableItems(m.sourceConn())}
            value={m.sourceTable()}
            label={m.sourceTable() ?? ""}
            placeholder={t("ddiff.pick_table")}
            accessibilityLabel={t("ddiff.source_table")}
            disabled={m.running()}
            onChange={(v) => setSourceTable(m, v)}
            style={{ width: 180 }}
          />
        </Side>
        <IconButton
          icon="arrow-left-right"
          size={14}
          testID="ddiff-swap"
          tooltip={t("ddiff.swap_tooltip")}
          label={t("ddiff.swap")}
          disabled={m.running() || !m.targetConn()}
          onPress={() => swap(m)}
          class="p-1.5"
        />
        <Side label={t("ddiff.target")}>
          <Dropdown
            testID="ddiff-target-conn"
            small
            items={targets().map((c) => ({ value: c.id, title: connLabel(c), leading: () => <DbIcon kind={c.kind} size={13} /> }))}
            value={m.targetConn()}
            label={target() ? connLabel(target()!) : ""}
            leading={target() ? <DbIcon kind={target()!.kind} size={13} /> : undefined}
            accessibilityLabel={t("ddiff.target_conn")}
            disabled={m.running()}
            onChange={(v) => setTargetConn(m, v)}
            style={{ width: 180 }}
          />
          <If when={target()}>{(c) => <EnvBadge env={c().env} />}</If>
          <Dropdown
            testID="ddiff-target-table"
            small
            items={tableItems(m.targetConn())}
            value={m.targetTable()}
            label={m.targetTable() ?? ""}
            placeholder={t("ddiff.pick_table")}
            accessibilityLabel={t("ddiff.target_table")}
            disabled={m.running()}
            onChange={(v) => setTargetTable(m, v)}
            style={{ width: 180 }}
          />
        </Side>
      </View>
      <View class="flex-row flex-wrap items-center gap-3">
        <View class="flex-row items-center gap-2">
          <Text class={`${FS.xs} text-muted-foreground`}>{t("ddiff.key")}</Text>
          <If
            when={ins()}
            else={<Text class={`${FS.sm} text-fg-faint`}>—</Text>}
          >
            {(i) => (
              <If
                when={i().primaryKey.length > 0}
                else={
                  <If
                    when={i().uniqueKeys.length > 0}
                    else={<Text testID="ddiff-no-key" class={`${FS.sm} text-negative`}>{t("ddiff.no_key")}</Text>}
                  >
                    <Dropdown
                      testID="ddiff-unique-key"
                      small
                      items={i().uniqueKeys.map((u) => ({ value: u.columns.join(","), title: `${u.columns.join(", ")} (${u.name})`, icon: "key-round" as const }))}
                      value={m.uniqueKey()}
                      label={m.uniqueKey()?.split(",").join(", ") ?? ""}
                      accessibilityLabel={t("ddiff.unique_key")}
                      tooltip={t("ddiff.unique_key_tooltip")}
                      disabled={m.running()}
                      onChange={(v) => m.uniqueKey.set(v)}
                      style={{ width: 200 }}
                    />
                  </If>
                }
              >
                <View testID="ddiff-key" class="flex-row items-center gap-1.5">
                  <Icon name="key-round" size={13} color="data" />
                  <Text class={`${FS.sm} ${MONO} text-foreground`}>{i().primaryKey.join(", ")}</Text>
                  <Text class={`${FS.xs} text-muted-foreground`}>{t("ddiff.primary_key")}</Text>
                </View>
              </If>
            )}
          </If>
        </View>
        <TextField
          testID="ddiff-filter"
          class="h-7 min-w-[200px] flex-1 font-mono"
          placeholder={t("ddiff.filter_placeholder")}
          value={m.filter()}
          disabled={m.running()}
          onChange={({ value }) => m.filter.set(value)}
        />
        <View class="flex-row items-center gap-2">
          <Text class={`${FS.xs} text-muted-foreground`}>{t("ddiff.max_rows")}</Text>
          <TextField
            testID="ddiff-max-rows"
            class="h-7 w-[100px] font-mono"
            value={m.maxRows()}
            disabled={m.running()}
            onChange={({ value }) => m.maxRows.set(value)}
          />
        </View>
        <If
          when={m.running()}
          else={
            <Btn
              testID="ddiff-run"
              small
              variant="primary"
              icon="play"
              label={m.report() ? t("ddiff.rerun") : t("ddiff.run")}
              disabled={!canRun(m)}
              onPress={() => void runCompare(m)}
            />
          }
        >
          <Btn testID="ddiff-cancel" small icon="x" label={t("ddiff.cancel")} onPress={() => cancelCompare(m)} />
        </If>
      </View>
      <If when={ins() && (ins()!.onlySource.length > 0 || ins()!.onlyTarget.length > 0)}>
        <Text testID="ddiff-columns-note" class={`${FS.xs} text-muted-foreground`}>
          {[
            ins()!.onlySource.length > 0 ? t("ddiff.cols_only_source", { cols: ins()!.onlySource.join(", ") }) : "",
            ins()!.onlyTarget.length > 0 ? t("ddiff.cols_only_target", { cols: ins()!.onlyTarget.join(", ") }) : "",
          ]
            .filter(Boolean)
            .join(" · ")}
        </Text>
      </If>
      <If when={m.inspectError()}>{(e) => <ErrorText text={e()} />}</If>
    </View>
  );
}

function Cell(props: { m: DataDiffModel; row: DataDiffRow | undefined; col: number }) {
  const r = () => props.row;
  const changed = () => !!r() && r()!.kind === "different" && r()!.changed.includes(props.col - 1);
  const text = () => {
    const row = r();
    if (!row) return "";
    if (props.col === 0) return t(`ddiff.kind_${row.kind}`);
    const i = props.col - 1;
    if (row.kind === "onlySource") return cellText(row.source?.[i]);
    if (row.kind === "onlyTarget") return cellText(row.target?.[i]);
    if (changed()) return `${cellText(row.source?.[i])} → ${cellText(row.target?.[i])}`;
    return cellText(row.source?.[i]);
  };
  const isNull = () => {
    const row = r();
    if (!row || props.col === 0 || changed()) return false;
    const vals = row.kind === "onlyTarget" ? row.target : row.source;
    return vals?.[props.col - 1] === null;
  };
  return (
    <Text
      class={`self-stretch px-3 line-clamp-1 ${props.col === 0 ? `${FS.xs} font-bold` : isNull() ? `${FS.xs} font-semibold` : FS.sm}`}
      style={{ lineHeight: 34 }}
      classList={{
        "text-success": props.col === 0 && r()?.kind === "onlySource",
        "text-negative": props.col === 0 && r()?.kind === "onlyTarget",
        "text-warning": props.col === 0 && r()?.kind === "different",
        "text-cell-null": isNull(),
        "text-cell": props.col > 0 && !isNull() && !changed(),
        "opacity-60 line-through": props.col > 0 && r()?.kind === "onlyTarget",
        "font-mono": props.col > 0 && props.m.report()?.classes[props.col - 1] !== "text",
        "bg-dirty border border-primary text-foreground": changed(),
      }}
    >
      {text()}
    </Text>
  );
}

function Results(props: { m: DataDiffModel }) {
  const m = props.m;
  const rep = () => m.report()!;
  // 展示用:两侧各按自己连接的规则遮罩敏感列(同步脚本仍用原值,它要拿去执行)
  const shown = (): DataDiffRow[] => {
    const r = rep();
    const src = maskRowsOf(m.sourceConn(), r.sourceTable, r.columns, r.rows.map((x) => x.source));
    const dst = maskRowsOf(m.targetConn(), r.targetTable, r.columns, r.rows.map((x) => x.target));
    return r.rows.map((x, i) => ({ ...x, source: src[i], target: dst[i] }));
  };
  const rows = () => {
    const c = m.category();
    const all = shown();
    return c === "all" ? all : all.filter((r) => r.kind === c);
  };
  const keys = () => new Set(rep().key);
  const columns = (): DataTableColumn[] => [
    { key: "kind", title: t("ddiff.col_status"), width: 120, minWidth: 80, maxWidth: 240, movable: false },
    ...rep().columns.map((name, i) => ({
      key: String(i),
      title: name,
      width: rep().key.includes(name) ? 120 : 180,
      minWidth: 48,
      maxWidth: 2400,
      align: rep().classes[i] === "number" ? ("end" as const) : ("start" as const),
      movable: false,
    })),
  ];
  const options = (): { value: DataDiffFilter; label: string; color?: string }[] => [
    { value: "all", label: t("ddiff.cat_all", { n: fmt(rep().onlySource + rep().onlyTarget + rep().different) }) },
    { value: "onlySource", label: t("ddiff.cat_only_source", { n: fmt(rep().onlySource) }), color: "success" },
    { value: "onlyTarget", label: t("ddiff.cat_only_target", { n: fmt(rep().onlyTarget) }), color: "negative" },
    { value: "different", label: t("ddiff.cat_different", { n: fmt(rep().different) }), color: "warning" },
  ];
  return (
    <View class="min-h-0 flex-1">
      <View class="shrink-0 gap-2 px-5 py-3">
        <View class="flex-row flex-wrap items-center gap-3">
          <Text testID="ddiff-summary" class={`${FS.sm} text-muted-foreground`}>
            {t("ddiff.summary", {
              identical: fmt(rep().identical),
              source: fmt(rep().sourceRows),
              target: fmt(rep().targetRows),
            })}
          </Text>
        </View>
        <View class="max-w-[720px]">
          <Segmented testID="ddiff-category" options={options()} value={m.category()} onChange={(v) => m.category.set(v)} />
        </View>
        <If when={rep().capped}>
          <Text testID="ddiff-capped" class={`${FS.xs} text-warning`}>{t("ddiff.capped", { n: fmt(rep().maxRows) })}</Text>
        </If>
        <If when={rep().truncated}>
          <Text testID="ddiff-truncated" class={`${FS.xs} text-warning`}>
            {t("ddiff.truncated", { kept: fmt(rep().rows.length) })}
          </Text>
        </If>
        <If when={rep().cancelled}>
          <Text class={`${FS.xs} text-warning`}>{t("ddiff.cancelled")}</Text>
        </If>
      </View>
      <View class="min-h-0 flex-[3] border-t border-border">
        <DataTable
          testID="ddiff-grid"
          class="flex-1"
          columns={columns()}
          data={rows()}
          rowHeight={34}
          headerHeight={36}
          stripe
          rowSelectable={false}
          columnSelectable={false}
          cellSelectable={false}
          movableColumns={false}
          renderCell={(c) => <Cell m={m} row={rows()[c.row]} col={c.column} />}
          renderHeaderCell={(h) => (
            <View class="flex-1 self-stretch flex-row items-center gap-1.5 px-3">
              <If when={h.column > 0 && keys().has(rep().columns[h.column - 1])}>
                <Icon name="key-round" size={12} color="data" />
              </If>
              <Text class={`${FS.sm} font-bold text-foreground line-clamp-1`}>
                {h.column === 0 ? t("ddiff.col_status") : rep().columns[h.column - 1]}
              </Text>
            </View>
          )}
          empty={
            <Text class={`${FS.sm} text-fg-faint`}>
              {rep().onlySource + rep().onlyTarget + rep().different === 0 ? t("ddiff.identical") : t("ddiff.none_in_category")}
            </Text>
          }
          classes={{
            root: "bg-background",
            header: "border-b border-border bg-background",
            headerCell: "px-0",
            headerContent: "self-stretch",
            resizeHandle: "bg-border group-hover:bg-primary group-active:bg-primary",
            row: "border-b border-border",
            rowOdd: "bg-list-even",
            rowHover: "bg-list-hover",
            cell: "px-0",
            empty: "items-center justify-center",
          }}
        />
      </View>
      <View class="min-h-0 flex-[2] border-t border-border">
        <View class="h-10 shrink-0 flex-row items-center gap-3 border-b border-border px-3">
          <Text class={`${FS.xs} flex-1 font-bold text-muted-foreground line-clamp-1`}>{t("ddiff.script")}</Text>
          <Toggle
            testID="ddiff-include-deletes"
            checked={m.includeDeletes()}
            label={t("ddiff.include_deletes")}
            onChange={(v) => setIncludeDeletes(m, v)}
          />
          <Btn testID="ddiff-copy" small variant="ghost" icon="copy" label={t("ddiff.copy")} onPress={() => void copy(m)} />
          <Btn
            testID="ddiff-open"
            small
            variant="outline"
            icon="square-play"
            label={t("ddiff.open_in_editor")}
            tooltip={t("ddiff.open_tooltip")}
            disabled={rep().onlySource + rep().onlyTarget + rep().different === 0 || !m.targetConn()}
            onPress={() => m.targetConn() && openQueryOn(m.targetConn()!, m.script())}
          />
        </View>
        <Editor testID="ddiff-sql" class="flex-1 bg-editor" value={m.script()} readOnly language="sql" fontSize={13} softWrap />
      </View>
    </View>
  );
}

async function copy(m: DataDiffModel) {
  if (!m.script()) return;
  await Clipboard.writeText(m.script());
  toast.success(t("ddiff.copied"));
}

export function DataDiff(props: { m: DataDiffModel }) {
  const m = props.m;
  return (
    <View testID="data-diff" class="min-h-0 flex-1 bg-background">
      <Setup m={m} />
      <If when={m.error()}>{(e) => <ErrorText class="px-5 pt-3" text={e()} />}</If>
      <If
        when={m.report()}
        else={
          <View class="flex-1 items-center justify-center gap-2 p-6">
            <If
              when={m.running()}
              else={<Hint>{keyColumns(m).length > 0 || !m.inspection() ? t("ddiff.empty") : t("ddiff.no_key_hint")}</Hint>}
            >
              <Icon name="loader-circle" size={18} color="data" />
              <Text testID="ddiff-progress" class={`${FS.sm} text-muted-foreground`}>
                {t("ddiff.running", {
                  source: fmt(m.progress()?.sourceRows ?? 0),
                  target: fmt(m.progress()?.targetRows ?? 0),
                })}
              </Text>
            </If>
          </View>
        }
      >
        <Results m={m} />
      </If>
    </View>
  );
}
