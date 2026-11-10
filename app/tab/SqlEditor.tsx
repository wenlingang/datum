// SQL 编辑器:nsh/base 的 Editor + Datum 的 schema 感知补全、关键字自动大写、
// 编辑器内快捷键、右键菜单与划词「发给 AI」浮钮。对应 GPUI 版 tab/editor.rs、selection_chip.rs。
//
// 补全的计算在 datum_core::completion(经 lib/native 的 grid.completions);这里只转成编辑器的补全项。

import { cleanup, effect, keymap, signal, Text, View } from "nsh";
import { Button, Editor, type EditorCompletionItem, type EditorCompletionKind, type EditorCompletionRequest } from "nsh/base";
import { Menu as NativeMenu } from "nsh/desktop";
import { t } from "nsh/i18n";
import { grid } from "../lib/native";
import { supportsSql } from "../lib/types";
import { conns } from "../store/conns";
import { Icon } from "../ui/kit";
import { FS } from "../ui/text";
import { beautify, explain, run, selectionText, sendSelectionToAi, type TabModel } from "./model";
import { primaryRange, rangeOf } from "./textpos";

const BOUNDARY = /[\s,;()]/;

/** 输入边界字符时,把紧邻前面的 SQL 关键字转为大写(长度不变,光标不跳)。 */
function onTextChange(m: TabModel, value: string) {
  const prev = m.sql();
  m.sql.set(value);
  if (!supportsSql(m.dbKind) || value.length !== prev.length + 1 || !m.editor) return;
  let i = 0;
  while (i < prev.length && prev.charCodeAt(i) === value.charCodeAt(i)) i++;
  if (!BOUNDARY.test(value[i] ?? "")) return;
  const r = grid.uppercaseKeywordBefore(value, i + 1);
  if (!r) return;
  m.editor.edit([{ range: rangeOf(value, r.start, r.end), text: r.text }]);
}

async function complete(m: TabModel, req: EditorCompletionRequest) {
  const editor = m.editor;
  if (!editor) return;
  const text = await editor.value();
  const cols = conns.columns(m.connId) ?? null;
  const tables = conns.tables(m.connId) ?? [];
  const result = grid.completions(m.dbKind, text, req.offset, tables, cols, req.trigger === "invoked");
  if (!result) {
    editor.presentCompletions(req.id, []);
    return;
  }
  const range = rangeOf(text, result.start, result.end);
  const items: EditorCompletionItem[] = result.items.map((item, i) => ({
    label: item.label,
    kind: item.kind as EditorCompletionKind,
    detail: item.detail || null,
    insertText: item.insert,
    // 保持计算出的顺序(模板 → 列 → 表 → 关键字)
    sortText: String(i).padStart(5, "0"),
    range,
  }));
  editor.presentCompletions(req.id, items);
}

function showEditorMenu(m: TabModel, at: { x: number; y: number }) {
  const relational = supportsSql(m.dbKind);
  const hasSelection = selectionText(m).trim() !== "";
  void NativeMenu.popup(
    [
      hasSelection && { label: t("tab.menu.send_to_ai"), onSelect: () => sendSelectionToAi(m) },
      { label: t("tab.menu.run"), onSelect: () => run(m) },
      relational && { label: t("tab.menu.explain"), onSelect: () => explain(m) },
      relational && { label: t("tab.menu.format"), onSelect: () => beautify(m) },
      { type: "separator" },
      { label: t("tab.menu.cut"), onSelect: () => m.editor?.run("cut") },
      { label: t("tab.menu.copy"), onSelect: () => m.editor?.run("copy") },
      { label: t("tab.menu.paste"), onSelect: () => m.editor?.run("paste") },
    ],
    at,
  );
}

/** 查询 tab 的编辑器(带划词浮钮)。 */
export function SqlEditor(props: { m: TabModel; autoFocus?: boolean }) {
  const m = props.m;
  /** 划词浮钮的位置(编辑器盒内),无选区时为 null */
  const chip = signal<{ x: number; y: number } | null>(null);
  let chipSeq = 0;

  // 编辑器内的快捷键:压过 Editor 自带的 mod+enter / cmd+e 等绑定。
  // 编辑器聚焦时才绑(比它在聚焦时绑定的更新,故优先)。
  effect(() => {
    if (!m.editorFocused()) return;
    const off = keymap.bind(
      {
        "mod+enter": () => run(m),
        "mod+shift+f": () => beautify(m),
        "mod+e": () => explain(m),
        "mod+shift+a": () => sendSelectionToAi(m),
      },
      { when: "always" },
    );
    cleanup(off);
  });

  const onSelection = async () => {
    const seq = ++chipSeq;
    const text = m.sql();
    const r = primaryRange(text, m.selections());
    if (!r || r[0] === r[1] || !text.slice(r[0], r[1]).trim() || !m.editor) {
      chip.set(null);
      return;
    }
    const end = rangeOf(text, r[1], r[1]);
    const boxes = await m.editor.rangeBounds(end);
    if (seq !== chipSeq) return;
    const b = boxes[0]?.bounds;
    if (!b) {
      chip.set(null);
      return;
    }
    chip.set({ x: Math.max(0, b.origin.x), y: Math.max(0, b.origin.y - 30) });
  };

  const placeholder = () =>
    m.dbKind === "redis"
      ? t("tab.editor_placeholder_redis")
      : t("tab.editor_placeholder_sql");

  return (
    <View class="relative min-h-0 flex-1 bg-editor">
      <Editor
        ref={(r) => {
          m.editor = r;
        }}
        testID="sql-editor"
        class="flex-1 bg-editor"
        value={m.sql()}
        language={m.dbKind === "redis" ? "text" : "sql"}
        placeholder={placeholder()}
        autoFocus={props.autoFocus}
        lineNumbers
        softWrap={false}
        fontSize={13}
        completion
        completionTriggerCharacters={[".", " ", ":"]}
        onChange={({ value }) => onTextChange(m, value)}
        onSelectionChange={({ selections }) => {
          m.selections.set(selections);
          void onSelection();
        }}
        onFocus={() => m.editorFocused.set(true)}
        onBlur={() => m.editorFocused.set(false)}
        onCompletionRequest={(req) => void complete(m, req)}
        onContextMenu={(e) => showEditorMenu(m, { x: e.x, y: e.y })}
        classes={{
          gutter: "bg-editor",
          lineNumber: "text-editor-line-number",
          activeLineNumber: "text-editor-active-line-number",
          activeLine: "bg-editor-active-line",
          selection: "bg-text-selection",
          caret: "bg-caret",
          placeholder: "text-fg-faint",
          indentGuide: "bg-hairline",
        }}
      />
      <If when={chip()}>
        {(p) => (
          <View class="absolute" style={{ left: p().x, top: p().y }}>
            <Button
              testID="send-selection"
              onPress={() => sendSelectionToAi(m)}
              class="h-6 flex-row items-center gap-1 rounded-(--radius) border border-ai bg-background px-2 shadow-sm"
            >
              <Icon name="sparkles" size={12} color="ai" />
              <Text class={`${FS.xs} font-semibold text-ai`}>{t("tab.send_to_ai_chip")}</Text>
            </Button>
          </View>
        )}
      </If>
    </View>
  );
}
