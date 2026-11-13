// 关系库诊断 tab(PG / MySQL / SQLite):顶部分区切换,下面是当前分区的卡片或表格。
// 分区由 core 声明(diag.rs),这里只按声明画:列名按键翻译,`_bytes` / `_pct` / `_ms` 后缀统一排版,
// 行操作(取消 / 终止会话、KILL、VACUUM)与整库操作(SQLite VACUUM / ANALYZE)都先弹确认框。

import { cleanup, Clipboard, effect, Scroll, signal, Text, untracked, View } from "nsh";
import { Dialog } from "nsh/base";
import { t } from "nsh/i18n";
import { askAiReviewSql } from "../chat/index";
import { db, errorText, sql as sqlNative } from "../lib/native";
import type { DbKind, DiagAction, DiagResult, DiagSection } from "../lib/types";
import { conns, loadTables } from "../store/conns";
import { toast } from "../store/toast";
import { Btn, EnvBadge, Hint, Icon } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { formatBytes } from "./RedisInsight";
import { ErrorText } from "../ui/error";
import { DropMenu, MItem } from "../ui/menu";
import { explainOn, openQueryOn } from "./index";
import { namedPlaceholders } from "./placeholders";

/** 自动刷新间隔。 */
const AUTO_MS = 5000;

/** 长文本列(SQL、定义、检查结果):更宽、最多三行。 */
const WIDE = new Set(["query", "blocked_query", "blocking_query", "definition", "message"]);
/** 只给操作用、不显示的列(MySQL 慢 SQL 的样例语句)。 */
const HIDDEN = new Set(["sample"]);
/** 中等宽度的文本列。 */
const MEDIUM = new Set(["app", "client", "host", "wait", "table", "index", "name", "state", "blocking_state"]);

function isNumeric(col: string): boolean {
  return /(_bytes|_pct|_ms|_rows|_seconds|^seconds|^calls|^rows|^scans|^pages|^rows_\w+)$/.test(col) || col.endsWith("_pid") || col === "pid" || col === "id" || col.endsWith("_id");
}

/** 列标题:有译文用译文,没有就显示原键。 */
function colLabel(col: string): string {
  const key = `diag.col.${col}`;
  const s = t(key);
  return s === key || s.endsWith(key) ? col : s;
}

/** 单元格显示值:字节换算、百分号、毫秒。 */
export function cellText(col: string, v: string | null): string {
  if (v === null || v === "") return "—";
  const n = Number(v);
  if (col.endsWith("_bytes") && Number.isFinite(n)) return formatBytes(n);
  if (col.endsWith("_pct")) return `${v}%`;
  if (col.endsWith("_ms") && Number.isFinite(n)) return `${n.toLocaleString()} ms`;
  if (col.endsWith("seconds") && Number.isFinite(n)) return `${n.toLocaleString()} s`;
  return v;
}

function colWidth(col: string): string {
  if (WIDE.has(col)) return "w-[380px]";
  if (MEDIUM.has(col)) return "w-[150px]";
  return "w-[100px]";
}

/** 需要提醒的值(空闲事务、死行占比高):标成警示色。 */
function warn(col: string, v: string | null): boolean {
  if (v === null) return false;
  if (col === "state" || col === "blocking_state") return v.startsWith("idle in transaction");
  if (col === "dead_pct") return Number(v) >= 20;
  if (col === "message") return v !== "ok";
  return false;
}

const ACTION_ICON: Record<DiagAction, "circle-x" | "x" | "eraser" | "scan-search"> = {
  cancel: "circle-x",
  terminate: "x",
  kill_query: "circle-x",
  kill: "x",
  vacuum_table: "eraser",
  optimize_table: "eraser",
  vacuum: "eraser",
  analyze: "scan-search",
};

/** 待确认的操作。 */
interface Pending {
  action: DiagAction;
  args: string[];
  /** 相关语句(会话当前 SQL),在确认框里给个上下文 */
  context: string | null;
}

function Cards(props: { r: DiagResult }) {
  const row = () => props.r.rows[0] ?? [];
  return (
    <View testID="diag-cards" class="flex-row flex-wrap gap-2">
      <Each of={props.r.columns}>
        {(c, i) => (
          <View class="min-w-[150px] flex-1 gap-1 rounded-(--radius) border border-border bg-panel px-3 py-2">
            <Text class={`${FS.xs} text-muted-foreground`}>{colLabel(c())}</Text>
            <Text class={`${FS.base} font-bold text-foreground`}>{cellText(c(), row()[i()] ?? null)}</Text>
          </View>
        )}
      </Each>
    </View>
  );
}

/** 一行里的 SQL 能做的事:在编辑器打开、交给 AI 解读;慢 SQL 还能直接 Explain;
 *  未使用的索引生成 DROP INDEX(放进编辑器,执行时照常过危险语句审查)。 */
function SqlMenu(props: {
  connId: string;
  kind: DbKind;
  section: string;
  sql: string | null;
  /** 带真实值的样例语句(MySQL 8.0 的慢 SQL):有就用它,不用填参数 */
  sample?: string | null;
  index: string[] | null;
}) {
  const prepared = () => {
    if (props.sample?.trim()) return props.sample;
    const raw = props.sql ?? "";
    // 慢 SQL 是归一化过的语句:占位符换成命名参数,执行前弹框填值
    const r = props.section === "slow" ? namedPlaceholders(raw, props.kind) : { sql: raw, count: 0 };
    if (r.count > 0) toast.info(t("diag.placeholders_note"));
    return r.sql;
  };
  const dropIndex = () => {
    const [schema, table, index] = props.index ?? [];
    const stmt =
      props.kind === "mysql"
        ? `ALTER TABLE ${sqlNative.ident(table, "mysql")} DROP INDEX ${sqlNative.ident(index, "mysql")};`
        : `DROP INDEX CONCURRENTLY ${sqlNative.ident(schema, "pg")}.${sqlNative.ident(index, "pg")};`;
    openQueryOn(props.connId, `-- ${t("diag.drop_index_note")}\n${stmt}`);
  };
  return (
    <DropMenu trigger={<Btn testID="diag-sql-menu" small variant="ghost" icon="code-xml" label={t("diag.sql_menu")} />}>
      <If when={props.sql}>
        <MItem label={t("diag.open_in_editor")} onSelect={() => openQueryOn(props.connId, prepared())} />
        <If when={props.section === "slow"}>
          <MItem label={t("diag.explain")} onSelect={() => explainOn(props.connId, prepared())} />
        </If>
        <MItem
          label={t("diag.ask_ai")}
          onSelect={() => askAiReviewSql(props.sql ?? "", t(`diag.section.${props.section}`))}
        />
      </If>
      <If when={props.index}>
        <MItem label={t("diag.drop_index")} onSelect={dropIndex} />
      </If>
    </DropMenu>
  );
}

function Table(props: { r: DiagResult; section: DiagSection; connId: string; kind: DbKind; onAction: (p: Pending) => void }) {
  const idx = (col: string) => props.r.columns.indexOf(col);
  const shown = () => props.r.columns.filter((c) => !HIDDEN.has(c));
  const contextCol = () => ["blocking_query", "query"].map(idx).find((i) => i >= 0) ?? -1;
  /** 行里的 SQL 列(会话、锁等待、长事务、慢 SQL) */
  const sqlCol = () => ["query", "blocked_query"].map(idx).find((i) => i >= 0) ?? -1;
  const isUnusedIdx = () => props.section.key === "unused_indexes" && idx("index") >= 0 && idx("table") >= 0;
  const hasMenu = () => sqlCol() >= 0 || isUnusedIdx();
  const hasActions = () => props.section.rowActions.length > 0 || hasMenu();
  const actionsWidth = () => (props.section.rowActions.length > 0 && hasMenu() ? "w-[230px]" : "w-[150px]");
  return (
    <If when={props.r.rows.length > 0} else={<Hint>{t("diag.empty")}</Hint>}>
      <Scroll horizontal class="min-h-0">
        <View testID="diag-table" class="rounded-(--radius) border border-border">
          <View class="flex-row gap-3 border-b border-border bg-panel px-3 py-1.5">
            <If when={hasActions()}>
              <View class={actionsWidth()} />
            </If>
            <Each of={shown()}>
              {(c) => (
                <Text
                  class={`${FS.xs} ${colWidth(c())} font-bold text-muted-foreground line-clamp-1`}
                  classList={{ "text-right": isNumeric(c()) }}
                >
                  {colLabel(c())}
                </Text>
              )}
            </Each>
          </View>
          <Each of={props.r.rows}>
            {(row) => (
              <View class="flex-row items-start gap-3 border-b border-hairline px-3 py-1 hover:bg-list-hover">
                <If when={hasActions()}>
                  <View class={`${actionsWidth()} flex-row gap-1`}>
                    <Each of={props.section.rowActions}>
                      {(a) => (
                        <Btn
                          testID={`diag-${a()}`}
                          small
                          variant={a() === "terminate" || a() === "kill" ? "outline" : "ghost"}
                          icon={ACTION_ICON[a()]}
                          label={t(`diag.action.${a()}`)}
                          onPress={() =>
                            props.onAction({
                              action: a(),
                              args: props.section.targetColumns.map((c) => row()[idx(c)] ?? ""),
                              context: contextCol() >= 0 ? (row()[contextCol()] ?? null) : null,
                            })
                          }
                        />
                      )}
                    </Each>
                    <If when={hasMenu()}>
                      <SqlMenu
                        connId={props.connId}
                        kind={props.kind}
                        section={props.section.key}
                        sql={sqlCol() >= 0 ? (row()[sqlCol()] ?? null) : null}
                        sample={idx("sample") >= 0 ? (row()[idx("sample")] ?? null) : null}
                        index={
                          isUnusedIdx()
                            ? [row()[idx("schema")] ?? "", row()[idx("table")] ?? "", row()[idx("index")] ?? ""]
                            : null
                        }
                      />
                    </If>
                  </View>
                </If>
                <Each of={shown()}>
                  {(c) => (
                    <Text
                      class={`${FS.xs} ${colWidth(c())} ${WIDE.has(c()) ? `${MONO} line-clamp-3` : "line-clamp-1"} text-foreground`}
                      classList={{ "text-right": isNumeric(c()), [MONO]: isNumeric(c()), "text-warning": warn(c(), row()[idx(c())] ?? null) }}
                      onPress={WIDE.has(c()) && row()[idx(c())] ? () => void copy(row()[idx(c())] ?? "") : undefined}
                    >
                      {cellText(c(), row()[idx(c())] ?? null)}
                    </Text>
                  )}
                </Each>
              </View>
            )}
          </Each>
        </View>
      </Scroll>
    </If>
  );
}

async function copy(text: string) {
  await Clipboard.writeText(text);
  toast.success(t("diag.copied"));
}

function ConfirmDialog(props: { pending: Pending | null; busy: boolean; prod: boolean; onClose: () => void; onConfirm: () => void }) {
  const p = () => props.pending;
  const target = () => (p()?.args ?? []).join(".");
  return (
    <Dialog
      testID="diag-confirm"
      open={p() !== null}
      onOpenChange={(e) => !e.open && props.onClose()}
      closeOnEscape
      animation="fade"
      backdropClass="bg-overlay"
      class="m-auto w-[560px] max-w-[92%] gap-3 rounded-(--radius-lg) border border-border bg-popover p-5 shadow-lg"
    >
      <View class="flex-row items-center gap-2">
        <Icon name="triangle-alert" size={16} color="warning" />
        <Text class={`${FS.base} font-bold text-popover-foreground`}>
          {p() ? t(`diag.confirm.${p()!.action}.title`, { target: target() }) : ""}
        </Text>
      </View>
      <Text class={`${FS.sm} text-popover-foreground`}>
        {p() ? t(`diag.confirm.${p()!.action}.body`, { target: target() }) : ""}
      </Text>
      <If when={p()?.context}>
        {(ctx) => (
          <View class="max-h-[160px] rounded-(--radius) border border-border bg-editor px-3 py-2">
            <Text class={`${FS.xs} ${MONO} text-foreground line-clamp-6`}>{ctx()}</Text>
          </View>
        )}
      </If>
      <If when={props.prod}>
        <Text class={`${FS.xs} text-negative`}>{t("diag.confirm_prod")}</Text>
      </If>
      <View class="flex-row justify-end gap-2 pt-1">
        <Btn variant="outline" label={t("diag.cancel")} onPress={props.onClose} testID="diag-confirm-cancel" />
        <Btn
          variant="danger"
          disabled={props.busy}
          label={p() ? t(`diag.action.${p()!.action}`) : ""}
          onPress={props.onConfirm}
          testID="diag-confirm-ok"
        />
      </View>
    </Dialog>
  );
}

export function Diagnostics(props: { connId: string }) {
  const id = props.connId;
  const conn = () => conns.get(id);
  const kind = conn()?.kind ?? "pg";
  const sections = db.diagSections(kind);
  const active = signal(sections[0]?.key ?? "");
  const results = signal<Record<string, DiagResult>>({});
  const errors = signal<Record<string, string>>({});
  const loading = signal<string | null>(null);
  const auto = signal(false);
  const pending = signal<Pending | null>(null);
  const busy = signal(false);
  let alive = true;
  let timer: ReturnType<typeof setInterval> | undefined;
  cleanup(() => {
    alive = false;
    if (timer) clearInterval(timer);
  });

  const current = () => sections.find((s) => s.key === active());

  async function load(key: string) {
    if (!conns.isConnected(id)) {
      if (!conns.isLoading(id)) void loadTables(id);
      return;
    }
    loading.set(key);
    try {
      const r = await db.diagRun(kind, id, key);
      if (!alive) return;
      results.set({ ...results(), [key]: r });
      const { [key]: _, ...rest } = errors();
      errors.set(rest);
    } catch (e) {
      if (alive) errors.set({ ...errors(), [key]: errorText(e) });
    } finally {
      if (alive && loading() === key) loading.set(null);
    }
  }

  function select(key: string) {
    active.set(key);
    const s = sections.find((x) => x.key === key);
    if (s && !s.manual && !results()[key]) void load(key);
  }

  function setAuto(on: boolean) {
    auto.set(on);
    if (timer) clearInterval(timer);
    timer = undefined;
    if (on) {
      timer = setInterval(() => {
        const s = current();
        if (s && !s.manual && loading() === null && pending() === null) void load(s.key);
      }, AUTO_MS);
    }
  }

  async function confirm() {
    const p = pending();
    if (!p) return;
    busy.set(true);
    try {
      await db.diagAct(kind, id, p.action, p.args);
      toast.success(t(`diag.done.${p.action}`));
      pending.set(null);
      const s = current();
      if (s) void load(s.key);
    } catch (e) {
      toast.error(errorText(e));
    } finally {
      busy.set(false);
    }
  }

  // 连接还没建立(直接从侧栏打开诊断)时先连上,连上后加载当前分区;连接失败只试一次,之后点「刷新」重试
  let triedConnect = false;
  effect(() => {
    if (!conns.isConnected(id)) {
      if (!triedConnect && !conns.isLoading(id)) {
        triedConnect = true;
        void loadTables(id);
      }
      return;
    }
    untracked(() => {
      const s = current();
      if (s && !s.manual && !results()[s.key]) void load(s.key);
    });
  });

  return (
    <View testID="diagnostics" class="min-h-0 flex-1 bg-background">
      <View class="shrink-0 gap-3 px-5 pt-4">
        <View class="flex-row items-center gap-3">
          <Icon name="stethoscope" size={16} color="data" />
          <Text class={`${FS.base} font-bold text-foreground`}>{t("diag.heading", { name: conn()?.name ?? "" })}</Text>
          <If when={conn()}>{(c) => <EnvBadge env={c().env} />}</If>
          <View class="flex-1" />
          <Btn
            testID="diag-auto"
            small
            variant={auto() ? "primary" : "outline"}
            icon="rotate-ccw-clock"
            label={t("diag.auto", { secs: AUTO_MS / 1000 })}
            onPress={() => setAuto(!auto())}
          />
          <Btn
            testID="diag-refresh"
            small
            icon="refresh-cw"
            label={current()?.manual ? t("diag.run") : t("diag.refresh")}
            disabled={loading() !== null}
            onPress={() => void load(active())}
          />
        </View>
        <Scroll horizontal class="shrink-0">
          <View class="flex-row gap-1 border-b border-border">
            <Each of={sections}>
              {(s) => (
                <View
                  testID={`diag-tab-${s().key}`}
                  class="-mb-px border-b-2 px-3 py-1.5"
                  classList={{ "border-primary": active() === s().key, "border-transparent": active() !== s().key }}
                  onPress={() => select(s().key)}
                >
                  <Text
                    class={`${FS.sm}`}
                    classList={{
                      "font-bold text-foreground": active() === s().key,
                      "text-muted-foreground": active() !== s().key,
                    }}
                  >
                    {t(`diag.section.${s().key}`)}
                  </Text>
                </View>
              )}
            </Each>
          </View>
        </Scroll>
      </View>

      <Scroll class="min-h-0 flex-1" contentClass="gap-3 px-5 py-3">
        <If when={current()}>
          {(s) => (
            <View class="gap-3">
              <View class="flex-row items-center gap-2">
                <Hint class={`${FS.xs} flex-1`}>{t(`diag.hint.${kind}.${s().key}`)}</Hint>
                <Each of={s().dbActions}>
                  {(a) => (
                    <Btn
                      testID={`diag-${a()}`}
                      small
                      variant="outline"
                      icon={ACTION_ICON[a()]}
                      label={t(`diag.action.${a()}`)}
                      onPress={() => pending.set({ action: a(), args: [], context: null })}
                    />
                  )}
                </Each>
              </View>
              <If
                when={results()[s().key]}
                else={
                  <If
                    when={errors()[s().key]}
                    else={
                      <Hint>
                        {!conns.isConnected(id)
                          ? t("diag.connecting")
                          : s().manual && loading() !== s().key
                            ? t("diag.manual_hint")
                            : t("diag.loading")}
                      </Hint>
                    }
                  >
                    {(e) => <ErrorText text={e()} />}
                  </If>
                }
              >
                {(r) => (
                  <View class="gap-2">
                    <If when={errors()[s().key]}>{(e) => <ErrorText text={e()} />}</If>
                    <If
                      when={r().unavailable}
                      else={
                        <If when={s().layout === "cards"} else={<Table r={r()} section={s()} connId={id} kind={kind} onAction={(p) => pending.set(p)} />}>
                          <Cards r={r()} />
                        </If>
                      }
                    >
                      {(u) => (
                        <View testID="diag-unavailable" class="gap-1 rounded-(--radius) border border-border bg-panel px-3 py-2">
                          <View class="flex-row items-start gap-1.5">
                            <Icon name="info" size={13} color="warning" class="mt-0.5" />
                            <Text class={`${FS.sm} flex-1 text-foreground`}>{t(`diag.unavailable.${u().reason}`)}</Text>
                          </View>
                          <If when={u().detail}>
                            {(d) => <ErrorText mono size="xs" tone="text-muted-foreground" text={d()} />}
                          </If>
                        </View>
                      )}
                    </If>
                    <If when={s().layout === "table" && !r().unavailable && r().rows.length > 0}>
                      <Text class={`${FS.xs} text-fg-faint`}>{t("diag.row_count", { n: r().rows.length })}</Text>
                    </If>
                  </View>
                )}
              </If>
            </View>
          )}
        </If>
      </Scroll>

      <ConfirmDialog
        pending={pending()}
        busy={busy()}
        prod={conn()?.env === "prod"}
        onClose={() => pending.set(null)}
        onConfirm={() => void confirm()}
      />
    </View>
  );
}
