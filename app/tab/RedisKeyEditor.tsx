// Redis key 的按类型编辑器:String 直接改、Hash / ZSet / List / Set 按元素增删改、Stream 按 ID 范围分页浏览、
// 追加条目与查看消费组;标题栏设置 / 移除 TTL、重命名。key 标签页与分析页(集群里的 key,在所在节点上)共用;
// 侧栏的「新建 key」也在这里。
//
// 只改界面上显示出来的那部分(大 key 只读了前 1000 个元素、字符串前 1 MB),写完整体重读一次。
// 写操作:prod 连接一律先确认;删除元素在任何环境都先确认。确认框里的命令只是给人看的,
// 真正执行时 core 逐个参数传给 Redis(二进制安全),不拼接命令文本。

import { cleanup, effect, List, Scroll, signal, type Signal, Text, untracked, View, type JSX } from "nsh";
import { Dialog, Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { DialogFrame, Field, Segmented } from "../dialogs/controls";
import { db, errorText } from "../lib/native";
import type { KeyDetail, RedisEdit, RedisKeyValue, RedisStreamGroup, RedisStreamPage } from "../lib/types";
import { conns } from "../store/conns";
import { toast } from "../store/toast";
import { editorText } from "../ui/editor";
import { ErrorText } from "../ui/error";
import { Btn, Hint, Icon, IconButton, TextField } from "../ui/kit";
import { FS, MONO } from "../ui/text";

// ------------------------------------------------------------------ 写操作的确认

/** 确认框里给人看的命令文本:简单的参数原样,其余加引号;过长的截断(只用于显示)。 */
export function previewCommand(args: (string | number)[]): string {
  return args
    .map((a) => {
      const s = String(a);
      if (/^[\w:.\-*+@/#$]+$/.test(s) && s.length <= 80) return s;
      const cut = s.length > 120 ? `${s.slice(0, 120)}…` : s;
      return JSON.stringify(cut);
    })
    .join(" ");
}

interface Pending {
  title: string;
  command: string;
  danger: boolean;
  resolve: (go: boolean) => void;
}

/** 一次写操作:标题、给人看的命令、是否破坏性(删除),以及真正执行的调用。 */
interface WriteOp {
  title: string;
  command: string;
  danger?: boolean;
  run: () => Promise<unknown>;
  /** 成功后的提示;不给用「已保存」 */
  done?: string;
}

/** 写操作闸门:prod 连接或破坏性操作先弹确认,再执行;失败弹错误提示。返回是否执行成功。 */
function createGate(connId: string, node: () => string | null) {
  const pending = signal<Pending | null>(null);
  const busy = signal(false);
  const conn = () => conns.get(connId);
  const prod = () => conn()?.env === "prod";

  async function write(op: WriteOp): Promise<boolean> {
    if (prod() || op.danger) {
      const go = await new Promise<boolean>((resolve) =>
        pending.set({ title: op.title, command: op.command, danger: !!op.danger, resolve }),
      );
      if (!go) return false;
    }
    busy.set(true);
    try {
      await op.run();
      toast.success(op.done ?? t("rkey.saved"));
      return true;
    } catch (e) {
      toast.error(errorText(e));
      return false;
    } finally {
      busy.set(false);
    }
  }

  const answer = (go: boolean) => {
    const p = pending();
    pending.set(null);
    p?.resolve(go);
  };
  cleanup(() => answer(false));

  function Host() {
    const p = () => pending();
    const target = () => {
      const c = conn();
      if (!c) return "";
      return `${c.name} · db${c.database || "0"}${node() ? ` · ${node()}` : ""}`;
    };
    return (
      <Dialog
        testID="rkey-confirm"
        open={p() !== null}
        onOpenChange={(e) => !e.open && answer(false)}
        closeOnEscape
        animation="fade"
        backdropClass="bg-overlay"
        class="m-auto w-[540px] max-w-[92%] gap-3 rounded-(--radius-lg) border border-border bg-popover p-5 shadow-lg"
      >
        <View class="flex-row items-center gap-2">
          <Icon name="triangle-alert" size={16} color={p()?.danger ? "destructive" : "warning"} />
          <Text class={`${FS.base} min-w-0 flex-1 font-bold text-popover-foreground line-clamp-2`}>{p()?.title ?? ""}</Text>
        </View>
        <Text class={`${FS.xs} text-muted-foreground`}>{t("rkey.confirm_target", { target: target() })}</Text>
        <Scroll class="max-h-[160px] rounded-(--radius) border border-border bg-background">
          <Text testID="rkey-confirm-command" class={`${FS.xs} ${MONO} select-text px-2.5 py-2 text-foreground`}>
            {p()?.command ?? ""}
          </Text>
        </Scroll>
        <If when={prod()}>
          <Text class={`${FS.xs} font-bold text-negative`}>{t("rkey.confirm_prod")}</Text>
        </If>
        <View class="flex-row justify-end gap-2 pt-1">
          <Btn variant="outline" label={t("rkey.cancel")} onPress={() => answer(false)} testID="rkey-confirm-cancel" />
          <Btn
            variant={p()?.danger ? "danger" : "primary"}
            label={t("rkey.confirm")}
            onPress={() => answer(true)}
            testID="rkey-confirm-ok"
          />
        </View>
      </Dialog>
    );
  }

  return { write, busy, Host };
}

type Gate = ReturnType<typeof createGate>;

// ------------------------------------------------------------------ 表单弹窗

interface FormField {
  id: string;
  label: string;
  initial: string;
  multiline?: boolean;
  placeholder?: string;
}

/** 通用的小表单:若干字段 + 提交;`onSubmit` 返回 true 才关闭(出错时保留输入)。 */
interface FormSpec {
  title: string;
  fields: FormField[];
  submit: string;
  onSubmit: (v: Record<string, string>) => Promise<boolean>;
  /** 提交按钮左边的另一个动作(如「移除 TTL」) */
  secondary?: { label: string; testID: string; onPress: () => Promise<boolean> };
}

/** 多行值的输入框(不受控的 Editor,见 ui/editor)。 */
function ValueEditor(props: { value: Signal<string>; testID: string; height: number }) {
  const text = editorText(
    () => props.value(),
    (v) => props.value.set(v),
  );
  return (
    <View class="overflow-hidden rounded-(--radius) border border-input" style={{ height: props.height }}>
      <Editor
        testID={props.testID}
        class="flex-1 bg-editor"
        ref={text.ref}
        defaultValue={text.defaultValue}
        onChange={text.onChange}
        lineNumbers={false}
        folding={false}
        softWrap
        fontSize={13}
        classes={{ selection: "bg-text-selection", caret: "bg-caret", activeLine: "bg-editor-active-line" }}
      />
    </View>
  );
}

function FormBody(props: { spec: FormSpec; busy: boolean; onClose: () => void }) {
  const values = Object.fromEntries(props.spec.fields.map((f) => [f.id, signal(f.initial)]));
  const collect = () => Object.fromEntries(Object.entries(values).map(([k, v]) => [k, v()]));
  const submit = async () => {
    if (await props.spec.onSubmit(collect())) props.onClose();
  };
  return (
    <View class="gap-3">
      <Each of={props.spec.fields} key={(f) => f.id}>
        {(f) => (
          <Field label={f().label} class="">
            <If
              when={f().multiline}
              else={
                <TextField
                  testID={`rkey-field:${f().id}`}
                  class="font-mono"
                  value={values[f().id]()}
                  placeholder={f().placeholder}
                  onChange={({ value }) => values[f().id].set(value)}
                />
              }
            >
              <ValueEditor value={values[f().id]} testID={`rkey-field:${f().id}`} height={160} />
            </If>
          </Field>
        )}
      </Each>
      <View class="flex-row items-center justify-end gap-2 pt-1">
        <If when={props.spec.secondary}>
          {(s) => (
            <Btn
              testID={s().testID}
              variant="outline"
              disabled={props.busy}
              label={s().label}
              onPress={() => void s().onPress().then((ok) => ok && props.onClose())}
            />
          )}
        </If>
        <View class="flex-1" />
        <Btn testID="rkey-form-cancel" variant="outline" label={t("rkey.cancel")} onPress={props.onClose} />
        <Btn testID="rkey-form-ok" variant="primary" disabled={props.busy} label={props.spec.submit} onPress={() => void submit()} />
      </View>
    </View>
  );
}

function FormDialog(props: { spec: FormSpec | null; busy: boolean; onClose: () => void }) {
  return (
    <DialogFrame testID="rkey-form" open={props.spec !== null} onClose={props.onClose} title={props.spec?.title ?? ""} width={560}>
      {/* 每次打开都重建表单,初值取自新的 spec */}
      <Each of={props.spec ? [props.spec] : []}>{(s) => <FormBody spec={s()} busy={props.busy} onClose={props.onClose} />}</Each>
    </DialogFrame>
  );
}

// ------------------------------------------------------------------ 元素列表

interface Row {
  /** 在 value.rows 里的下标 */
  ix: number;
  cells: string[];
}

/** 元素表格(虚拟列表):表头 + 每行的单元格与编辑 / 删除按钮。 */
function ElementList(props: {
  columns: string[];
  rows: Row[];
  /** 首列固定宽度(多列时) */
  firstWidth?: number;
  readOnly: boolean;
  onEdit?: (r: Row) => void;
  onDelete?: (r: Row) => void;
}) {
  const multi = () => props.columns.length > 1;
  const actions = () => (props.readOnly ? 0 : (props.onEdit ? 26 : 0) + (props.onDelete ? 26 : 0));
  return (
    <View testID="rkey-elements" class="min-h-0 flex-1 rounded-(--radius) border border-border">
      <View class="flex-row gap-3 border-b border-border bg-panel px-3 py-1.5">
        <Each of={props.columns}>
          {(c, i) => (
            <Text
              class={`${FS.xs} font-bold text-muted-foreground`}
              classList={{ "min-w-0 flex-1": !(multi() && i() === 0) }}
              style={multi() && i() === 0 ? { width: props.firstWidth ?? 220 } : undefined}
            >
              {c()}
            </Text>
          )}
        </Each>
        <View style={{ width: actions() }} />
      </View>
      <List
        each={props.rows}
        keyExtractor={(r) => String(r.ix)}
        estimatedItemSize={28}
        class="min-h-0 flex-1"
        empty={<Hint class={`${FS.xs} p-3`}>{t("rkey.no_elements")}</Hint>}
      >
        {(r) => (
          <View class="min-h-[28px] flex-row items-center gap-3 border-b border-border px-3 hover:bg-list-hover">
            <Each of={r().cells}>
              {(cell, i) => (
                <Text
                  class={`${FS.xs} ${MONO} select-text line-clamp-2`}
                  classList={{
                    "text-muted-foreground": multi() && i() === 0,
                    "text-foreground": !(multi() && i() === 0),
                    "min-w-0 flex-1": !(multi() && i() === 0),
                  }}
                  style={multi() && i() === 0 ? { width: props.firstWidth ?? 220 } : undefined}
                >
                  {cell()}
                </Text>
              )}
            </Each>
            <If when={!props.readOnly}>
              <View class="flex-row">
                <If when={props.onEdit}>
                  {(edit) => (
                    <IconButton
                      icon="pencil"
                      size={13}
                      class="p-1"
                      label={`${t("rkey.edit")} ${r().cells[0]}`}
                      tooltip={t("rkey.edit")}
                      onPress={() => edit()(r())}
                    />
                  )}
                </If>
                <If when={props.onDelete}>
                  {(del) => (
                    <IconButton
                      icon="trash"
                      size={13}
                      class="p-1"
                      label={`${t("rkey.delete")} ${r().cells[0]}`}
                      tooltip={t("rkey.delete")}
                      onPress={() => del()(r())}
                    />
                  )}
                </If>
              </View>
            </If>
          </View>
        )}
      </List>
    </View>
  );
}

// ------------------------------------------------------------------ 各类型

interface Ctx {
  connId: string;
  node: string | null;
  key: string;
  gate: Gate;
  form: Signal<FormSpec | null>;
  readOnly: boolean;
  reload: () => Promise<void>;
}

/** 执行一次元素级编辑,成功后重读。 */
async function edit(ctx: Ctx, op: Omit<WriteOp, "run">, e: RedisEdit): Promise<boolean> {
  const ok = await ctx.gate.write({ ...op, run: () => db.redisEdit(ctx.connId, ctx.node, ctx.key, e) });
  if (ok) await ctx.reload();
  return ok;
}

function StringBody(props: { ctx: Ctx; value: RedisKeyValue }) {
  const ctx = props.ctx;
  const original = () => props.value.rows[0]?.[0] ?? "";
  const draft = signal(untracked(original));
  // 重读后(保存成功、刷新)以新值为准
  effect(() => {
    const v = original();
    untracked(() => draft.set(v));
  });
  const dirty = () => draft() !== original();
  const locked = () => ctx.readOnly || !!props.value.truncated;
  const save = () =>
    void edit(
      ctx,
      { title: t("rkey.confirm_save", { key: ctx.key }), command: previewCommand(["SET", ctx.key, draft(), "XX"]) },
      { op: "stringSet", value: draft() },
    );
  return (
    <View class="min-h-0 flex-1 gap-2">
      <View class="min-h-0 flex-1 overflow-hidden rounded-(--radius) border border-border">
        <StringEditor value={draft} readOnly={locked()} />
      </View>
      <View class="flex-row items-center gap-2">
        <Text class={`${FS.xs} ${MONO} flex-1 text-fg-faint`}>{t("rkey.chars", { n: [...draft()].length })}</Text>
        <If when={!locked()}>
          <Btn small variant="outline" label={t("rkey.revert")} disabled={!dirty()} onPress={() => draft.set(original())} />
          <Btn
            testID="rkey-string-save"
            small
            variant="primary"
            icon="check"
            label={t("rkey.save")}
            disabled={!dirty() || ctx.gate.busy()}
            onPress={save}
          />
        </If>
      </View>
    </View>
  );
}

function StringEditor(props: { value: Signal<string>; readOnly: boolean }) {
  const text = editorText(
    () => props.value(),
    (v) => props.value.set(v),
  );
  return (
    <Editor
      testID="rkey-string"
      class="flex-1 bg-editor"
      ref={text.ref}
      defaultValue={text.defaultValue}
      onChange={text.onChange}
      readOnly={props.readOnly}
      lineNumbers={false}
      folding={false}
      softWrap
      fontSize={13}
      classes={{ selection: "bg-text-selection", caret: "bg-caret", activeLine: "bg-editor-active-line" }}
    />
  );
}

/** 列表上方:筛选框 + 已显示 / 总数 + 新增按钮。 */
function ElementsBar(props: { filter: Signal<string>; shown: number; total: number; children?: JSX.Element }) {
  return (
    <View class="flex-row items-center gap-2">
      <View class="relative w-[240px] justify-center">
        <TextField
          testID="rkey-filter"
          class="h-7 pl-7"
          value={props.filter()}
          placeholder={t("rkey.filter")}
          onChange={({ value }) => props.filter.set(value)}
        />
        <View class="pointer-events-none absolute left-2.5">
          <Icon name="search" size={13} color="fg-faint" />
        </View>
      </View>
      <Text class={`${FS.xs} flex-1 text-muted-foreground`}>{t("rkey.shown", { shown: props.shown, total: props.total })}</Text>
      {props.children}
    </View>
  );
}

function filtered(value: RedisKeyValue, q: string): Row[] {
  const rows = value.rows.map((cells, ix) => ({ ix, cells }));
  const s = q.trim().toLowerCase();
  return s ? rows.filter((r) => r.cells.some((c) => c.toLowerCase().includes(s))) : rows;
}

function CollectionBody(props: { ctx: Ctx; value: RedisKeyValue; size: number }) {
  const ctx = props.ctx;
  const filter = signal("");
  const rows = () => filtered(props.value, filter());
  const type = () => props.value.keyType;
  const k = ctx.key;
  const open = (spec: FormSpec) => ctx.form.set(spec);

  const add = () => {
    switch (type()) {
      case "hash":
        return open({
          title: t("rkey.add_field"),
          submit: t("rkey.add"),
          fields: [
            { id: "field", label: t("rkey.col_field"), initial: "" },
            { id: "value", label: t("rkey.col_value"), initial: "", multiline: true },
          ],
          onSubmit: (v) =>
            edit(ctx, { title: t("rkey.confirm_add", { key: k }), command: previewCommand(["HSETNX", k, v.field, v.value]) }, {
              op: "hashAdd",
              field: v.field,
              value: v.value,
            }),
        });
      case "zset":
        return open({
          title: t("rkey.add_member"),
          submit: t("rkey.add"),
          fields: [
            { id: "member", label: t("rkey.col_member"), initial: "" },
            { id: "score", label: t("rkey.col_score"), initial: "0" },
          ],
          onSubmit: (v) =>
            edit(ctx, { title: t("rkey.confirm_add", { key: k }), command: previewCommand(["ZADD", k, "NX", v.score, v.member]) }, {
              op: "zsetAdd",
              member: v.member,
              score: v.score,
            }),
        });
      case "set":
        return open({
          title: t("rkey.add_member"),
          submit: t("rkey.add"),
          fields: [{ id: "member", label: t("rkey.col_member"), initial: "", multiline: true }],
          onSubmit: (v) =>
            edit(ctx, { title: t("rkey.confirm_add", { key: k }), command: previewCommand(["SADD", k, v.member]) }, {
              op: "setAdd",
              member: v.member,
            }),
        });
    }
  };

  const push = (head: boolean) =>
    open({
      title: head ? t("rkey.push_head") : t("rkey.push_tail"),
      submit: t("rkey.add"),
      fields: [{ id: "value", label: t("rkey.col_value"), initial: "", multiline: true }],
      onSubmit: (v) =>
        edit(ctx, { title: t("rkey.confirm_add", { key: k }), command: previewCommand([head ? "LPUSHX" : "RPUSHX", k, v.value]) }, {
          op: "listPush",
          value: v.value,
          head,
        }),
    });

  const editRow = (r: Row) => {
    const [a, b] = r.cells;
    switch (type()) {
      case "hash":
        return open({
          title: t("rkey.edit_field", { name: a }),
          submit: t("rkey.save"),
          fields: [
            { id: "field", label: t("rkey.col_field"), initial: a },
            { id: "value", label: t("rkey.col_value"), initial: b, multiline: true },
          ],
          onSubmit: (v) =>
            edit(
              ctx,
              {
                title: t("rkey.confirm_save", { key: k }),
                command:
                  v.field === a
                    ? previewCommand(["HSET", k, v.field, v.value])
                    : `MULTI\n${previewCommand(["HSET", k, v.field, v.value])}\n${previewCommand(["HDEL", k, a])}\nEXEC`,
              },
              { op: "hashSet", field: v.field, value: v.value, oldField: a },
            ),
        });
      case "zset":
        return open({
          title: t("rkey.edit_member", { name: a }),
          submit: t("rkey.save"),
          fields: [
            { id: "member", label: t("rkey.col_member"), initial: a },
            { id: "score", label: t("rkey.col_score"), initial: b },
          ],
          onSubmit: (v) =>
            edit(
              ctx,
              {
                title: t("rkey.confirm_save", { key: k }),
                command:
                  v.member === a
                    ? previewCommand(["ZADD", k, v.score, v.member])
                    : `MULTI\n${previewCommand(["ZADD", k, v.score, v.member])}\n${previewCommand(["ZREM", k, a])}\nEXEC`,
              },
              { op: "zsetSet", member: v.member, score: v.score, oldMember: a },
            ),
        });
      case "list": {
        const index = Number(a);
        return open({
          title: t("rkey.edit_index", { index }),
          submit: t("rkey.save"),
          fields: [{ id: "value", label: t("rkey.col_value"), initial: b, multiline: true }],
          onSubmit: (v) =>
            edit(ctx, { title: t("rkey.confirm_save", { key: k }), command: previewCommand(["LSET", k, index, v.value]) }, {
              op: "listSet",
              index,
              old: b,
              value: v.value,
            }),
        });
      }
    }
  };

  const del = (r: Row) => {
    const [a, b] = r.cells;
    const title = t("rkey.confirm_delete", { name: type() === "list" ? `#${a}` : a });
    const done = t("rkey.deleted");
    switch (type()) {
      case "hash":
        return void edit(ctx, { title, done, danger: true, command: previewCommand(["HDEL", k, a]) }, { op: "hashDel", field: a });
      case "zset":
        return void edit(ctx, { title, done, danger: true, command: previewCommand(["ZREM", k, a]) }, { op: "zsetDel", member: a });
      case "set":
        return void edit(ctx, { title, done, danger: true, command: previewCommand(["SREM", k, a]) }, { op: "setDel", member: a });
      case "list": {
        const index = Number(a);
        // 按下标删:占位值 + LREM(按值 LREM 遇到重复值会删错格),两步在一个事务里
        const tomb = "__datum_deleted__<uuid>";
        const command = `MULTI\n${previewCommand(["LSET", k, index, tomb])}\n${previewCommand(["LREM", k, 1, tomb])}\nEXEC`;
        return void edit(ctx, { title, done, danger: true, command }, { op: "listDel", index, old: b });
      }
    }
  };

  const columns = () => {
    switch (type()) {
      case "hash":
        return [t("rkey.col_field"), t("rkey.col_value")];
      case "zset":
        return [t("rkey.col_member"), t("rkey.col_score")];
      case "list":
        return [t("rkey.col_index"), t("rkey.col_value")];
      default:
        return [t("rkey.col_member")];
    }
  };

  return (
    <View class="min-h-0 flex-1 gap-2">
      <ElementsBar filter={filter} shown={props.value.rows.length} total={props.size}>
        <If when={!ctx.readOnly}>
          <If
            when={type() === "list"}
            else={<Btn testID="rkey-add" small icon="plus" label={t("rkey.add")} onPress={add} />}
          >
            <Btn testID="rkey-push-head" small icon="plus" label={t("rkey.push_head")} onPress={() => push(true)} />
            <Btn testID="rkey-push-tail" small icon="plus" label={t("rkey.push_tail")} onPress={() => push(false)} />
          </If>
        </If>
      </ElementsBar>
      <ElementList
        columns={columns()}
        rows={rows()}
        firstWidth={type() === "list" ? 64 : type() === "zset" ? undefined : 220}
        readOnly={ctx.readOnly}
        onEdit={type() === "set" ? undefined : editRow}
        onDelete={del}
      />
    </View>
  );
}

// ------------------------------------------------------------------ Stream

/** 一页的条数。 */
const STREAM_PAGE = 50;

function formatIdle(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  return `${Math.round(s / 86400)}d`;
}

function Groups(props: { groups: RedisStreamGroup[] }) {
  return (
    <If when={props.groups.length > 0} else={<Hint class={FS.xs}>{t("rkey.no_groups")}</Hint>}>
      <View testID="rkey-groups" class="gap-2">
        <Each of={props.groups} key={(g) => g.name}>
          {(g) => (
            <View class="gap-1 rounded-(--radius) border border-border px-3 py-2">
              <View class="flex-row flex-wrap items-center gap-x-4 gap-y-1">
                <Text class={`${FS.sm} ${MONO} font-bold text-foreground`}>{g().name}</Text>
                <Text class={`${FS.xs} text-muted-foreground`}>{t("rkey.group_consumers", { n: g().consumers })}</Text>
                <Text class={`${FS.xs} text-muted-foreground`}>
                  {g().pending > 0
                    ? t("rkey.group_pending_range", { n: g().pending, min: g().pendingMin ?? "", max: g().pendingMax ?? "" })
                    : t("rkey.group_pending", { n: 0 })}
                </Text>
                <Text class={`${FS.xs} ${MONO} text-muted-foreground`}>{t("rkey.group_last", { id: g().lastDeliveredId })}</Text>
                <If when={g().lag !== null}>
                  <Text class={`${FS.xs} text-muted-foreground`}>{t("rkey.group_lag", { n: g().lag ?? 0 })}</Text>
                </If>
              </View>
              <Each of={g().consumerList} key={(c) => c.name}>
                {(c) => (
                  <View class="flex-row items-center gap-4 pl-3">
                    <Text class={`${FS.xs} ${MONO} w-[160px] text-foreground line-clamp-1`}>{c().name}</Text>
                    <Text class={`${FS.xs} w-[110px] text-muted-foreground`}>{t("rkey.group_pending", { n: c().pending })}</Text>
                    <Text class={`${FS.xs} text-muted-foreground`}>{t("rkey.consumer_idle", { t: formatIdle(c().idleMs) })}</Text>
                  </View>
                )}
              </Each>
            </View>
          )}
        </Each>
      </View>
    </If>
  );
}

function StreamBody(props: { ctx: Ctx; version: number }) {
  const ctx = props.ctx;
  const k = ctx.key;
  const rev = signal(true);
  const start = signal("-");
  const end = signal("+");
  /** 各页的起点(正序为 start,倒序为 end);末尾是当前页 */
  const cursors = signal<string[]>([]);
  const page = signal<RedisStreamPage | null>(null);
  const groups = signal<RedisStreamGroup[] | null>(null);
  const err = signal<string | null>(null);
  let alive = true;
  cleanup(() => (alive = false));

  async function load() {
    const cur = cursors().at(-1);
    const s = !rev() && cur ? cur : start().trim() || "-";
    const e = rev() && cur ? cur : end().trim() || "+";
    try {
      const p = await db.redisStreamRange(ctx.connId, ctx.node, k, s, e, STREAM_PAGE, rev());
      if (!alive) return;
      page.set(p);
      err.set(null);
    } catch (e) {
      if (alive) err.set(errorText(e));
    }
    try {
      const g = await db.redisStreamGroups(ctx.connId, ctx.node, k);
      if (alive) groups.set(g);
    } catch (e) {
      if (alive) err.set(errorText(e));
    }
  }
  const first = () => {
    cursors.set([]);
    void load();
  };
  // 写完重读(version 变化)时留在当前页
  effect(() => {
    void props.version;
    untracked(() => void load());
  });

  const add = () =>
    ctx.form.set({
      title: t("rkey.add_entry"),
      submit: t("rkey.add"),
      fields: [
        { id: "id", label: t("rkey.entry_id"), initial: "*" },
        { id: "fields", label: t("rkey.entry_fields"), initial: "", multiline: true, placeholder: "field value" },
      ],
      onSubmit: async (v) => {
        const fields = parseFieldLines(v.fields);
        if (fields.length === 0) {
          toast.error(t("rkey.entry_fields_empty"));
          return false;
        }
        const id = v.id.trim() || "*";
        return edit(ctx, { title: t("rkey.confirm_add", { key: k }), command: previewCommand(["XADD", k, id, ...fields.flat()]) }, {
          op: "streamAdd",
          id,
          fields,
        });
      },
    });
  const del = (r: Row) =>
    void edit(
      ctx,
      { title: t("rkey.confirm_delete", { name: r.cells[0] }), done: t("rkey.deleted"), danger: true, command: previewCommand(["XDEL", k, r.cells[0]]) },
      { op: "streamDel", id: r.cells[0] },
    );

  const rows = (): Row[] =>
    (page()?.entries ?? []).map((e, ix) => ({ ix, cells: [e.id, e.fields.map(([f, v]) => `${f}: ${v}`).join("  ·  ")] }));

  return (
    <View class="min-h-0 flex-1 gap-2">
      <View class="flex-row flex-wrap items-center gap-2">
        <View class="w-[200px]">
          <Segmented
            testID="rkey-stream-order"
            options={[
              { value: "desc", label: t("rkey.newest_first") },
              { value: "asc", label: t("rkey.oldest_first") },
            ]}
            value={rev() ? "desc" : "asc"}
            onChange={(v) => {
              rev.set(v === "desc");
              first();
            }}
          />
        </View>
        <TextField testID="rkey-stream-start" class="h-7 w-[150px] font-mono" value={start()} placeholder="-" onChange={({ value }) => start.set(value)} />
        <Text class={`${FS.xs} text-muted-foreground`}>…</Text>
        <TextField testID="rkey-stream-end" class="h-7 w-[150px] font-mono" value={end()} placeholder="+" onChange={({ value }) => end.set(value)} />
        <Btn testID="rkey-stream-apply" small variant="outline" label={t("rkey.apply_range")} onPress={first} />
        <View class="flex-1" />
        <If when={!ctx.readOnly}>
          <Btn testID="rkey-stream-add" small icon="plus" label={t("rkey.add_entry")} onPress={add} />
        </If>
      </View>
      <If when={err()}>{(e) => <ErrorText text={e()} />}</If>
      <ElementList
        columns={["ID", t("rkey.entry_fields")]}
        rows={rows()}
        firstWidth={190}
        readOnly={ctx.readOnly}
        onDelete={del}
      />
      <View class="flex-row items-center gap-2">
        <Text testID="rkey-stream-page" class={`${FS.xs} flex-1 text-muted-foreground`}>
          {t("rkey.stream_page", { page: cursors().length + 1, shown: page()?.entries.length ?? 0, total: page()?.length ?? 0 })}
        </Text>
        <Btn small variant="outline" icon="chevrons-left" label={t("rkey.first_page")} disabled={cursors().length === 0} onPress={first} />
        <Btn
          testID="rkey-stream-prev"
          small
          variant="outline"
          icon="chevron-left"
          label={t("rkey.prev_page")}
          disabled={cursors().length === 0}
          onPress={() => {
            cursors.set(cursors().slice(0, -1));
            void load();
          }}
        />
        <Btn
          testID="rkey-stream-next"
          small
          variant="outline"
          icon="chevron-right"
          label={t("rkey.next_page")}
          disabled={!page()?.next}
          onPress={() => {
            const n = page()?.next;
            if (!n) return;
            cursors.set([...cursors(), n]);
            void load();
          }}
        />
      </View>
      <View class="max-h-[40%] gap-1.5">
        <Text class={`${FS.xs} font-bold text-fg-faint`}>{t("rkey.groups")}</Text>
        <Scroll class="min-h-0 shrink">
          <If when={groups()} else={<Hint class={FS.xs}>{t("rinsight.loading")}</Hint>}>
            {(g) => <Groups groups={g()} />}
          </If>
        </Scroll>
      </View>
    </View>
  );
}

/** 条目字段:每行 `字段 值`(第一个空白分开,值里可以有空格)。 */
export function parseFieldLines(text: string): [string, string][] {
  return text
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l !== "")
    .map((l) => {
      const m = /^(\S+)\s+([\s\S]*)$/.exec(l);
      return m ? ([m[1], m[2]] as [string, string]) : ([l, ""] as [string, string]);
    });
}

// ------------------------------------------------------------------ 编辑器本体

export function formatKeyTtl(ttl: number): string {
  if (ttl === -1) return t("detail.ttl_never");
  if (ttl === -2) return t("detail.ttl_missing");
  return `${ttl}s`;
}

/**
 * 一个 key 的编辑器。`value` / `detail` 由宿主读取(key 标签页走模型,分析页自己读),`reload` 重读二者;
 * `node` 为集群模式下 key 所在的节点(所有读写都发到那里),单机为 null。
 */
export function RedisKeyEditor(props: {
  connId: string;
  node: string | null;
  value: RedisKeyValue | null;
  detail: KeyDetail | null;
  reload: () => Promise<void>;
  onRenamed: (newKey: string) => void;
}) {
  const gate = createGate(props.connId, () => props.node);
  const form = signal<FormSpec | null>(null);
  /** 每次写完 +1,Stream 视图据此重读当前页 */
  const version = signal(0);
  const key = () => props.value?.key ?? props.detail?.key ?? "";
  const type = () => props.value?.keyType ?? props.detail?.keyType ?? "";
  const readOnly = () => !!props.value?.binary;
  const ctx = (): Ctx => ({
    connId: props.connId,
    node: props.node,
    key: key(),
    gate,
    form,
    readOnly: readOnly(),
    reload: async () => {
      await props.reload();
      version.update((n) => n + 1);
    },
  });

  const editTtl = () =>
    form.set({
      title: t("rkey.ttl_title", { key: key() }),
      submit: t("rkey.ttl_set"),
      fields: [
        {
          id: "ttl",
          label: t("rkey.ttl_seconds"),
          initial: (props.detail?.ttl ?? -1) > 0 ? String(props.detail?.ttl) : "",
          placeholder: "3600",
        },
      ],
      onSubmit: async (v) => {
        const n = Number(v.ttl.trim());
        if (!Number.isInteger(n) || n <= 0) {
          toast.error(t("rkey.ttl_invalid"));
          return false;
        }
        const k = key();
        const ok = await gate.write({
          title: t("rkey.confirm_ttl", { key: k }),
          command: previewCommand(["EXPIRE", k, n]),
          run: () => db.redisSetTtl(props.connId, props.node, k, n),
        });
        if (ok) await ctx().reload();
        return ok;
      },
      secondary: {
        label: t("rkey.ttl_remove"),
        testID: "rkey-ttl-persist",
        onPress: async () => {
          const k = key();
          const ok = await gate.write({
            title: t("rkey.confirm_ttl", { key: k }),
            command: previewCommand(["PERSIST", k]),
            run: () => db.redisSetTtl(props.connId, props.node, k, null),
          });
          if (ok) await ctx().reload();
          return ok;
        },
      },
    });

  const rename = () =>
    form.set({
      title: t("rkey.rename_title", { key: key() }),
      submit: t("rkey.rename"),
      fields: [{ id: "name", label: t("rkey.new_name"), initial: key() }],
      onSubmit: async (v) => {
        const k = key();
        const next = v.name;
        if (next === "" || next === k) return next === k;
        const ok = await gate.write({
          title: t("rkey.confirm_rename", { key: k, next }),
          command: previewCommand(["RENAMENX", k, next]),
          run: () => db.redisRename(props.connId, props.node, k, next),
          done: t("rkey.renamed"),
        });
        if (ok) props.onRenamed(next);
        return ok;
      },
    });

  const missing = () => props.value !== null && type() === "none";

  return (
    <View testID="rkey-editor" class="min-h-0 flex-1 gap-3">
      <View class="flex-row items-center gap-3">
        <Icon name="key-round" size={15} color="data" />
        <Text class={`${FS.base} ${MONO} min-w-0 shrink font-bold text-foreground line-clamp-1 select-text`}>{key()}</Text>
        <Text class={`${FS.xs} rounded-full bg-raised px-2 text-data`}>{type()}</Text>
        <If when={props.detail}>
          {(d) => (
            <>
              <Text class={`${FS.xs} text-muted-foreground`}>{t(d().keyType === "stream" ? "rkey.size_entries" : d().keyType === "string" ? "rkey.size_bytes" : "rkey.size", { n: d().size.toLocaleString() })}</Text>
              <Text testID="rkey-ttl" class={`${FS.xs} text-muted-foreground`}>
                TTL {formatKeyTtl(d().ttl)}
              </Text>
            </>
          )}
        </If>
        <If when={props.node}>
          <Text class={`${FS.xs} ${MONO} text-fg-faint`}>{props.node}</Text>
        </If>
        <View class="flex-1" />
        <If when={!missing()}>
          <Btn testID="rkey-ttl-btn" small variant="outline" icon="rotate-ccw-clock" label={t("rkey.ttl")} onPress={editTtl} />
          <Btn testID="rkey-rename-btn" small variant="outline" icon="type" label={t("rkey.rename")} onPress={rename} />
        </If>
        <IconButton icon="refresh-cw" size={14} tooltip={t("rkey.refresh")} label={t("rkey.refresh")} onPress={() => void ctx().reload()} />
      </View>
      <If when={props.value?.truncated}>
        <Text class={`${FS.xs} text-warning`}>
          {type() === "string" ? t("rkey.string_truncated") : t("rkey.truncated")}
        </Text>
      </If>
      <If when={readOnly()}>
        <Text class={`${FS.xs} text-warning`}>{t("rkey.binary_read_only")}</Text>
      </If>
      <If when={missing()}>
        <Hint>{t("rkey.missing")}</Hint>
      </If>
      <If when={props.value && !missing() ? props.value : null}>
        {(v) => (
          <Each of={[type()]}>
            {(ty) =>
              ty() === "string" ? (
                <StringBody ctx={ctx()} value={v()} />
              ) : ty() === "stream" ? (
                <StreamBody ctx={ctx()} version={version()} />
              ) : ["hash", "list", "set", "zset"].includes(ty()) ? (
                <CollectionBody ctx={ctx()} value={v()} size={props.detail?.size ?? v().rows.length} />
              ) : (
                <Hint>{t("rkey.unsupported_type", { type: ty() })}</Hint>
              )
            }
          </Each>
        )}
      </If>
      <FormDialog spec={form()} busy={gate.busy()} onClose={() => form.set(null)} />
      <gate.Host />
    </View>
  );
}

// ------------------------------------------------------------------ 新建 key

const NEW_TYPES = ["string", "hash", "list", "set", "zset", "stream"] as const;
type NewType = (typeof NEW_TYPES)[number];

/** 每种类型初值的写法:一行一个元素;hash / stream 为 `字段 值`,zset 为 `分数 成员`。 */
function itemsOf(type: NewType, text: string): string[][] {
  if (type === "string") return [[text]];
  const lines = text.split("\n").filter((l) => l.trim() !== "");
  if (type === "list" || type === "set") return lines.map((l) => [l]);
  const pairs = parseFieldLines(text);
  // zset 的一行是「分数 成员」(与 ZADD 的参数顺序一致),core 那边要 [成员, 分数]
  return type === "zset" ? pairs.map(([score, member]) => [member, score]) : pairs;
}

/** 侧栏「新建 key」:类型 + 名称 + 初值 + 可选 TTL。 */
export function NewKeyDialog(props: { connId: string; open: boolean; onClose: () => void; onCreated: (key: string) => void }) {
  const gate = createGate(props.connId, () => null);
  const name = signal("");
  const type = signal<NewType>("string");
  const ttl = signal("");
  const value = signal("");
  // 每次打开清空
  effect(() => {
    if (!props.open) return;
    untracked(() => {
      name.set("");
      type.set("string");
      ttl.set("");
      value.set("");
    });
  });
  const hint = () => (type() === "string" ? "" : t(`rkey.items_hint_${type()}`));
  const submit = async () => {
    const key = name();
    const ttlText = ttl().trim();
    const secs = ttlText === "" ? null : Number(ttlText);
    if (secs !== null && (!Number.isInteger(secs) || secs <= 0)) {
      toast.error(t("rkey.ttl_invalid"));
      return;
    }
    const items = itemsOf(type(), value());
    const head: Record<NewType, string[]> = {
      string: ["SET", key, ...(items[0] ?? []), "NX", ...(secs ? ["EX", String(secs)] : [])],
      hash: ["HSET", key, ...items.flat()],
      list: ["RPUSH", key, ...items.flat()],
      set: ["SADD", key, ...items.flat()],
      zset: ["ZADD", key, ...items.flatMap(([m, s]) => [s, m])],
      stream: ["XADD", key, "*", ...items.flat()],
    };
    const command =
      type() !== "string" && secs ? `MULTI\n${previewCommand(head[type()])}\n${previewCommand(["EXPIRE", key, secs])}\nEXEC` : previewCommand(head[type()]);
    const ok = await gate.write({
      title: t("rkey.confirm_create", { key }),
      command,
      run: () => db.redisCreateKey(props.connId, null, { key, keyType: type(), items, ttl: secs }),
      done: t("rkey.created"),
    });
    if (ok) props.onCreated(key);
  };
  return (
    <>
      <DialogFrame testID="rkey-new" open={props.open} onClose={props.onClose} title={t("rkey.new_key")} width={600}>
        <View class="gap-3">
          <Field label={t("rkey.key_type")} class="">
            <Segmented
              testID="rkey-new-type"
              options={NEW_TYPES.map((v) => ({ value: v, label: v }))}
              value={type()}
              onChange={(v) => type.set(v)}
            />
          </Field>
          <View class="flex-row gap-3">
            <Field label={t("rkey.key_name")}>
              <TextField testID="rkey-new-name" class="font-mono" autoFocus value={name()} onChange={({ value: v }) => name.set(v)} />
            </Field>
            <Field label={t("rkey.ttl_optional")} class="shrink-0" style={{ width: 150 }}>
              <TextField testID="rkey-new-ttl" class="font-mono" value={ttl()} placeholder={t("rkey.ttl_never_placeholder")} onChange={({ value: v }) => ttl.set(v)} />
            </Field>
          </View>
          <Field label={type() === "string" ? t("rkey.col_value") : t("rkey.items")} class="">
            <ValueEditor value={value} testID="rkey-new-value" height={180} />
            <If when={hint()}>
              <Text class={`${FS.xs} text-fg-faint`}>{hint()}</Text>
            </If>
          </Field>
          <View class="flex-row justify-end gap-2 pt-1">
            <Btn testID="rkey-new-cancel" variant="outline" label={t("rkey.cancel")} onPress={props.onClose} />
            <Btn
              testID="rkey-new-ok"
              variant="primary"
              disabled={gate.busy() || name() === ""}
              label={t("rkey.create")}
              onPress={() => void submit()}
            />
          </View>
        </View>
      </DialogFrame>
      <gate.Host />
    </>
  );
}
