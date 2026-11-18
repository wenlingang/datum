// 详情弹窗:行详情(结果表格右键「查看详情」)与表详情(侧栏表右键「查看表详情」:字段 + 索引)。
// 以前是右侧常驻的详情栏;这两个都是偶尔看一眼的场景,改成弹窗,看完关掉。
// Redis key 的类型 / 大小 / TTL 在 key 编辑页顶部常驻显示,不再单独放。

import { Clipboard, Scroll, Text, View, type JSX } from "nsh";
import { DataTable, type DataTableContextMenu } from "nsh/base";
import { Menu as NativeMenu } from "nsh/desktop";
import { t } from "nsh/i18n";
import { DialogFrame } from "../dialogs/controls";
import { grid } from "../lib/native";
import type { DbKind, TableDetail } from "../lib/types";
import { toast } from "../store/toast";
import { Icon } from "../ui/kit";
import { FS } from "../ui/text";
import { JsonTree } from "./JsonTree";
import { canFilterJson, closeRowDetail, columnTypes, filterByJson, isJsonDoc, viewRow, type TabModel } from "./model";
import { displayValue, isMaskedCell } from "./mask";

// ------------------------------------------------------------ 行详情

/** 结果表格右键「查看详情」:这一行的全部字段(敏感列遮罩,JSON 展开成树)。 */
export function RowDetailDialog(props: { m: TabModel }) {
  const m = props.m;
  // 结果变了(翻页、重新执行)这一行可能已不存在:不显示
  const row = () => {
    const r = m.rowDetail();
    return r !== null && m.result() && viewRow(m, r) ? r : null;
  };
  return (
    <DialogFrame
      open={row() !== null}
      onClose={() => closeRowDetail(m)}
      title={t("detail.row_detail")}
      width={640}
      testID="row-detail-dialog"
    >
      <Each of={row() !== null ? [row()!] : []} key={(r) => r}>
        {(r) => <RowFields m={m} row={r()} />}
      </Each>
    </DialogFrame>
  );
}

function RowFields(props: { m: TabModel; row: number }) {
  const m = props.m;
  const columns = () => m.result()?.columns ?? [];
  const types = () => columnTypes(m);
  // 展示值:敏感列遮罩(与网格一致,点过「显示原值」的格子除外)
  const fields = () =>
    columns().map((c, i) => ({ c, v: displayValue(m, props.row, i), masked: isMaskedCell(m, props.row, i) }));
  return (
    <Scroll testID="row-detail" class="max-h-[560px] min-h-0 rounded-(--radius) border border-border">
      <Each of={fields()} key={(f) => f.c}>
        {(f) => (
          <View class="gap-1 border-b border-border px-3 py-2">
            <View class="flex-row items-center justify-between gap-2">
              <View class="min-w-0 flex-row items-center gap-1">
                <Text class={`${FS.xs} font-semibold text-foreground`}>{f().c}</Text>
                <If when={f().masked}>
                  <Icon name="eye-off" size={11} color="fg-faint" />
                </If>
              </View>
              <Text class={`shrink-0 text-data ${FS.xs}`}>{types()[f().c] ?? "—"}</Text>
            </View>
            {/* JSON 对象 / 数组展开成树(PG 上可点字段生成筛选),其余原样显示 */}
            <If
              when={!f().masked && isJsonDoc(f().v) ? f().v : null}
              else={
                <Text
                  testID={f().masked ? `row-masked:${f().c}` : undefined}
                  class={`${FS.xs} font-mono select-text`}
                  classList={{ "text-foreground": f().v !== null && !f().masked, "text-muted-foreground": f().v === null || f().masked }}
                >
                  {f().v ?? "NULL"}
                </Text>
              }
            >
              {(text) => (
                <JsonTree
                  testID={`row-json:${f().c}`}
                  text={text()}
                  column={f().c}
                  kind={m.dbKind}
                  depth={1}
                  onFilter={
                    canFilterJson(m, f().c)
                      ? (path, kind, value) => {
                          // 筛选会重新查询,这一行的位置随之变化:关掉弹窗回到表格
                          closeRowDetail(m);
                          filterByJson(m, f().c, path, kind, value);
                        }
                      : undefined
                  }
                />
              )}
            </If>
          </View>
        )}
      </Each>
    </Scroll>
  );
}

// ------------------------------------------------------------ 表详情

/** 正在查看的表详情(侧栏「查看表详情」)。 */
export interface TableDetailView {
  table: string;
  kind: DbKind;
  detail: TableDetail;
}

function Dim(props: { v: string | null | undefined }) {
  return (
    <Text class={`${FS.xs} px-2 line-clamp-1`} classList={{ "text-foreground": !!props.v, "text-muted-foreground": !props.v }}>
      {props.v ? props.v : "—"}
    </Text>
  );
}

function Check(props: { on: boolean }) {
  return <Text class={`${FS.xs} px-2 text-primary`}>{props.on ? "✓" : ""}</Text>;
}

/** 表结构的一张只读小表(字段 / 索引):表格自己横竖滚动。 */
function MiniTable<T>(props: {
  testID: string;
  label: string;
  columns: [key: string, title: string, width: number][];
  data: T[];
  cell: (row: T, key: string) => JSX.Element;
  onContextMenu?: (e: DataTableContextMenu) => void;
  empty?: string;
  /** 高度(点):DataTable 要有高度才显示行 */
  height: number;
}) {
  return (
    <DataTable
      testID={props.testID}
      accessibilityLabel={props.label}
      class="rounded-(--radius) border border-border"
      style={{ height: props.height }}
      columns={props.columns.map(([key, title, width]) => ({ key, title, width, minWidth: 40, maxWidth: 600 }))}
      data={props.data}
      rowHeight={26}
      headerHeight={26}
      rowSelectable={false}
      columnSelectable={false}
      movableColumns={false}
      renderCell={(c) => {
        const row = props.data[c.row];
        return row === undefined ? <Text /> : props.cell(row, c.key);
      }}
      onContextMenu={props.onContextMenu}
      empty={props.empty ? <Text class={`${FS.xs} px-3 py-1 text-muted-foreground`}>{props.empty}</Text> : undefined}
      classes={{
        root: "bg-popover",
        header: "border-b border-border bg-popover",
        headerCell: `px-2 ${FS.xs} font-semibold text-muted-foreground`,
        row: "border-b border-border",
        rowHover: "bg-list-hover",
        rowActive: "bg-list-hover",
        cell: "px-0",
        empty: "items-start justify-start",
      }}
    />
  );
}

/** 表详情弹窗:字段与索引两张表;字段表右键「复制 INSERT」。 */
export function TableDetailDialog(props: { view: TableDetailView | null; onClose: () => void }) {
  return (
    <DialogFrame
      open={props.view !== null}
      onClose={props.onClose}
      title={props.view ? t("detail.table_title", { name: props.view.table }) : ""}
      width={760}
      testID="table-detail-dialog"
    >
      <If when={props.view}>{(v) => <TableDetailBody view={v()} />}</If>
    </DialogFrame>
  );
}

/** 表格高度:按行数撑开(表头 + 行),最多 `maxRows` 行,再多就在表格里滚动。 */
const tableHeight = (rows: number, maxRows: number) => 26 * (Math.min(Math.max(rows, 1), maxRows) + 1) + 2;

function TableDetailBody(props: { view: TableDetailView }) {
  const d = () => props.view.detail;
  const copyInsert = async () => {
    const sql = grid.buildInsert(
      props.view.table,
      d().columns.map((c) => c.name),
      props.view.kind,
    );
    await Clipboard.writeText(sql);
    toast.success(t("workspace.copied"));
  };
  return (
    <View class="gap-3">
      <View class="gap-1.5">
        <Text class={`${FS.xs} font-semibold text-muted-foreground`}>{t("detail.columns_n", { count: d().columns.length })}</Text>
        <MiniTable
          testID="detail-columns"
          label={t("detail.columns_n", { count: d().columns.length })}
          height={tableHeight(d().columns.length, 12)}
          columns={[
            // 列宽合计 ≈ 弹窗内宽(760 − 左右内边距 48 − 边框),不出横向滚动
            ["name", "Name", 160],
            ["type", "Type", 140],
            ["default", "Default", 110],
            ["comment", "Comment", 140],
            ["notNull", "NotNull", 76],
            ["pk", "Primary", 76],
          ]}
          data={d().columns}
          cell={(c, key) =>
            key === "name" ? (
              <Text class={`${FS.xs} px-2 font-semibold text-foreground line-clamp-1`}>{c.name}</Text>
            ) : key === "type" ? (
              <Text class={`${FS.xs} px-2 text-data line-clamp-1`}>{c.dataType}</Text>
            ) : key === "default" ? (
              <Dim v={c.default} />
            ) : key === "comment" ? (
              <Dim v={c.comment} />
            ) : (
              <Check on={key === "pk" ? c.isPk : c.notNull} />
            )
          }
          onContextMenu={(e) => {
            if (e.row == null) return;
            void NativeMenu.popup([{ label: t("detail.copy_insert"), onSelect: () => void copyInsert() }], { x: e.x, y: e.y });
          }}
        />
      </View>
      <View class="gap-1.5">
        <Text class={`${FS.xs} font-semibold text-muted-foreground`}>{t("detail.indexes_n", { count: d().indexes.length })}</Text>
        <MiniTable
          testID="detail-indexes"
          label={t("detail.indexes_n", { count: d().indexes.length })}
          height={tableHeight(d().indexes.length, 6)}
          columns={[
            ["name", "Name", 272],
            ["columns", "Columns", 272],
            ["primary", "Primary", 79],
            ["unique", "Unique", 79],
          ]}
          data={d().indexes}
          cell={(i, key) =>
            key === "name" || key === "columns" ? (
              <Text class={`${FS.xs} px-2 text-foreground line-clamp-1`}>{key === "name" ? i.name : i.columns}</Text>
            ) : (
              <Check on={key === "primary" ? i.isPrimary : i.isUnique} />
            )
          }
          empty={t("detail.no_indexes")}
        />
      </View>
    </View>
  );
}
