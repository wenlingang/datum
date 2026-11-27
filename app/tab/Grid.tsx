// 结果网格:nsh/base 的 DataTable + Datum 的单元格分级配色、暂存编辑/删除态、
// 自定义表头(列名 + 类型 + 排序指示)与行右键菜单。对应 GPUI 版 tab/grid.rs。
//
// 单元格不存状态:renderCell 只拿到行列号,值、暂存态都在绑定里从模型读(格子会被复用)。

import { Clipboard, effect, Text, untracked, View } from "nsh";
import { DataTable, type DataTableColumn, type DataTableRef } from "nsh/base";
import { Menu as NativeMenu } from "nsh/desktop";
import { t } from "nsh/i18n";
import { grid } from "../lib/native";
import { supportsSql } from "../lib/types";
import { Icon, tip } from "../ui/kit";
import { FS } from "../ui/text";
import { isJsonType } from "./values";
import {
  columnTypes,
  cycleSort,
  dirtyKey,
  isJsonDoc,
  openRowDetail,
  pkIndex,
  toggleDelete,
  viewIndices,
  viewRow,
  type TabModel,
} from "./model";
import { columnMask, copyInsertDisplayed, copyRowDisplayed, copyValueDisplayed, displayValue, isMaskedCell, openCellView, revealCell, toggleColumnMask } from "./mask";
import { privacy } from "../store/privacy";

/**
 * 列宽下限;上限取 COL_MAX 与结果区宽度中大的那个。maxWidth 同时限制 autoFit、伸展与拖动:
 * 太小则文本列少时铺不满结果区,太大则很长的值会适配得非常宽,所以随结果区走,最宽一屏。
 */
const COL_MIN = 64;
const COL_MAX = 600;
/** 适配列宽时在最宽文本外加的余量:格子 / 表头左右 px-3,再留遮罩图标与表头类型行的空 */
const FIT_PAD = 32;
/** 推断数字列时采样的行数。 */
const SAMPLE_ROWS = 120;

type Align = "left" | "right" | "center";

const NUMERIC = /^-?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][-+]?\d+)?$/;

/** 没有声明类型的列(查询结果、表达式列):采样的非空值全是数字就右对齐。按结果缓存。 */
const inferredCache = new WeakMap<object, Align[]>();
function inferredAligns(m: TabModel): Align[] {
  const result = m.result();
  if (!result) return [];
  const hit = inferredCache.get(result);
  if (hit) return hit;
  const out = result.columns.map((_, ci): Align => {
    let seen = 0;
    for (const row of result.rows.slice(0, SAMPLE_ROWS)) {
      const v = row[ci];
      if (v === null || v === undefined) continue;
      if (!NUMERIC.test(v)) return "left";
      seen++;
    }
    return seen > 0 ? "right" : "left";
  });
  inferredCache.set(result, out);
  return out;
}

/** 列对齐:有类型按类型,没有就按取值推断。 */
function columnAlign(m: TabModel, col: number): Align {
  const ty = columnTypes(m)[columnName(m, col)];
  return ty ? (grid.colAlign(ty) as Align) : (inferredAligns(m)[col] ?? "left");
}

/**
 * 列 key:`结果编号:列下标`。每个结果一个编号,新结果的列重新 autoFit(同一结果排序 / 筛选不动列宽)。
 * 行也按这些 key 给成对象(见 ResultGrid 的 rows):新列先于新行到达表格时,旧行里没有新 key,
 * 列就等新行来了再适配,不会按上一个结果同位置的值适配。
 */
const resultIds = new WeakMap<object, number>();
let nextResultId = 0;
function columnKeyPrefix(result: object): string {
  let id = resultIds.get(result);
  if (id === undefined) resultIds.set(result, (id = nextResultId++));
  return `${id}:`;
}

/**
 * 列宽交给 DataTable:`autoFit` 按最宽文本适配一次,`grow` 让文本列分掉结果区剩余的宽度
 * (没有文本列就全部分);内容比结果区宽时横向滚动。
 */
function columnLayout(m: TabModel): DataTableColumn[] {
  const result = m.result();
  if (!result) return [];
  const prefix = columnKeyPrefix(result);
  const aligns = result.columns.map((_, i) => columnAlign(m, i));
  const anyText = aligns.includes("left");
  const maxWidth = Math.max(COL_MAX, Math.floor(m.gridWidth()));
  return result.columns.map((name, i) => ({
    key: `${prefix}${i}`,
    title: name,
    minWidth: COL_MIN,
    maxWidth,
    fitPadding: FIT_PAD,
    autoFit: true,
    grow: !anyText || aligns[i] === "left" ? 1 : 0,
    align: aligns[i] === "right" ? "end" : aligns[i] === "center" ? "center" : "start",
    movable: false,
  }));
}

function columnName(m: TabModel, col: number): string {
  return m.result()?.columns[col] ?? "";
}

function GridCell(props: { m: TabModel; row: number; col: number }) {
  const m = props.m;
  // 展示值:敏感列遮罩(点过「显示原值」的格子除外)
  const value = () => displayValue(m, props.row, props.col);
  const masked = () => isMaskedCell(m, props.row, props.col);
  const type = () => columnTypes(m)[columnName(m, props.col)] ?? null;
  const style = () => grid.classifyCell(value(), type());
  const align = () => columnAlign(m, props.col);
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
      testID={masked() ? `masked:${props.row}:${columnName(m, props.col)}` : undefined}
      class={`self-stretch px-3 line-clamp-1 ${isNull() ? `${FS.xs} font-semibold` : FS.sm}`}
      style={{ lineHeight: 36 }}
      classList={{
        "text-right": align() === "right",
        "text-center": align() === "center",
        "text-cell-strong": !masked() && style().tier === "strong",
        "text-cell": !masked() && style().tier === "normal",
        "text-cell-dim": masked() || style().tier === "dim",
        "text-cell-hash": style().tier === "hash",
        "text-cell-null": style().tier === "null",
        "text-negative": style().tier === "negative",
        // JSON 列压成一行的文本用等宽,结构更好认(双击展开成树)
        "font-mono": masked() || style().mono || isJsonType(type()),
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
  const align = () => columnAlign(m, props.col);
  const sorted = () => {
    const s = m.sort();
    return s && s.col === name() ? s : null;
  };
  const masked = () => columnMask(m, props.col) !== null;
  return (
    <View
      testID={`th:${name()}`}
      onPress={() => cycleSort(m, name())}
      onContextMenu={(e) => showHeaderMenu(m, props.col, e.pagePosition)}
      class="flex-1 self-stretch flex-row items-center gap-1.5 px-3"
    >
      <If when={masked()}>
        <View testID={`th-masked:${name()}`} tooltip={tip(t("mask.column_masked_tip"))}>
          <Icon name="eye-off" size={12} color="fg-faint" />
        </View>
      </If>
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
            <Text class={`font-mono font-semibold text-fg-faint line-clamp-1 ${FS["2xs"]}`}>
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

/** 表头右键:遮罩此列 / 取消遮罩(写进连接的手动覆盖)。 */
function showHeaderMenu(m: TabModel, col: number, at: { x: number; y: number }) {
  const masked = columnMask(m, col) !== null;
  void NativeMenu.popup(
    [
      masked
        ? { label: t("mask.unmask_column"), disabled: privacy.demo(), onSelect: () => toggleColumnMask(m, col) }
        : { label: t("mask.mask_column"), onSelect: () => toggleColumnMask(m, col) },
    ],
    at,
  );
}

function showRowMenu(m: TabModel, row: number, col: number, at: { x: number; y: number }) {
  const editable = pkIndex(m) !== null;
  const deletable = editable && m.table !== null;
  const pi = pkIndex(m);
  const pk = pi === null ? null : (viewRow(m, row)?.[pi] ?? "");
  const deleted = pk !== null && m.deletes().includes(pk);
  const masked = isMaskedCell(m, row, col);
  const json = !masked && isJsonDoc(viewRow(m, row)?.[col] ?? null);
  void NativeMenu.popup(
    [
      { label: t("grid.view_detail"), onSelect: () => openRowDetail(m, row) },
      // JSON 对象 / 数组:弹窗默认就是树形,这里点明入口
      json && { label: t("grid.expand_json"), onSelect: () => openCellView(m, row, col) },
      { label: editable ? t("grid.edit") : t("grid.view"), onSelect: () => openCellView(m, row, col) },
      // 遮罩的格子:显式点「显示原值」才看到(演示模式下不可用)
      masked && { label: t("mask.reveal"), disabled: privacy.demo(), onSelect: () => revealCell(m, row, col) },
      { type: "separator" },
      // 复制的是展示值:遮罩的列复制出来也是遮罩后的
      { label: t("grid.copy_value"), onSelect: () => void copyValueDisplayed(m, row, col, (s) => Clipboard.writeText(s)) },
      { label: t("grid.copy_row"), onSelect: () => void copyRowDisplayed(m, row, (s) => Clipboard.writeText(s)) },
      supportsSql(m.dbKind) && {
        label: t("grid.copy_insert"),
        onSelect: () => void copyInsertDisplayed(m, row, (s) => Clipboard.writeText(s)),
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
  // 单元格 / 行详情弹窗打开时表格交出键盘:DataTable 获得焦点后按快捷键绑定 Esc、方向键、⌘C,
  // 弹窗盖在上面时仍然生效(Esc 先被拿去清空选中,弹窗关不掉;方向键在背后移动选中)
  effect(() => {
    if (m.cell() !== null || m.rowDetail() !== null) untracked(() => table?.blur());
  });
  // 结果区宽度(防抖 120ms):只用来定列宽上限,分配宽度由 DataTable 的 grow 做
  let relayout: ReturnType<typeof setTimeout> | undefined;
  const onLayout = (w: number) => {
    if (Math.abs(w - m.gridWidth()) <= 1) return;
    if (relayout) clearTimeout(relayout);
    if (m.gridWidth() === 0) m.gridWidth.set(w);
    else relayout = setTimeout(() => m.gridWidth.set(w), 120);
  };
  const rows = () => {
    const result = m.result();
    if (!result) return [];
    const prefix = columnKeyPrefix(result);
    return viewIndices(m).map((i) => Object.fromEntries((result.rows[i] ?? []).map((v, ci) => [`${prefix}${ci}`, v])));
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
          if (target.row != null && target.column != null) openCellView(m, target.row, target.column);
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
