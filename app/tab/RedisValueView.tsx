// Redis 值的格式识别与只读展示:String 值的查看区、集合元素 / Stream 字段的「查看」弹窗共用。
//
// 解码在 core(rds::decode):自动识别 JSON、MessagePack、gzip / zlib(解压后再识别内层)、Java 序列化、
// 无 schema 的 Protobuf,认不出时给文本或十六进制;手动选的格式解不出来同样回退,不报错。
// 非 UTF-8 的值经 KeyValue 的 `raw` 拿到原始字节(base64),界面上显示的替换文本识别不了二进制格式。
// 手动选的格式按「连接 + key 前缀」记住(store/redisFormats)。

import { cleanup, effect, Scroll, signal, Text, untracked, View, type JSX } from "nsh";
import { Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { DialogFrame, Dropdown } from "../dialogs/controls";
import { db } from "../lib/native";
import type { RedisDecoded, RedisRawCell, RedisViewFormat } from "../lib/types";
import { setViewFormat, VIEW_FORMATS, viewFormat } from "../store/redisFormats";
import { editorText } from "../ui/editor";
import { Btn, Icon } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { JsonTree } from "./JsonTree";

/** 要解码的值:显示文本 + 非 UTF-8 时的原始字节。 */
export interface ValueSource {
  text: string;
  b64: string | null;
}

/** 在 raw 里找单元格的原始字节。 */
export function rawOf(raw: RedisRawCell[] | undefined, row: number, col: number): string | null {
  return raw?.find((c) => c.row === row && c.col === col)?.b64 ?? null;
}

/** 格式的显示名(专有名词不翻译)。 */
export function formatName(f: RedisViewFormat): string {
  switch (f) {
    case "auto":
      return t("rkey.fmt_auto");
    case "text":
      return t("rkey.fmt_text");
    case "hex":
      return t("rkey.fmt_hex");
    case "json":
      return "JSON";
    case "msgpack":
      return "MessagePack";
    case "gzip":
      return "gzip";
    case "zlib":
      return "zlib";
    case "java":
      return t("rkey.fmt_java");
    case "protobuf":
      return "Protobuf";
  }
}

/**
 * 按当前格式解码,源或格式变了就重新解码。返回的信号在第一次结果回来之前为 null。
 * 先后发出的请求可能乱序回来:只认最后一次。
 */
export function createDecoded(source: () => ValueSource, format: () => RedisViewFormat) {
  const out = signal<RedisDecoded | null>(null);
  let seq = 0;
  let alive = true;
  cleanup(() => (alive = false));
  effect(() => {
    const s = source();
    const f = format();
    const my = ++seq;
    untracked(() => {
      void db
        .redisDecode(s.text, s.b64, f)
        .then((d) => {
          if (alive && my === seq) out.set(d);
        })
        .catch(() => {
          // 只有参数不对(base64 坏了)才会走到这里:按文本显示
          if (alive && my === seq) out.set({ format: "text", layers: [], text: s.text, tree: null, fallback: true, notes: [], size: s.text.length });
        });
    });
  });
  return out;
}

/** 格式下拉:选中即记住(按连接 + key 前缀)。 */
export function FormatPicker(props: { connId: string; keyName: string }) {
  const value = () => viewFormat(props.connId, props.keyName);
  return (
    <Dropdown
      testID="rkey-format"
      small
      items={VIEW_FORMATS.map((f) => ({ value: f, title: formatName(f) }))}
      value={value()}
      label={formatName(value())}
      accessibilityLabel={t("rkey.view_as")}
      tooltip={t("rkey.view_as_tip")}
      onChange={(v) => setViewFormat(props.connId, props.keyName, v as RedisViewFormat)}
      style={{ width: 150 }}
    />
  );
}

/** 识别结果的说明:`MessagePack · 解压自 gzip`、回退提示、附注。 */
export function DecodeInfo(props: { decoded: RedisDecoded | null; requested: RedisViewFormat }) {
  const d = () => props.decoded;
  const label = () => {
    const x = d();
    if (!x) return "";
    const layers = x.layers.length ? ` · ${t("rkey.decompressed_from", { layers: x.layers.map(formatName).join(" → ") })}` : "";
    return `${formatName(x.format)}${layers}`;
  };
  const notes = () => (d()?.notes ?? []).map((n) => t(`rkey.note_${n}`));
  return (
    <View class="min-w-0 flex-1 flex-row items-center gap-2">
      <If when={d()}>
        <View testID="rkey-format-detected" class="shrink-0">
          <Text class={`${FS.xs} rounded-full bg-raised px-2 text-data`}>{label()}</Text>
        </View>
      </If>
      <If when={d()?.fallback}>
        <View class="min-w-0 shrink flex-row items-center gap-1">
          <Icon name="circle-alert" size={12} color="warning" />
          <Text testID="rkey-format-fallback" class={`${FS.xs} min-w-0 text-warning line-clamp-1`}>
            {t("rkey.format_fallback", { format: formatName(props.requested), shown: formatName(d()?.format ?? "text") })}
          </Text>
        </View>
      </If>
      <If when={notes().length > 0}>
        <Text class={`${FS.xs} min-w-0 shrink text-muted-foreground line-clamp-1`}>{notes().join(" · ")}</Text>
      </If>
    </View>
  );
}

/** 树 / 文本切换(只在有树时显示)。 */
export function ViewToggle(props: { mode: "tree" | "text"; onChange: (m: "tree" | "text") => void }) {
  return (
    <View class="flex-row items-center gap-1">
      <Btn
        small
        testID="rkey-view-tree"
        icon="list-tree"
        variant={props.mode === "tree" ? "secondary" : "ghost"}
        label={t("json.view_tree")}
        onPress={() => props.onChange("tree")}
      />
      <Btn
        small
        testID="rkey-view-text"
        icon="braces"
        variant={props.mode === "text" ? "secondary" : "ghost"}
        label={t("json.view_text")}
        onPress={() => props.onChange("text")}
      />
    </View>
  );
}

/** 只读文本(格式化的 JSON、十六进制转储、替换过的文本)。 */
function ReadOnlyText(props: { text: string; testID: string }) {
  const text = editorText(
    () => props.text,
    () => {},
  );
  return (
    <Editor
      testID={props.testID}
      class="flex-1 bg-editor"
      ref={text.ref}
      defaultValue={text.defaultValue}
      readOnly
      lineNumbers={false}
      folding={false}
      softWrap
      fontSize={13}
      classes={{ selection: "bg-text-selection", caret: "bg-caret", activeLine: "bg-editor-active-line" }}
    />
  );
}

/** 解码结果的只读展示:有树时按 `mode` 显示树或文本。 */
export function DecodedBody(props: { decoded: RedisDecoded; mode: "tree" | "text" }) {
  return (
    <If
      when={props.decoded.tree !== null && props.mode === "tree" ? props.decoded.tree : null}
      else={<ReadOnlyText testID="rkey-decoded-text" text={props.decoded.text} />}
    >
      {(tree) => (
        <Scroll testID="rkey-decoded-tree" class="min-h-0 flex-1 bg-editor" contentClass="p-2">
          <JsonTree text={tree()} column="value" kind="redis" depth={2} />
        </Scroll>
      )}
    </If>
  );
}

/** 一个值的查看区:格式下拉 + 识别说明 + 树 / 文本。元素查看弹窗用(只读)。 */
function ValueInspector(props: { connId: string; keyName: string; source: ValueSource; extra?: JSX.Element }) {
  const format = () => viewFormat(props.connId, props.keyName);
  const decoded = createDecoded(() => props.source, format);
  const mode = signal<"tree" | "text">("tree");
  return (
    <View class="min-h-0 flex-1 gap-2">
      <View class="flex-row items-center gap-2">
        {props.extra}
        <FormatPicker connId={props.connId} keyName={props.keyName} />
        <DecodeInfo decoded={decoded()} requested={format()} />
        <If when={decoded()?.tree != null}>
          <ViewToggle mode={mode()} onChange={(m) => mode.set(m)} />
        </If>
      </View>
      <View class="min-h-0 flex-1 overflow-hidden rounded-(--radius) border border-border">
        <If when={decoded()} else={<View class="flex-1 bg-editor" />}>
          {(d) => <DecodedBody decoded={d()} mode={mode()} />}
        </If>
      </View>
      <Text class={`${FS.xs} ${MONO} text-fg-faint`}>{t("rkey.bytes", { n: (decoded()?.size ?? 0).toLocaleString() })}</Text>
    </View>
  );
}

/** 元素查看弹窗的内容:一个值,或 Stream 条目的若干字段(下拉切换字段)。 */
export interface ElementView {
  title: string;
  /** 字段名 + 值;集合元素只有一项(名称为空) */
  items: { name: string; source: ValueSource }[];
}

export function ElementViewDialog(props: { connId: string; keyName: string; view: ElementView | null; onClose: () => void }) {
  return (
    <DialogFrame testID="rkey-view" open={props.view !== null} onClose={props.onClose} title={props.view?.title ?? ""} width={820}>
      {/* 每次打开都重建,字段选择回到第一个 */}
      <Each of={props.view ? [props.view] : []}>
        {(v) => {
          const pick = signal(0);
          const items = () => v().items;
          const cur = () => items()[Math.min(pick(), items().length - 1)];
          return (
            <View class="h-[480px]">
              <If when={cur()}>
                {(c) => (
                  <Each of={[pick()]}>
                    {() => (
                      <ValueInspector
                        connId={props.connId}
                        keyName={props.keyName}
                        source={c().source}
                        extra={
                          items().length > 1 ? (
                            <Dropdown
                              testID="rkey-view-field"
                              small
                              items={items().map((it, i) => ({ value: String(i), title: it.name }))}
                              value={String(pick())}
                              label={c().name}
                              accessibilityLabel={t("rkey.view_field")}
                              onChange={(i) => pick.set(Number(i))}
                              style={{ width: 160 }}
                            />
                          ) : undefined
                        }
                      />
                    )}
                  </Each>
                )}
              </If>
            </View>
          );
        }}
      </Each>
    </DialogFrame>
  );
}
