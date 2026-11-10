// 单元格全文弹窗:查看完整值(JSON 自动格式化),有主键时可修改并暂存。
// 对应 GPUI 版 tab/cell_dialog.rs。

import { cleanup, Clipboard, signal, Text, View } from "nsh";
import { Dialog, Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { Btn, IconButton } from "../ui/kit";
import { FS } from "../ui/text";
import { stageEdit, type CellDialogState, type TabModel } from "./model";

/** 值能解析为 JSON 对象/数组时返回 2 空格缩进的格式化文本。 */
export function tryFormatJson(v: string): string | null {
  const s = v.trim();
  if (s.length < 2 || !(s.startsWith("{") || s.startsWith("["))) return null;
  try {
    const parsed: unknown = JSON.parse(s);
    return parsed !== null && typeof parsed === "object" ? JSON.stringify(parsed, null, 2) : null;
  } catch {
    return null;
  }
}

function CellEditor(props: { m: TabModel; cell: CellDialogState }) {
  const c = props.cell;
  // 能解析成 JSON 的值默认以格式化形态展示;基准值同步取格式化文本,避免刚打开就判为已修改
  const initial = tryFormatJson(c.value ?? "") ?? c.value ?? "";
  const draft = signal(initial);
  const base = signal(initial);
  const staged = signal(false);
  const copied = signal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  cleanup(() => timer && clearTimeout(timer));
  const dirty = () => draft() !== base();

  const stage = () => {
    if (!c.editable || !dirty()) return;
    stageEdit(props.m, c.viewIx, c.colIx, draft());
    base.set(draft());
    staged.set(true);
  };
  const copy = async () => {
    await Clipboard.writeText(draft());
    copied.set(true);
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => copied.set(false), 1600);
  };

  return (
    <View class="h-[560px] w-full">
      <View class="flex-row items-center gap-2 pb-2">
        <Text class={`${FS.sm} font-mono font-semibold text-foreground`}>{c.column}</Text>
        <If when={c.dataType}>
          {(ty) => (
            <Text class="font-mono text-fg-faint" style={{ fontSize: 9.5 }}>
              {ty().toUpperCase()}
            </Text>
          )}
        </If>
        <If when={c.value === null}>
          <Text class={`${FS.xs} text-fg-faint`}>(NULL)</Text>
        </If>
      </View>
      <View class="min-h-0 flex-1 overflow-hidden rounded-(--radius) border border-border">
        <Editor
          testID="cell-editor"
          class="flex-1 bg-editor"
          value={draft()}
          onChange={({ value }) => {
            draft.set(value);
            staged.set(false);
          }}
          readOnly={!c.editable}
          lineNumbers={false}
          softWrap
          fontSize={13}
          classes={{ selection: "bg-text-selection", caret: "bg-caret", activeLine: "bg-editor-active-line" }}
        />
      </View>
      <View class="flex-row items-center justify-between pt-3">
        <Text class={`${FS.xs} font-mono text-fg-faint`}>{t("cell.chars", { n: [...draft()].length })}</Text>
        <View class="flex-row gap-2">
          <Btn
            small
            variant="outline"
            icon={copied() ? "check" : "copy"}
            label={copied() ? t("cell.copied") : t("cell.copy")}
            onPress={() => void copy()}
          />
          <If when={c.editable}>
            <Btn
              small
              testID="cell-stage"
              variant={dirty() ? "primary" : "outline"}
              label={staged() && !dirty() ? t("cell.staged") : t("cell.stage")}
              tooltip={t("cell.stage_tooltip")}
              onPress={stage}
            />
          </If>
        </View>
      </View>
    </View>
  );
}

/** 单元格弹窗宿主:模型的 `cell` 非空时显示。 */
export function CellDialog(props: { m: TabModel }) {
  const m = props.m;
  const close = () => m.cell.set(null);
  return (
    <Dialog
      open={m.cell() !== null}
      onOpenChange={(e) => !e.open && close()}
      closeOnEscape
      closeOnBackdropPress
      animation="fade"
      backdropClass="bg-overlay"
      class="m-auto w-[880px] max-w-[92%] rounded-(--radius-lg) border border-border bg-popover p-5 shadow-lg"
    >
      <If when={m.cell()}>
        {(c) => (
          <View class="gap-3">
            <View class="flex-row items-center justify-between">
              <Text class={`${FS.base} font-bold text-popover-foreground`}>
                {c().editable ? t("cell.edit_title") : t("cell.view_title")}
              </Text>
              <IconButton icon="x" size={14} onPress={close} />
            </View>
            <CellEditor m={m} cell={c()} />
          </View>
        )}
      </If>
    </Dialog>
  );
}
