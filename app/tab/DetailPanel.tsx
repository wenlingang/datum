// 右侧详情栏:选中行时显示行详情;Redis 显示 key 详情;否则显示表结构(字段 + 索引)。
// 对应 GPUI 版 workspace/detail.rs。

import { Clipboard, Scroll, Text, View, type JSX } from "nsh";
import { Menu as NativeMenu } from "nsh/desktop";
import { t } from "nsh/i18n";
import { grid } from "../lib/native";
import { type KeyDetail, supportsSql, type TableDetail } from "../lib/types";
import { toast } from "../store/toast";
import { workspace } from "../store/workspace";
import { SectionLabel } from "../ui/kit";
import { FS } from "../ui/text";
import { columnTypes, modelOf, viewRow, type TabModel } from "./model";

function Header(props: { children: string }) {
  return (
    <Text class={`${FS.xs} shrink-0 border-b border-border px-3 py-2 font-bold text-muted-foreground`}>
      {props.children}
    </Text>
  );
}

function RowDetail(props: { m: TabModel; row: number }) {
  const m = props.m;
  const columns = () => m.result()?.columns ?? [];
  const values = () => viewRow(m, props.row) ?? [];
  const types = () => columnTypes(m);
  // Mongo:该文档没有的字段不列出(缺失 ≠ null)
  const fields = () =>
    columns()
      .map((c, i) => ({ c, v: values()[i] ?? null }))
      .filter((f) => !m.mongo || f.v !== null);
  return (
    <View class="flex-1">
      <Header>{t("detail.row_detail")}</Header>
      <Scroll testID="row-detail" class="min-h-0 flex-1">
        <Each of={fields()} key={(f) => f.c}>
          {(f) => (
            <View class="gap-1 border-b border-border px-3 py-2">
              <View class="flex-row items-center justify-between gap-2">
                <Text class={`${FS.xs} font-semibold text-foreground`}>{f().c}</Text>
                <Text class="shrink-0 text-data" style={{ fontSize: 10 }}>
                  {types()[f().c] ?? "—"}
                </Text>
              </View>
              <Text
                class={`${FS.xs} font-mono`}
                classList={{ "text-foreground": f().v !== null, "text-muted-foreground": f().v === null }}
              >
                {f().v ?? "NULL"}
              </Text>
            </View>
          )}
        </Each>
      </Scroll>
    </View>
  );
}

const EMPTY_KEY: KeyDetail = { key: "", keyType: "", ttl: -2, size: 0 };

function RedisDetail(props: { m: TabModel }) {
  const d = () => props.m.redisDetail() ?? EMPTY_KEY;
  const ttl = () => (d().ttl === -1 ? t("detail.ttl_never") : d().ttl === -2 ? t("detail.ttl_missing") : `${d().ttl}s`);
  const line = (label: string, value: JSX.Element) => (
    <View class="flex-row gap-2">
      <Text class={`${FS.xs} w-9 shrink-0 text-muted-foreground`}>{label}</Text>
      <View class="min-w-0 flex-1">{value}</View>
    </View>
  );
  return (
    <View class="flex-1">
      <Header>{t("detail.key_detail")}</Header>
      <View class="gap-2 p-3">
        {line(t("detail.key"), <Text class={`${FS.xs} font-bold text-foreground`}>{d().key}</Text>)}
        {line(t("detail.type"), <Text class={`${FS.xs} text-data`}>{d().keyType}</Text>)}
        {line("TTL", <Text class={`${FS.xs} text-foreground`}>{ttl()}</Text>)}
        {line(t("detail.size"), <Text class={`${FS.xs} text-foreground`}>{String(d().size)}</Text>)}
      </View>
    </View>
  );
}

function Cell(props: { w: number; children: JSX.Element; class?: string }) {
  return (
    <View class={`shrink-0 overflow-hidden ${props.class ?? ""}`} style={{ width: props.w }}>
      {props.children}
    </View>
  );
}

function Dim(props: { v: string | null | undefined }) {
  return (
    <Text class={`${FS.xs} line-clamp-1`} classList={{ "text-foreground": !!props.v, "text-muted-foreground": !props.v }}>
      {props.v ? props.v : "—"}
    </Text>
  );
}

function Check(props: { on: boolean }) {
  return <Text class={`${FS.xs} text-primary`}>{props.on ? "✓" : ""}</Text>;
}

function HeadRow(props: { cols: [string, number][] }) {
  return (
    <View class="flex-row border-b border-border px-3 py-1">
      <Each of={props.cols}>
        {(c) => (
          <Cell w={c()[1]}>
            <Text class={`${FS.xs} font-semibold text-muted-foreground line-clamp-1`}>{c()[0]}</Text>
          </Cell>
        )}
      </Each>
    </View>
  );
}

const EMPTY_DETAIL: TableDetail = { columns: [], indexes: [] };

function TableDetailView(props: { m: TabModel; table: string }) {
  const d = () => props.m.detail() ?? EMPTY_DETAIL;
  const copyInsert = async () => {
    const sql = grid.buildInsert(
      props.table,
      d().columns.map((c) => c.name),
      props.m.dbKind,
    );
    await Clipboard.writeText(sql);
    toast.success(t("workspace.copied"));
  };
  return (
    <View class="flex-1">
      <Header>{t("detail.table_title", { name: props.table })}</Header>
      <Scroll horizontal class="min-h-0 flex-1" contentClass="min-w-[560px]">
        <View class="flex-1 min-w-[560px]">
          <Scroll class="min-h-0 flex-1">
            <Text class="px-3 py-1 text-muted-foreground" style={{ fontSize: 10 }}>
              {t("detail.columns_n", { count: d().columns.length })}
            </Text>
            <HeadRow
              cols={[
                ["Name", 120],
                ["Type", 110],
                ["Default", 90],
                ["Comment", 110],
                ["NotNull", 56],
                ["Primary", 56],
              ]}
            />
            <Each of={d().columns} key={(c) => c.name}>
              {(c) => (
                <View
                  testID={`col:${c().name}`}
                  class="flex-row border-b border-border px-3 py-1"
                  onContextMenu={(e) =>
                    void NativeMenu.popup(
                      [{ label: t("detail.copy_insert"), onSelect: () => void copyInsert() }],
                      e.pagePosition,
                    )
                  }
                >
                  <Cell w={120}>
                    <Text class={`${FS.xs} font-semibold text-foreground line-clamp-1`}>{c().name}</Text>
                  </Cell>
                  <Cell w={110}>
                    <Text class={`${FS.xs} text-data line-clamp-1`}>{c().dataType}</Text>
                  </Cell>
                  <Cell w={90}>
                    <Dim v={c().default} />
                  </Cell>
                  <Cell w={110}>
                    <Dim v={c().comment} />
                  </Cell>
                  <Cell w={56}>
                    <Check on={c().notNull} />
                  </Cell>
                  <Cell w={56}>
                    <Check on={c().isPk} />
                  </Cell>
                </View>
              )}
            </Each>
          </Scroll>
          <Scroll class="min-h-0 flex-1 border-t-2 border-border">
            <Text class="px-3 py-1 text-muted-foreground" style={{ fontSize: 10 }}>
              {t("detail.indexes_n", { count: d().indexes.length })}
            </Text>
            <HeadRow
              cols={[
                ["Name", 160],
                ["Columns", 160],
                ["Primary", 56],
                ["Unique", 56],
              ]}
            />
            <Each of={d().indexes} key={(i) => i.name}>
              {(i) => (
                <View class="flex-row border-b border-border px-3 py-1">
                  <Cell w={160}>
                    <Text class={`${FS.xs} text-foreground line-clamp-1`}>{i().name}</Text>
                  </Cell>
                  <Cell w={160}>
                    <Text class={`${FS.xs} text-foreground line-clamp-1`}>{i().columns}</Text>
                  </Cell>
                  <Cell w={56}>
                    <Check on={i().isPrimary} />
                  </Cell>
                  <Cell w={56}>
                    <Check on={i().isUnique} />
                  </Cell>
                </View>
              )}
            </Each>
            <If when={d().indexes.length === 0}>
              <Text class={`${FS.xs} px-3 py-1 text-muted-foreground`}>{t("detail.no_indexes")}</Text>
            </If>
          </Scroll>
        </View>
      </Scroll>
    </View>
  );
}

type Mode =
  | { kind: "row"; m: TabModel; row: number }
  | { kind: "redis"; m: TabModel; key: string }
  | { kind: "table"; m: TabModel; table: string }
  | { kind: "none" };

function modeOf(m: TabModel | undefined): Mode {
  if (!m) return { kind: "none" };
  const row = m.selectedRow();
  if (m.result() && row !== null && viewRow(m, row)) return { kind: "row", m, row };
  const rd = m.redisDetail();
  if (rd) return { kind: "redis", m, key: rd.key };
  if (m.detail() && m.table && supportsSql(m.dbKind)) return { kind: "table", m, table: m.table };
  return { kind: "none" };
}

const modeKey = (mode: Mode) =>
  mode.kind === "row"
    ? `row:${mode.row}`
    : mode.kind === "redis"
      ? `redis:${mode.key}`
      : mode.kind === "table"
        ? `table:${mode.table}`
        : "none";

/** 右侧详情栏(跟随当前 tab)。 */
export function DetailPanelView() {
  const mode = () => modeOf(modelOf(workspace.activeTab()));
  return (
    <View testID="detail-panel" class="flex-1 border-l border-border bg-panel">
      <Each of={[mode()]} key={(x) => `${workspace.activeTab()?.id ?? ""}:${modeKey(x)}`}>
        {(x) => {
          const v = x();
          if (v.kind === "row") return <RowDetail m={v.m} row={v.row} />;
          if (v.kind === "redis") return <RedisDetail m={v.m} />;
          if (v.kind === "table") return <TableDetailView m={v.m} table={v.table} />;
          return (
            <View class="p-2">
              <SectionLabel>{t("detail.details")}</SectionLabel>
            </View>
          );
        }}
      </Each>
    </View>
  );
}
