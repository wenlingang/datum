// 结果网格:nsh/base 的 DataTable + Datum 的单元格分级配色、暂存编辑/删除态、
// 自定义表头(列名 + 类型 + 排序指示)与行右键菜单。对应 GPUI 版 tab/grid.rs。
//
// 单元格不存状态:renderCell 只拿到行列号,值、暂存态都在绑定里从模型读(格子会被复用)。

import { Clipboard, Text, View } from "nsh";
import { DataTable, type DataTableColumn, type DataTableRef } from "nsh/base";
import { Menu as NativeMenu } from "nsh/desktop";
import { t } from "nsh/i18n";
import { grid } from "../lib/native";
import { supportsSql } from "../lib/types";
import { Icon } from "../ui/kit";
import { FS } from "../ui/text";
import { isJsonType } from "./values";
import {
  columnTypes,
  copyInsertRow,
  cycleSort,
  dirtyKey,
  isJsonDoc,
  openCell,
  openRowDetail,
  pkIndex,
  toggleDelete,
  viewIndices,
  viewRow,
  type TabModel,
} from "./model";

/** 自适应列宽的边界与采样。 */
const AUTO_MIN = 64;
const AUTO_MAX = 420;
const SAMPLE_ROWS = 120;
const CELL_PAD = 32;
const SORT_ICON_SPACE = 20;
const DEFAULT_W = 170;

/** 按字符估算文本宽度:CJK 等宽字符按两格计。 */
function textWidth(text: string, pxPerChar: number): number {
  let w = 0;
  let n = 0;
  for (const ch of text) {
    if (n++ >= 80) break;
    w += (ch.codePointAt(0) ?? 0) < 128 ? 1 : 1.9;
  }
  return w * pxPerChar;
}

function columnLayout(m: TabModel): DataTableColumn[] {
  const result = m.result();
  if (!result) return [];
  const types = columnTypes(m);
  const view = viewIndices(m);
  const autos = result.columns.map((col, ci) => {
    let w = Math.max(textWidth(col, 7.6) + SORT_ICON_SPACE, types[col] ? textWidth(types[col], 6.6) : 0);
    for (const ri of view.slice(0, SAMPLE_ROWS)) {
      const v = result.rows[ri]?.[ci];
      w = Math.max(w, textWidth(v ?? "NULL", 7.4));
    }
    return Math.min(AUTO_MAX, Math.max(AUTO_MIN, w + CELL_PAD));
  });
  const aligns = result.columns.map((c) => grid.colAlign(types[c] ?? null));
  const widths = grid.columnWidths(
    autos,
    m.gridWidth(),
    DEFAULT_W,
    aligns.map((a) => a === "left"),
  );
  return result.columns.map((name, i) => ({
    key: String(i),
    title: name,
    width: widths[i],
    minWidth: 48,
    maxWidth: 2400,
    align: aligns[i] === "right" ? "end" : aligns[i] === "center" ? "center" : "start",
    movable: false,
  }));
}

function columnName(m: TabModel, col: number): string {
  return m.result()?.columns[col] ?? "";
}

function GridCell(props: { m: TabModel; row: number; col: number }) {
  const m = props.m;
  const value = () => viewRow(m, props.row)?.[props.col] ?? null;
  const type = () => columnTypes(m)[columnName(m, props.col)] ?? null;
  const style = () => grid.classifyCell(value(), type());
  const align = () => grid.colAlign(type());
  const pk = () => {
    const pi = pkIndex(m);
    return pi === null ? null : (viewRow(m, props.row)?.[pi] ?? "");
  };
  const deleted = () => {
    const p = pk();
    return p !== null && m.deletes().includes(p);
  };
  const dirty = () => {
    const p = pk();
    return !deleted() && p !== null && !!m.dirty()[dirtyKey(p, columnName(m, props.col))];
  };
  const isNull = () => value() === null;
  const text = () => {
    const v = value();
    if (v === null) return "NULL";
    // 多行值在网格里压成一行(双击看全文)
    return (m.result()?.columns.length ?? 0) === 1 ? v : v.replace(/[\r\n]/g, " ");
  };
  // 根元素直接是 Text(DataTable 的自定义格子里,View 包一层在真实平台上不显示文字);
  // 行高撑满格子,暂存态的底色才铺满整格
  return (
    <Text
      class={`self-stretch px-3 line-clamp-1 ${isNull() ? `${FS.xs} font-semibold` : FS.sm}`}
      style={{ lineHeight: 36 }}
      classList={{
        "text-right": align() === "right",
        "text-center": align() === "center",
        "text-cell-strong": style().tier === "strong",
        "text-cell": style().tier === "normal",
        "text-cell-dim": style().tier === "dim",
        "text-cell-hash": style().tier === "hash",
        "text-cell-null": style().tier === "null",
        "text-negative": style().tier === "negative",
        // JSON 列压成一行的文本用等宽,结构更好认(双击展开成树)
        "font-mono": style().mono || isJsonType(type()),
        "bg-deleted opacity-55 line-through": deleted(),
        "bg-dirty border border-primary": dirty(),
      }}
    >
      {text()}
    </Text>
  );
}

function HeaderCell(props: { m: TabModel; col: number }) {
  const m = props.m;
  const name = () => columnName(m, props.col);
  const type = () => columnTypes(m)[name()] ?? null;
  const align = () => grid.colAlign(type());
  const sorted = () => {
    const s = m.sort();
    return s && s.col === name() ? s : null;
  };
  return (
    <View
      testID={`th:${name()}`}
      onPress={() => cycleSort(m, name())}
      class="flex-1 self-stretch flex-row items-center gap-1.5 px-3"
    >
      <View
        class="min-w-0 flex-1 gap-0.5"
        classList={{
          "items-end": align() === "right",
          "items-center": align() === "center",
          "items-start": align() === "left",
        }}
      >
        <Text class={`${FS.sm} font-bold text-foreground line-clamp-1`}>{name()}</Text>
        <If when={type()}>
          {(ty) => (
            <Text class="font-mono font-semibold text-fg-faint line-clamp-1" style={{ fontSize: 9.5 }}>
              {ty().toUpperCase()}
            </Text>
          )}
        </If>
      </View>
      <If when={sorted()}>
        {(s) => <Icon name={s().desc ? "chevron-down" : "chevron-up"} size={13} color="data" />}
      </If>
    </View>
  );
}

function showRowMenu(m: TabModel, row: number, col: number, at: { x: number; y: number }) {
  const editable = pkIndex(m) !== null;
  const deletable = editable && m.table !== null;
  const pi = pkIndex(m);
  const pk = pi === null ? null : (viewRow(m, row)?.[pi] ?? "");
  const deleted = pk !== null && m.deletes().includes(pk);
  const json = isJsonDoc(viewRow(m, row)?.[col] ?? null);
  void NativeMenu.popup(
    [
      { label: t("grid.view_detail"), onSelect: () => openRowDetail(m, row) },
      // JSON 对象 / 数组:弹窗默认就是树形,这里点明入口
      json && { label: t("grid.expand_json"), onSelect: () => openCell(m, row, col) },
      { label: editable ? t("grid.edit") : t("grid.view"), onSelect: () => openCell(m, row, col) },
      supportsSql(m.dbKind) && {
        label: t("grid.copy_insert"),
        onSelect: () => void copyInsertRow(m, row, (s) => Clipboard.writeText(s)),
      },
      deletable && { type: "separator" },
      deletable && {
        label: deleted ? t("grid.undo_delete") : t("grid.delete_row"),
        onSelect: () => toggleDelete(m, row),
      },
    ],
    at,
  );
}

/** 结果网格。 */
export function ResultGrid(props: { m: TabModel }) {
  const m = props.m;
  let table: DataTableRef | undefined;
  let refit: ReturnType<typeof setTimeout> | undefined;
  const rows = () => {
    const result = m.result();
    return result ? viewIndices(m).map((i) => result.rows[i]) : [];
  };
  // 结果区宽度变化后(防抖 120ms)按新宽度重排列宽,让列铺满结果区
  const onLayout = (w: number) => {
    if (Math.abs(w - m.gridWidth()) <= 1) return;
    if (refit) clearTimeout(refit);
    if (m.gridWidth() === 0) m.gridWidth.set(w);
    else refit = setTimeout(() => m.gridWidth.set(w), 120);
  };
  return (
    <View class="relative min-h-0 flex-1" onLayout={(e) => onLayout(e.bounds.size.width)}>
      <DataTable
        ref={table}
        testID="result-grid"
        class="flex-1"
        columns={columnLayout(m)}
        data={rows()}
        rowHeight={36}
        headerHeight={44}
        stripe
        rowSelectable
        columnSelectable={false}
        cellSelectable={false}
        movableColumns={false}
        selection={m.selectedRow() === null ? null : { row: m.selectedRow() }}
        onSelectionChange={({ selection }) => m.selectedRow.set(selection?.row ?? null)}
        onDoublePress={({ target }) => {
          if (target.row != null && target.column != null) openCell(m, target.row, target.column);
        }}
        onContextMenu={(e) => {
          if (e.row == null) return;
          showRowMenu(m, e.row, e.column ?? 0, { x: e.x, y: e.y });
          table?.clearMenuTarget();
        }}
        renderCell={(c) => <GridCell m={m} row={c.row} col={c.column} />}
        renderHeaderCell={(h) => <HeaderCell m={m} col={h.column} />}
        empty={<Text class={`${FS.sm} text-fg-faint`}>{t("grid.no_rows")}</Text>}
        classes={{
          root: "bg-background",
          header: "border-b border-border bg-background",
          headerCell: "px-0",
          headerContent: "self-stretch",
          resizeHandle: "bg-border group-hover:bg-primary group-active:bg-primary",
          row: "border-b border-border",
          rowOdd: "bg-list-even",
          rowHover: "bg-list-hover",
          rowSelected: "bg-list-active",
          rowActive: "bg-list-hover",
          cell: "px-0",
          empty: "items-center justify-center",
        }}
      />
    </View>
  );
}
