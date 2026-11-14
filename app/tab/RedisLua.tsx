// Redis Lua 脚本标签页:编辑脚本(Lua 语法高亮)、按表单填 KEYS / ARGV、执行(EVAL)、查看结果、SCRIPT KILL。
//
// 做成单独的标签页而不是查询标签页里的一个模式:查询标签页的模型围绕「逐条命令 + 表格结果 + 历史」,
// 脚本要的是另一种编辑器语言、参数表单、树形结果和运行中的终止按钮,硬塞进去两边都别扭;
// 每个标签页各自记住脚本与上次填的 KEYS / ARGV(状态在 luaModel,换 tab、重启后都还在)。
//
// 安全:
// - 执行前静态检查脚本(core 的 rds::lua::analyze):prod 连接一律确认(要勾选);脚本里有写命令、
//   命令名无法静态判断(按可能写入)时非 prod 也确认;FLUSHALL / FLUSHDB / SWAPDB / KEYS 特别标出并要勾选。
// - 集群模式下 KEYS 不在同一槽会报 CROSSSLOT:执行前本地算 CRC16 槽,不同槽时在确认框里警告。
// - 执行在专用连接上(core);超过 busy 阈值(默认 5 秒)后显示 SCRIPT KILL(另开连接发)与「放弃等待」。
//   写过数据的脚本服务器不允许 KILL(UNKILLABLE):只提示可能需要 SHUTDOWN NOSAVE,不提供这个操作。
// - 结果在 core 里按上限截断(单个字符串、每层元素、总节点、总字节、嵌套层数);这里展平成行虚拟列表显示。

import { effect, List, signal, Text, untracked, View } from "nsh";
import { EditorKeys } from "datum/native";
import { Button, Dialog, Editor, Popover, ResizablePanel, ResizablePanelGroup } from "nsh/base";
import { t } from "nsh/i18n";
import { CheckBox } from "../dialogs/controls";
import { db, errorText, sql as sqlText } from "../lib/native";
import type { LuaAnalysis, LuaSlotCheck, LuaValue } from "../lib/types";
import { conns } from "../store/conns";
import { toast } from "../store/toast";
import { viewFormat } from "../store/redisFormats";
import { editorText } from "../ui/editor";
import { ErrorText } from "../ui/error";
import { Btn, Hint, Icon, IconButton, TextField, tip } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { abandonLua, runLua, type LuaModel } from "./luaModel";
import { formatBytes } from "./RedisInsight";
import { createDecoded, DecodedBody, DecodeInfo, ElementViewDialog, FormatPicker, ViewToggle, type ElementView } from "./RedisValueView";
import { SnippetList } from "./Snippets";

/** 运行多久后显示 SCRIPT KILL / 放弃等待:Redis 默认的 busy-reply-threshold(此前服务器不处理别的客户端)。 */
const SLOW_MS = 5000;
/** 结果行里字符串最多排版的字符数(只显示一行)。 */
const ROW_CHARS = 300;
/** 结果行高。 */
const ROW_H = 24;
/** 格式偏好按「连接 + key 前缀」记住:脚本结果用一个固定的名字。 */
const FORMAT_KEY = "(lua)";
const POPUP = "rounded-(--radius-lg) border border-border bg-popover shadow-lg";

interface Row {
  path: string;
  depth: number;
  label: string;
  v: LuaValue;
  /** 「另有 n 项未显示」行 */
  omitted?: number;
}

/** 结果树展平成行;`collapsed` 里的数组不展开。 */
function flatten(v: LuaValue, collapsed: Set<string>): Row[] {
  const out: Row[] = [];
  const walk = (x: LuaValue, path: string, depth: number, label: string) => {
    out.push({ path, depth, label, v: x });
    if (x.type !== "array" || collapsed.has(path)) return;
    x.items.forEach((it, i) => walk(it, `${path}.${i}`, depth + 1, `${i + 1})`));
    if (x.len > x.items.length) out.push({ path: `${path}.more`, depth: depth + 1, label: "", v: { type: "nil" }, omitted: x.len - x.items.length });
  };
  walk(v, "r", 0, "");
  return out;
}

const oneLine = (s: string) => s.slice(0, ROW_CHARS).replace(/\r?\n/g, "↵").replace(/\t/g, " ");

function typeName(v: LuaValue): string {
  switch (v.type) {
    case "nil":
      return t("rlua.type_nil");
    case "int":
      return t("rlua.type_int");
    case "status":
      return t("rlua.type_status");
    case "error":
      return t("rlua.type_error");
    case "str":
      return t("rlua.type_str");
    case "array":
      return t("rlua.type_array");
  }
}

/** 结果的一行。 */
function ResultRow(props: { row: Row; collapsed: boolean; onToggle: () => void; onView: () => void }) {
  const r = () => props.row;
  const v = () => r().v;
  return (
    <View class="flex-row items-center gap-2 pr-3 hover:bg-list-hover" style={{ height: ROW_H, paddingLeft: 12 + r().depth * 16 }}>
      <If
        when={v().type === "array" && r().omitted === undefined}
        else={<View style={{ width: 14 }} />}
      >
        <IconButton
          icon={props.collapsed ? "chevron-right" : "chevron-down"}
          size={12}
          class="p-0.5"
          label={props.collapsed ? t("rlua.expand") : t("rlua.collapse")}
          onPress={props.onToggle}
        />
      </If>
      <If when={r().label}>
        <Text class={`${FS.xs} ${MONO} shrink-0 text-fg-faint`}>{r().label}</Text>
      </If>
      <If
        when={r().omitted === undefined}
        else={<Text class={`${FS.xs} text-warning`}>{t("rlua.omitted", { n: (r().omitted ?? 0).toLocaleString() })}</Text>}
      >
        {(() => {
          const x = v();
          switch (x.type) {
            case "nil":
              return <Text class={`${FS.xs} ${MONO} text-fg-faint`}>(nil)</Text>;
            case "int":
              return <Text class={`${FS.xs} ${MONO} text-syntax-number`}>{`(integer) ${x.text}`}</Text>;
            case "status":
              return <Text class={`${FS.xs} ${MONO} min-w-0 flex-1 text-success line-clamp-1`}>{x.text}</Text>;
            case "error":
              return <Text class={`${FS.xs} ${MONO} min-w-0 flex-1 text-negative line-clamp-1`}>{`(error) ${x.text}`}</Text>;
            case "array":
              return (
                <Text class={`${FS.xs} text-muted-foreground`}>
                  {x.len === 0 ? t("rlua.empty_array") : t("rlua.array_items", { n: x.len.toLocaleString() })}
                </Text>
              );
            case "str":
              return (
                <>
                  <Text class={`${FS.xs} ${MONO} min-w-0 flex-1 text-syntax-string line-clamp-1`}>
                    {x.b64 ? t("rlua.binary") : `"${oneLine(x.text)}"`}
                  </Text>
                  <Text class={`${FS["2xs"]} ${MONO} shrink-0 text-fg-faint`}>{formatBytes(x.len)}</Text>
                  <Btn small variant="ghost" label={t("rlua.view")} onPress={props.onView} />
                </>
              );
          }
        })()}
      </If>
    </View>
  );
}

/** 顶层就是字符串:直接显示格式识别后的内容(JSON、MessagePack、gzip…)。 */
function StringResult(props: { connId: string; v: Extract<LuaValue, { type: "str" }> }) {
  const format = () => viewFormat(props.connId, FORMAT_KEY);
  const decoded = createDecoded(() => ({ text: props.v.text, b64: props.v.b64 ?? null }), format);
  const mode = signal<"tree" | "text">("tree");
  return (
    <View class="min-h-0 flex-1 gap-2 px-3 py-2">
      <View class="flex-row items-center gap-2">
        <FormatPicker connId={props.connId} keyName={FORMAT_KEY} />
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
    </View>
  );
}

/** 服务器错误的说明(托管 Redis 禁用 EVAL、ACL 没权限、编译错误…)+ 原文。 */
function ErrorResult(props: { text: string; hint: string | null }) {
  return (
    <View testID="rlua-error" class="gap-2 px-4 py-3">
      <If when={props.hint}>
        {(h) => (
          <View class="flex-row items-start gap-2 rounded-(--radius) border border-warning bg-panel px-3 py-2">
            <Icon name="circle-alert" size={14} color="warning" />
            <Text testID="rlua-error-hint" class={`${FS.sm} min-w-0 flex-1 text-foreground`}>
              {t(`rlua.hint.${h()}`)}
            </Text>
          </View>
        )}
      </If>
      <ErrorText mono text={props.text} />
    </View>
  );
}

function Results(props: { m: LuaModel }) {
  const m = props.m;
  const collapsed = signal<Set<string>>(new Set());
  const view = signal<ElementView | null>(null);
  // 新结果:全部展开
  effect(() => {
    m.outcome();
    untracked(() => collapsed.set(new Set()));
  });
  const rows = () => {
    const o = m.outcome();
    return o ? flatten(o.value, collapsed()) : [];
  };
  const toggle = (path: string) =>
    collapsed.update((s) => {
      const next = new Set(s);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  const summary = () => {
    const o = m.outcome();
    if (!o) return "";
    const parts = [typeName(o.value), t("rlua.elapsed", { ms: o.elapsedMs.toLocaleString() }), `db${o.db}`];
    return parts.join(" · ");
  };
  return (
    <View class="min-h-0 flex-1 bg-background">
      <View class="h-8 shrink-0 flex-row items-center gap-3 border-b border-border bg-panel px-3">
        <Text testID="rlua-summary" class={`${FS.xs} text-muted-foreground`}>
          {m.run() !== null ? t("rlua.running", { s: ((m.now() - m.startedAt()) / 1000).toFixed(1) }) : summary()}
        </Text>
        <If when={m.outcome()?.truncated && m.run() === null}>
          <View class="flex-row items-center gap-1">
            <Icon name="triangle-alert" size={12} color="warning" />
            <Text testID="rlua-truncated" class={`${FS.xs} text-warning`}>{t("rlua.truncated")}</Text>
          </View>
        </If>
      </View>
      <If when={m.error()}>{(e) => <View class="px-4 py-3"><ErrorText testID="rlua-failed" mono text={e()} /></View>}</If>
      <If
        when={m.outcome()}
        else={
          <If when={m.error() === null && m.run() === null}>
            <View class="flex-1 items-center justify-center p-6">
              <Hint>{t("rlua.no_result")}</Hint>
            </View>
          </If>
        }
      >
        {(o) => (
          <If
            when={o().value.type === "error" ? o().value : null}
            else={
              <If
                when={o().value.type === "str" ? (o().value as Extract<LuaValue, { type: "str" }>) : null}
                else={
                  <List
                    testID="rlua-result"
                    each={rows()}
                    keyExtractor={(r) => r.path}
                    estimatedItemSize={ROW_H}
                    class="min-h-0 flex-1"
                  >
                    {(r) => (
                      <ResultRow
                        row={r()}
                        collapsed={collapsed().has(r().path)}
                        onToggle={() => toggle(r().path)}
                        onView={() => {
                          const x = r().v;
                          if (x.type === "str") {
                            view.set({ title: r().label ? `${t("rlua.result")} ${r().label}` : t("rlua.result"), items: [{ name: "", source: { text: x.text, b64: x.b64 ?? null } }] });
                          }
                        }}
                      />
                    )}
                  </List>
                }
              >
                {(s) => (
                  <Each of={[s()]}>
                    {(sv) => <StringResult connId={m.connId} v={sv()} />}
                  </Each>
                )}
              </If>
            }
          >
            {(e) => <ErrorResult text={(e() as { text: string }).text} hint={o().hint} />}
          </If>
        )}
      </If>
      <ElementViewDialog connId={m.connId} keyName={FORMAT_KEY} view={view()} onClose={() => view.set(null)} />
    </View>
  );
}

/** KEYS 或 ARGV 的一组输入行:可增删。 */
function ParamList(props: {
  title: string;
  name: "KEYS" | "ARGV";
  values: string[];
  slots?: number[] | null;
  disabled: boolean;
  onChange: (v: string[]) => void;
}) {
  const set = (i: number, value: string) => props.onChange(props.values.map((x, j) => (j === i ? value : x)));
  const lower = props.name.toLowerCase();
  return (
    <View class="gap-1.5">
      <View class="flex-row items-center gap-2">
        <Text class={`${FS.xs} flex-1 font-bold text-muted-foreground`}>{props.title}</Text>
        <Btn
          small
          variant="ghost"
          icon="plus"
          testID={`rlua-add-${lower}`}
          label={t("rlua.add")}
          disabled={props.disabled}
          onPress={() => props.onChange([...props.values, ""])}
        />
      </View>
      <If when={props.values.length === 0}>
        <Text class={`${FS.xs} text-fg-faint`}>{props.name === "KEYS" ? t("rlua.no_keys") : t("rlua.no_args")}</Text>
      </If>
      <Each of={props.values.map((_, i) => i)}>
        {(i) => (
          <View class="flex-row items-center gap-2">
            <Text class={`${FS.xs} ${MONO} w-[64px] text-foreground`}>{`${props.name}[${i() + 1}]`}</Text>
            <TextField
              testID={`rlua-${lower}-${i() + 1}`}
              class="h-7 min-w-0 flex-1 font-mono"
              value={props.values[i()] ?? ""}
              onChange={({ value }) => set(i(), value)}
            />
            <If when={props.slots?.[i()] !== undefined}>
              <Text class={`${FS["2xs"]} ${MONO} w-[44px] text-right text-fg-faint`}>{props.slots?.[i()]}</Text>
            </If>
            <IconButton
              icon="x"
              size={12}
              class="p-1"
              label={`${t("rlua.remove")} ${props.name}[${i() + 1}]`}
              tooltip={t("rlua.remove")}
              onPress={() => props.onChange(props.values.filter((_, j) => j !== i()))}
            />
          </View>
        )}
      </Each>
    </View>
  );
}

interface Gate {
  analysis: LuaAnalysis;
  prod: boolean;
  crossSlot: LuaSlotCheck | null;
}

/** 执行前确认:prod、写命令、动态命令、危险命令、KEYS 跨槽。prod 或有危险命令时要勾选。 */
function ConfirmRun(props: { gate: Gate | null; connName: string; onClose: () => void; onConfirm: () => void }) {
  const acked = signal(false);
  const g = () => props.gate;
  const needAck = () => !!g() && (g()!.prod || g()!.analysis.dangerous.length > 0);
  return (
    <Dialog
      testID="rlua-confirm"
      open={g() !== null}
      onOpenChange={(e) => {
        if (!e.open) props.onClose();
        acked.set(false);
      }}
      closeOnEscape
      animation="fade"
      backdropClass="bg-overlay"
      class="m-auto w-[560px] max-w-[92%] gap-3 rounded-(--radius-lg) border border-border bg-popover p-5 shadow-lg"
    >
      <View class="flex-row items-center gap-2">
        <Icon name="triangle-alert" size={16} color="warning" />
        <Text class={`${FS.base} min-w-0 flex-1 font-bold text-popover-foreground`}>{t("rlua.confirm_title")}</Text>
      </View>
      <If when={g()?.prod}>
        <Text testID="rlua-confirm-prod" class={`${FS.sm} font-bold text-negative`}>
          {t("rlua.confirm_prod", { name: props.connName })}
        </Text>
      </If>
      <If when={(g()?.analysis.dangerous.length ?? 0) > 0}>
        <View class="gap-1 rounded-(--radius) border border-destructive bg-panel px-3 py-2">
          <Text testID="rlua-confirm-danger" class={`${FS.sm} font-bold text-negative`}>
            {t("rlua.confirm_danger", { cmds: g()?.analysis.dangerous.join(", ") ?? "" })}
          </Text>
          <Text class={`${FS.xs} text-muted-foreground`}>{t("rlua.confirm_danger_note")}</Text>
        </View>
      </If>
      <If when={(g()?.analysis.writes.length ?? 0) > 0}>
        <Text testID="rlua-confirm-writes" class={`${FS.sm} text-popover-foreground`}>
          {t("rlua.confirm_writes", { cmds: g()?.analysis.writes.join(", ") ?? "" })}
        </Text>
      </If>
      <If when={(g()?.analysis.unknown.length ?? 0) > 0}>
        <Text class={`${FS.sm} text-popover-foreground`}>{t("rlua.confirm_unknown", { cmds: g()?.analysis.unknown.join(", ") ?? "" })}</Text>
      </If>
      <If when={g()?.analysis.dynamic}>
        <Text testID="rlua-confirm-dynamic" class={`${FS.sm} text-popover-foreground`}>{t("rlua.confirm_dynamic")}</Text>
      </If>
      <If when={g() && !g()!.analysis.mayWrite && g()!.analysis.dangerous.length === 0}>
        <Text class={`${FS.sm} text-popover-foreground`}>{t("rlua.confirm_read_only")}</Text>
      </If>
      <If when={g()?.crossSlot}>
        {(c) => (
          <View class="gap-1 rounded-(--radius) border border-warning bg-panel px-3 py-2">
            <Text testID="rlua-confirm-slots" class={`${FS.sm} text-warning`}>{t("rlua.confirm_cross_slot")}</Text>
            <Text class={`${FS.xs} ${MONO} text-muted-foreground line-clamp-3`}>
              {c().slots.map((s) => `${s.key} → ${s.slot}`).join("   ")}
            </Text>
          </View>
        )}
      </If>
      <If when={needAck()}>
        <View class="flex-row items-center gap-2">
          <CheckBox testID="rlua-confirm-ack" checked={acked()} onChange={(v) => acked.set(v)} label={t("rlua.confirm_ack")} />
          <Text class={`${FS.sm} text-popover-foreground`} onPress={() => acked.update((v) => !v)}>
            {t("rlua.confirm_ack")}
          </Text>
        </View>
      </If>
      <View class="flex-row justify-end gap-2 pt-1">
        <Btn variant="outline" label={t("rlua.cancel")} onPress={props.onClose} testID="rlua-confirm-cancel" />
        <Btn
          variant={g()?.analysis.mayWrite || (g()?.analysis.dangerous.length ?? 0) > 0 ? "danger" : "primary"}
          icon="play"
          disabled={needAck() && !acked()}
          label={t("rlua.confirm_run")}
          onPress={() => {
            acked.set(false);
            props.onConfirm();
          }}
          testID="rlua-confirm-ok"
        />
      </View>
    </Dialog>
  );
}

export function RedisLua(props: { m: LuaModel }) {
  const m = props.m;
  const conn = () => conns.get(m.connId);
  const prod = () => conn()?.env === "prod";
  const running = () => m.run() !== null;
  const slow = () => running() && m.now() - m.startedAt() >= SLOW_MS;
  const gate = signal<Gate | null>(null);
  const snippetsOpen = signal(false);
  const focused = signal(false);
  const text = editorText(
    () => m.script(),
    (v) => m.script.set(v),
  );

  // 编辑时的提示:写命令 / 危险命令(纯本地计算)
  const analysis = () => sqlText.analyzeLua(m.script());
  const slots = () => (m.keys().length >= 2 ? sqlText.keySlots(m.keys()) : null);

  // 集群模式只查一次(KEYS 跨槽提示用);查不到按未知
  effect(() => {
    if (m.cluster() !== null || !conns.isConnected(m.connId)) return;
    untracked(() => {
      void db
        .redisClusterEnabled(m.connId)
        .then((c) => m.cluster.set(c))
        .catch(() => {});
    });
  });

  async function requestRun() {
    if (running() || !m.script().trim()) return;
    const a = sqlText.analyzeLua(m.script());
    let crossSlot: LuaSlotCheck | null = null;
    const sc = slots();
    if (sc && !sc.same) {
      if (m.cluster() === null) m.cluster.set(await db.redisClusterEnabled(m.connId).catch(() => null));
      if (m.cluster()) crossSlot = sc;
    }
    if (prod() || a.mayWrite || a.dangerous.length > 0 || crossSlot) gate.set({ analysis: a, prod: prod(), crossSlot });
    else void runLua(m);
  }

  async function kill() {
    m.killing.set(true);
    try {
      const r = await db.redisScriptKill(m.connId);
      switch (r.status) {
        case "killed":
          toast.success(t("rlua.kill_done"));
          break;
        case "notBusy":
          toast.info(t("rlua.kill_not_busy"));
          break;
        case "unkillable":
          m.killNote.set(t("rlua.kill_unkillable"));
          break;
        case "failed":
          toast.error(`${t("rlua.kill_failed")}: ${r.message}`);
          break;
      }
    } catch (e) {
      toast.error(errorText(e));
    } finally {
      m.killing.set(false);
    }
  }

  const EDITOR_KEYS = ["mod+enter"];

  return (
    <View testID="redis-lua" class="min-h-0 flex-1 bg-background">
      <ResizablePanelGroup axis="vertical" class="flex-1" handleClass="bg-border">
        <ResizablePanel size={300} minSize={140}>
          <View class="flex-1">
            <View class="min-h-0 flex-1 flex-row">
              <View class="relative min-w-0 flex-1 bg-editor">
                <EditorKeys keys={EDITOR_KEYS} active={focused()} onKey={() => void requestRun()} />
                <Editor
                  ref={text.ref}
                  testID="rlua-editor"
                  class="flex-1 bg-editor"
                  defaultValue={text.defaultValue}
                  language="lua"
                  placeholder={t("rlua.placeholder")}
                  autoFocus
                  lineNumbers
                  softWrap={false}
                  fontSize={13}
                  onChange={text.onChange}
                  onFocus={() => focused.set(true)}
                  onBlur={() => focused.set(false)}
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
              </View>
              <View testID="rlua-params" class="w-[320px] shrink-0 gap-4 border-l border-border bg-panel px-3 py-3">
                <ParamList
                  title={t("rlua.keys_title")}
                  name="KEYS"
                  values={m.keys()}
                  slots={slots() && !slots()!.same ? slots()!.slots.map((s) => s.slot) : null}
                  disabled={false}
                  onChange={(v) => m.keys.set(v)}
                />
                <If when={slots() && !slots()!.same && m.cluster() === true}>
                  <View class="flex-row items-start gap-1.5">
                    <Icon name="triangle-alert" size={12} color="warning" />
                    <Text testID="rlua-slot-warning" class={`${FS.xs} min-w-0 flex-1 text-warning`}>{t("rlua.slot_warning")}</Text>
                  </View>
                </If>
                <ParamList
                  title={t("rlua.args_title")}
                  name="ARGV"
                  values={m.args()}
                  disabled={false}
                  onChange={(v) => m.args.set(v)}
                />
                <Hint class={FS.xs}>{t("rlua.params_hint")}</Hint>
              </View>
            </View>
            <View class="h-[38px] shrink-0 flex-row items-center gap-2 border-t border-border bg-panel px-2.5">
              <Btn
                small
                variant="primary"
                testID="rlua-run"
                icon={running() ? "loader-circle" : "play"}
                label={t("rlua.run")}
                kbd="⌘↵"
                disabled={running() || !m.script().trim()}
                onPress={() => void requestRun()}
              />
              <If when={slow()}>
                <Btn
                  small
                  variant="danger"
                  testID="rlua-kill"
                  icon="circle-x"
                  label="SCRIPT KILL"
                  tooltip={t("rlua.kill_tip")}
                  disabled={m.killing()}
                  onPress={() => void kill()}
                />
                <Btn small variant="ghost" testID="rlua-abandon" label={t("rlua.abandon")} tooltip={t("rlua.abandon_tip")} onPress={() => abandonLua(m)} />
              </If>
              {/* 编辑时的提示:写命令 / 危险命令 / 动态命令 */}
              <View class="min-w-0 flex-1 flex-row items-center gap-1.5">
                <If when={analysis().dangerous.length > 0}>
                  <Text testID="rlua-tag-danger" class={`${FS["2xs"]} shrink-0 rounded-full bg-destructive px-2 text-destructive-foreground`}>
                    {t("rlua.tag_danger", { cmds: analysis().dangerous.join(" ") })}
                  </Text>
                </If>
                <If when={analysis().writes.length > 0}>
                  <Text testID="rlua-tag-writes" class={`${FS["2xs"]} min-w-0 rounded-full border border-warning px-2 text-warning line-clamp-1`}>
                    {t("rlua.tag_writes", { cmds: analysis().writes.join(" ") })}
                  </Text>
                </If>
                <If when={analysis().dynamic || analysis().unknown.length > 0}>
                  <Text testID="rlua-tag-dynamic" class={`${FS["2xs"]} shrink-0 rounded-full border border-warning px-2 text-warning`}>
                    {t("rlua.tag_dynamic")}
                  </Text>
                </If>
                <If when={m.script().trim() && !analysis().mayWrite && analysis().dangerous.length === 0}>
                  <Text testID="rlua-tag-readonly" class={`${FS["2xs"]} shrink-0 rounded-full bg-raised px-2 text-muted-foreground`}>
                    {t("rlua.tag_read_only")}
                  </Text>
                </If>
              </View>
              <Popover
                open={snippetsOpen()}
                onOpenChange={(e) => snippetsOpen.set(e.open)}
                placement="top"
                align="end"
                offset={4}
                popupClass={POPUP}
                trigger={
                  <Button
                    testID="rlua-snippets"
                    tooltip={tip(t("rlua.snippets_tip"))}
                    class="h-6 flex-row items-center gap-1.5 rounded-(--radius) px-2 hover:bg-list-hover active:bg-secondary-active"
                  >
                    <Icon name="bookmark" size={13} />
                    <Text class={`${FS.xs} font-medium text-foreground`}>{t("snippets.button")}</Text>
                  </Button>
                }
              >
                <SnippetList
                  lang="redis"
                  lua
                  source={() => m.script().trim()}
                  hasSelection={() => false}
                  onPick={(s) => {
                    snippetsOpen.set(false);
                    m.script.set(s.sql);
                  }}
                />
              </Popover>
            </View>
            <If when={slow()}>
              <View testID="rlua-slow" class="shrink-0 flex-row items-start gap-2 border-t border-warning bg-panel px-3 py-2">
                <Icon name="triangle-alert" size={14} color="warning" />
                <View class="min-w-0 flex-1 gap-1">
                  <Text class={`${FS.xs} text-foreground`}>
                    {t("rlua.slow", { s: Math.floor((m.now() - m.startedAt()) / 1000) })}
                  </Text>
                  <If when={m.killNote()}>
                    {(n) => <Text testID="rlua-unkillable" class={`${FS.xs} font-bold text-negative`}>{n()}</Text>}
                  </If>
                </View>
              </View>
            </If>
          </View>
        </ResizablePanel>
        <ResizablePanel minSize={90}>
          <Results m={m} />
        </ResizablePanel>
      </ResizablePanelGroup>
      <ConfirmRun
        gate={gate()}
        connName={conn()?.name ?? ""}
        onClose={() => gate.set(null)}
        onConfirm={() => {
          gate.set(null);
          void runLua(m);
        }}
      />
    </View>
  );
}
