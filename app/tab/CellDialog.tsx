// 单元格全文弹窗:查看完整值,有主键时可修改并暂存。对应 GPUI 版 tab/cell_dialog.rs。
//
// JSON 值(json / jsonb 列,或内容是 JSON 对象 / 数组的文本)默认展开成树,可切到「文本」编辑:
// 编辑时实时校验(json 列不合法时不能暂存),可一键格式化 / 压缩。格式化在原生侧做,
// 键顺序与数字写法不变(JS 的 JSON.parse + stringify 会改写大数和 1.10)。

import { cleanup, Clipboard, computed, Scroll, signal, Text, View } from "nsh";
import { Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { grid } from "../lib/native";
import type { JsonError, JsonKind, JsonPath } from "../lib/types";
import { editorText } from "../ui/editor";
import { FS } from "../ui/text";
import { JsonTree } from "./JsonTree";
import { ViewSwitch } from "./ui";
import { canFilterJson, filterByJson, isJsonDoc, stageEdit, type CellDialogState, type TabModel } from "./model";
import { revealOpenCell } from "./mask";
import { privacy } from "../store/privacy";
import { isJsonType } from "./values";
import { DialogFrame } from "../dialogs/controls";
import { Btn, Icon } from "../ui/kit";

/** 值能解析为 JSON 对象 / 数组时返回 2 空格缩进的格式化文本(键顺序、数字写法不变)。 */
export function tryFormatJson(v: string): string | null {
  return isJsonDoc(v) ? grid.jsonFormat(v, true) : null;
}

/** 解析失败的说明:`第 3 行第 7 列:此处不应出现「x」`。 */
export function jsonErrorText(e: JsonError): string {
  const reason = t(`json.err.${e.kind}`, { found: e.found ?? "" });
  return t("json.error_at", { line: e.line, column: e.column, reason });
}

function CellEditor(props: { m: TabModel; cell: CellDialogState }) {
  const c = props.cell;
  /** 遮罩中的格子:只读显示遮罩值,点「显示原值」后重开成原值才能修改 */
  const masked = !!c.masked;
  const editable = c.editable && !masked;
  /** json 列:内容必须是合法 JSON 才能暂存;JSON 形态的文本只是展开显示,不强制 */
  const jsonCol = !masked && isJsonType(c.dataType);
  const json = jsonCol || (!masked && isJsonDoc(c.value));
  // JSON 值默认以格式化形态展示;基准值同步取格式化文本,避免刚打开就判为已修改
  const initial = tryFormatJson(c.value ?? "") ?? c.value ?? "";
  const draft = signal(initial);
  const base = signal(initial);
  const staged = signal(false);
  const view = signal<"tree" | "text">(json && isJsonDoc(initial) ? "tree" : "text");
  // 编辑器不受控:用户输入只回报到 draft;格式化 JSON 等程序修改经 setValue 推回(见 ui/editor)
  const draftText = editorText(
    () => draft(),
    (v) => {
      draft.set(v);
      staged.set(false);
    },
  );
  const copied = signal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  cleanup(() => timer && clearTimeout(timer));
  const dirty = () => draft() !== base();
  const check = computed(() => (json ? grid.jsonParse(draft()) : null));
  const jsonError = () => {
    const r = check();
    return r && !r.ok ? r.error : null;
  };
  const blocked = () => jsonCol && jsonError() !== null;

  const stage = () => {
    if (!editable || !dirty() || blocked()) return;
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
  const reformat = (pretty: boolean) => {
    if (jsonError()) return;
    const next = grid.jsonFormat(draft(), pretty);
    if (next !== draft()) {
      draft.set(next);
      staged.set(false);
    }
  };
  const onFilter = canFilterJson(props.m, c.column)
    ? (path: JsonPath, kind: JsonKind, value: string) => {
        props.m.cell.set(null);
        filterByJson(props.m, c.column, path, kind, value);
      }
    : undefined;

  return (
    <View class="h-[560px] w-full">
      <View class="flex-row items-center gap-2 pb-2">
        <Text class={`${FS.sm} font-mono font-semibold text-foreground`}>{c.column}</Text>
        <If when={c.dataType}>
          {(ty) => (
            <Text class={`font-mono text-fg-faint ${FS["2xs"]}`}>
              {ty().toUpperCase()}
            </Text>
          )}
        </If>
        <If when={c.value === null}>
          <Text class={`${FS.xs} text-fg-faint`}>(NULL)</Text>
        </If>
        <View class="flex-1" />
        <If when={json}>
          <ViewSwitch
            value={view()}
            onChange={(v) => view.set(v)}
            options={[
              { value: "tree", label: t("json.view_tree"), icon: "list-tree", testID: "cell-view-tree" },
              { value: "text", label: t("json.view_text"), icon: "braces", testID: "cell-view-text" },
            ]}
          />
        </If>
      </View>
      <If when={masked}>
        <View testID="cell-masked" class="mb-2 flex-row items-center gap-2 rounded-(--radius) border border-border bg-panel px-3 py-2">
          <Icon name="eye-off" size={14} color="muted-foreground" />
          <Text class={`${FS.sm} min-w-0 flex-1 text-muted-foreground`}>
            {privacy.demo() ? t("mask.cell_masked_demo") : c.editable ? t("mask.cell_masked_edit") : t("mask.cell_masked")}
          </Text>
          <Btn
            small
            testID="cell-reveal"
            variant="outline"
            icon="eye"
            label={t("mask.reveal")}
            disabled={privacy.demo()}
            tooltip={privacy.demo() ? t("mask.reveal_disabled_demo") : t("mask.reveal_tooltip")}
            onPress={() => revealOpenCell(props.m)}
          />
        </View>
      </If>
      <View class="min-h-0 flex-1 overflow-hidden rounded-(--radius) border border-border">
        <If when={view() === "tree"}>
          <If
            when={jsonError()}
            else={
              <Scroll testID="cell-json-tree" class="min-h-0 flex-1 bg-editor" contentClass="p-2">
                <JsonTree text={draft()} column={c.column} kind={props.m.dbKind} depth={2} onFilter={onFilter} />
              </Scroll>
            }
          >
            {(e) => (
              <View class="flex-1 items-center justify-center gap-2 bg-editor p-6">
                <Icon name="triangle-alert" size={16} color="warning" />
                <Text class={`${FS.sm} text-muted-foreground`}>{jsonErrorText(e())}</Text>
              </View>
            )}
          </If>
        </If>
        {/* 编辑器一直挂着(切到树时隐藏):不受控的编辑器重建会丢撤销历史 */}
        <View class="min-h-0 flex-1" classList={{ hidden: view() !== "text" }}>
          <Editor
            testID="cell-editor"
            class="flex-1 bg-editor"
            ref={draftText.ref}
            defaultValue={draftText.defaultValue}
            onChange={draftText.onChange}
            readOnly={!editable}
            language={json ? "json" : undefined}
            lineNumbers={false}
            folding={false}
            softWrap
            fontSize={13}
            classes={{ selection: "bg-text-selection", caret: "bg-caret", activeLine: "bg-editor-active-line" }}
          />
        </View>
      </View>
      <View class="flex-row items-center justify-between gap-3 pt-3">
        <View class="min-w-0 flex-1 flex-row items-center gap-3">
          <Text class={`${FS.xs} shrink-0 font-mono text-fg-faint`}>{t("cell.chars", { n: [...draft()].length })}</Text>
          <If when={json}>
            <If
              when={jsonError()}
              else={
                <View testID="cell-json-valid" class="flex-row items-center gap-1">
                  <Icon name="circle-check" size={12} color="success" />
                  <Text class={`${FS.xs} text-muted-foreground`}>{t("json.valid")}</Text>
                </View>
              }
            >
              {(e) => (
                <View class="min-w-0 flex-1 flex-row items-center gap-1">
                  <Icon name={jsonCol ? "circle-alert" : "triangle-alert"} size={12} color={jsonCol ? "destructive" : "warning"} />
                  <Text
                    testID="cell-json-error"
                    class={`${FS.xs} min-w-0 flex-1 line-clamp-1`}
                    classList={{ "text-destructive": jsonCol, "text-warning": !jsonCol }}
                  >
                    {jsonErrorText(e())}
                  </Text>
                </View>
              )}
            </If>
          </If>
        </View>
        <View class="shrink-0 flex-row gap-2">
          <If when={json && view() === "text"}>
            <Btn
              small
              testID="cell-json-format"
              variant="ghost"
              label={t("json.format")}
              disabled={jsonError() !== null}
              onPress={() => reformat(true)}
            />
            <Btn
              small
              testID="cell-json-compact"
              variant="ghost"
              label={t("json.compact")}
              disabled={jsonError() !== null}
              onPress={() => reformat(false)}
            />
          </If>
          <Btn
            small
            variant="outline"
            icon={copied() ? "check" : "copy"}
            label={copied() ? t("cell.copied") : t("cell.copy")}
            onPress={() => void copy()}
          />
          <If when={editable}>
            <Btn
              small
              testID="cell-stage"
              variant={dirty() && !blocked() ? "primary" : "outline"}
              disabled={blocked()}
              label={staged() && !dirty() ? t("cell.staged") : t("cell.stage")}
              tooltip={blocked() ? t("json.invalid_cannot_stage") : t("cell.stage_tooltip")}
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
    <DialogFrame
      open={m.cell() !== null}
      onClose={close}
      testID="cell-dialog"
      title={m.cell()?.editable && !m.cell()?.masked ? t("cell.edit_title") : t("cell.view_title")}
      width={880}
    >
      {/* 按格子与遮罩状态重建:「显示原值」后编辑器(不受控)要换成原值 */}
      <Each of={m.cell() ? [m.cell()!] : []} key={(c) => `${c.viewIx}:${c.colIx}:${c.masked ? "m" : "v"}`}>
        {(c) => <CellEditor m={m} cell={c()} />}
      </Each>
    </DialogFrame>
  );
}
