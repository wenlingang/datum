// JSON 值展开成树:单元格弹窗与行详情里用。容器可折叠;点一行选中,行尾给出「复制路径 / 复制值」,
// 能筛选时(PG)标量字段再给「按此值筛选」。右键同样三项。
//
// 解析在原生侧(datum_core::json):对象保留原文键顺序、数字保留原文写法(JS 的 JSON.parse 会改写大数与 1.10)。

import { Clipboard, computed, effect, signal, Text, untracked, View } from "nsh";
import { Menu as NativeMenu } from "nsh/desktop";
import { t } from "nsh/i18n";
import { grid } from "../lib/native";
import type { DbKind, JsonKind, JsonNode, JsonPath } from "../lib/types";
import { toast } from "../store/toast";
import { Icon, IconButton } from "../ui/kit";
import { FS, MONO } from "../ui/text";

/** 每个容器最多列出的子项、整棵树最多渲染的行(大文档展开不至于卡住界面;完整内容看「文本」)。 */
const CHILD_MAX = 300;
const ROW_MAX = 1500;
const ROW_H = 22;

interface Row {
  id: string;
  path: JsonPath;
  depth: number;
  /** 对象键、数组下标;根为 null */
  key: string | number | null;
  node: JsonNode;
}

/** 截断行:某个容器的子项超出上限,或整棵树的行数到顶。 */
interface More {
  id: string;
  depth: number;
  hidden: number;
}

const isContainer = (n: JsonNode): n is Extract<JsonNode, { kind: "object" | "array" }> =>
  n.kind === "object" || n.kind === "array";

const childCount = (n: JsonNode) => (n.kind === "object" ? n.entries.length : n.kind === "array" ? n.items.length : 0);

/** testID 用的路径写法:`a.0.b`,根为 `$`。 */
export const pathId = (path: JsonPath) => (path.length === 0 ? "$" : path.join("."));

function flatten(root: JsonNode, open: Set<string>): (Row | More)[] {
  const out: (Row | More)[] = [];
  const walk = (node: JsonNode, path: JsonPath, depth: number, key: string | number | null) => {
    if (out.length >= ROW_MAX) return;
    const id = JSON.stringify(path);
    out.push({ id, path, depth, key, node });
    if (!isContainer(node) || !open.has(id)) return;
    const kids: [string | number, JsonNode][] =
      node.kind === "object" ? node.entries.map((e) => [e.key, e.node]) : node.items.map((n, i) => [i, n]);
    for (const [k, child] of kids.slice(0, CHILD_MAX)) {
      if (out.length >= ROW_MAX) break;
      walk(child, [...path, k], depth + 1, k);
    }
    const shown = Math.min(kids.length, CHILD_MAX);
    if (kids.length > shown) out.push({ id: `${id}+`, depth: depth + 1, hidden: kids.length - shown });
  };
  walk(root, [], 0, null);
  return out;
}

/** 默认展开到 `depth` 层(根为第 0 层)。 */
function initialOpen(root: JsonNode, depth: number): Set<string> {
  const open = new Set<string>();
  const walk = (node: JsonNode, path: JsonPath, d: number) => {
    if (!isContainer(node) || d >= depth) return;
    open.add(JSON.stringify(path));
    if (node.kind === "object") node.entries.slice(0, CHILD_MAX).forEach((e) => walk(e.node, [...path, e.key], d + 1));
    else node.items.slice(0, CHILD_MAX).forEach((n, i) => walk(n, [...path, i], d + 1));
  };
  walk(root, [], 0);
  return open;
}

/** 标量的显示文本(字符串带引号、换行压成一行)与颜色。 */
function scalar(node: JsonNode): { text: string; color: string } {
  switch (node.kind) {
    case "string":
      return { text: JSON.stringify(node.text.length > 300 ? `${node.text.slice(0, 300)}…` : node.text), color: "text-syntax-string" };
    case "number":
      return { text: node.text, color: "text-syntax-number" };
    case "bool":
      return { text: node.text, color: "text-syntax-keyword" };
    case "null":
      return { text: "null", color: "text-cell-null italic" };
    default:
      return { text: "", color: "" };
  }
}

export interface JsonTreeProps {
  /** JSON 文本(调用方已确认能解析;解析不了时什么都不显示) */
  text: string;
  /** 所在列(复制路径时拼成 `"col"->'a'`) */
  column: string;
  kind: DbKind;
  /** 默认展开到第几层,根为 0;缺省 2 */
  depth?: number;
  testID?: string;
  /** 「按此值筛选」:只在能筛选的场合传(标量字段才有) */
  onFilter?: (path: JsonPath, kind: JsonKind, value: string) => void;
}

export function JsonTree(props: JsonTreeProps) {
  const parsed = computed(() => grid.jsonParse(props.text));
  const root = () => {
    const p = parsed();
    return p.ok ? p.node : null;
  };
  const seed = () => {
    const r = root();
    return r ? initialOpen(r, props.depth ?? 2) : new Set<string>();
  };
  const open = signal<Set<string>>(untracked(seed));
  // 文本换了(弹窗里改完切回树):按默认层数重新展开
  let first = true;
  effect(() => {
    const next = seed();
    if (first) first = false;
    else open.set(next);
  });
  const selected = signal<string | null>(null);
  const rows = () => {
    const r = root();
    return r ? flatten(r, open()) : [];
  };

  const toggle = (id: string) => {
    const next = new Set(open());
    if (next.has(id)) next.delete(id);
    else next.add(id);
    open.set(next);
  };
  const copyPath = async (path: JsonPath) => {
    await Clipboard.writeText(grid.jsonPathText(props.kind, props.column, path));
    toast.success(t("json.copied_path"));
  };
  const copyValue = async (path: JsonPath) => {
    const v = grid.jsonValueAt(props.text, path);
    if (v === null) return;
    await Clipboard.writeText(v);
    toast.success(t("json.copied_value"));
  };
  const filterable = (r: Row) => !!props.onFilter && !isContainer(r.node) && r.path.length > 0;
  const filter = (r: Row) => {
    if (r.node.kind === "object" || r.node.kind === "array") return;
    props.onFilter?.(r.path, r.node.kind, r.node.text);
  };
  const menu = (r: Row, at: { x: number; y: number }) => {
    selected.set(r.id);
    void NativeMenu.popup(
      [
        { label: t("json.copy_path"), onSelect: () => void copyPath(r.path) },
        { label: t("json.copy_value"), onSelect: () => void copyValue(r.path) },
        filterable(r) && { label: t("json.filter_by_value"), onSelect: () => filter(r) },
      ],
      at,
    );
  };

  return (
    <View testID={props.testID} class="gap-px">
      <Each of={rows()} key={(r) => r.id}>
        {(item) => {
          // 截断行的 id 以 + 结尾,同一个 key 不会在两种行之间切换
          const first = item();
          if ("hidden" in first) {
            const more = () => item() as More;
            return (
              <Text class={`${FS.xs} text-fg-faint`} style={{ paddingLeft: 20 + more().depth * 16, height: ROW_H, lineHeight: ROW_H }}>
                {t("json.more_items", { n: more().hidden })}
              </Text>
            );
          }
          const r = () => item() as Row;
          const isSel = () => selected() === r().id;
          const container = () => isContainer(r().node);
          const s = () => scalar(r().node);
          return (
            <View
              testID={`json-node:${pathId(r().path)}`}
              class="flex-row items-center gap-1 rounded-(--radius) pr-1"
              classList={{ "bg-list-active": isSel(), "hover:bg-list-hover": !isSel() }}
              style={{ paddingLeft: 4 + r().depth * 16, minHeight: ROW_H }}
              onPress={() => {
                selected.set(r().id);
                if (container()) toggle(r().id);
              }}
              onContextMenu={(e) => menu(r(), e.pagePosition)}
            >
              <View class="w-[14px] shrink-0 items-center">
                <If when={container()}>
                  <Icon name={open().has(r().id) ? "chevron-down" : "chevron-right"} size={12} />
                </If>
              </View>
              <If when={r().key !== null}>
                <Text
                  class={`${FS.xs} ${MONO} shrink-0`}
                  classList={{ "text-fg-soft": typeof r().key === "string", "text-fg-faint": typeof r().key === "number" }}
                >
                  {typeof r().key === "number" ? `[${r().key}]` : String(r().key)}
                </Text>
                <Text class={`${FS.xs} ${MONO} shrink-0 text-fg-faint`}>:</Text>
              </If>
              <If
                when={container()}
                else={<Text class={`${FS.xs} ${MONO} min-w-0 flex-1 line-clamp-1 select-text ${s().color}`}>{s().text}</Text>}
              >
                <Text class={`${FS.xs} ${MONO} min-w-0 flex-1 text-fg-faint line-clamp-1`}>
                  {r().node.kind === "object"
                    ? `{…} ${t("json.n_keys", { n: childCount(r().node) })}`
                    : `[…] ${t("json.n_items", { n: childCount(r().node) })}`}
                </Text>
              </If>
              <If when={isSel()}>
                <View class="shrink-0 flex-row items-center">
                  <IconButton testID="json-copy-path" icon="link-2" size={12} class="p-1" tooltip={t("json.copy_path")} onPress={() => void copyPath(r().path)} />
                  <IconButton testID="json-copy-value" icon="copy" size={12} class="p-1" tooltip={t("json.copy_value")} onPress={() => void copyValue(r().path)} />
                  <If when={filterable(r())}>
                    <IconButton testID="json-filter" icon="funnel" size={12} class="p-1" tooltip={t("json.filter_by_value")} onPress={() => filter(r())} />
                  </If>
                </View>
              </If>
            </View>
          );
        }}
      </Each>
    </View>
  );
}
