// 写操作记录标签页(每个连接一个):Datum 在这个连接上执行过的写操作,按天分组的时间线。
// 左边列表(筛选:来源、状态、文字、时间),右边选中条目的完整语句与改动前的行;
// 可复制、放进编辑器、从这里撤销(重启之后也行,撤销前核对现在的行),按筛选导出 Markdown / CSV。
// 写操作记录的内容只在本地,从不交给 AI。

import { cleanup, Clipboard, effect, List, Scroll, signal, Text, untracked, View } from "nsh";
import { AlertDialog, DataTable, Editor, ResizablePanel, ResizablePanelGroup, type DataTableCellClass } from "nsh/base";
import { t } from "nsh/i18n";
import { Dropdown } from "../dialogs/controls";
import { errorText, journal, shell } from "../lib/native";
import type { Env, JournalDetail, JournalFilter, JournalSource, JournalSummary, UndoPrep } from "../lib/types";
import { conns } from "../store/conns";
import { journalRev } from "../store/journal";
import { maskRowsOf } from "../store/privacy";
import { toast } from "../store/toast";
import { Btn, EnvBadge, Hint, Icon, SearchField, EmptyState, Loading, SectionLabel, type IconName } from "../ui/kit";
import { DropMenu, MItem } from "../ui/menu";
import { FS, MONO } from "../ui/text";
import { openQueryOn } from "./index";
import { checkJournalUndo, journalUndoPrep } from "./journalUndo";
import { runUndo } from "./model";
import { UndoConfirm } from "./UndoDialog";
import { ViewSwitch } from "./ui";

const SNAP_ROW = 22;
const SNAP_HEADER = 24;

/** 快照预览里值为 NULL 的格子(淡色显示)。 */
function nullCells(rows: (string | null)[][]): DataTableCellClass[] {
  const out: DataTableCellClass[] = [];
  rows.forEach((r, row) => r.forEach((v, column) => v === null && out.push({ row, column, class: "text-fg-faint" })));
  return out;
}

const SOURCES: JournalSource[] = ["editor", "ai", "grid", "undo", "import", "alter", "redis", "lua", "testdata"];

const SOURCE_ICON: Record<JournalSource, IconName> = {
  editor: "terminal",
  ai: "sparkles",
  grid: "table",
  undo: "undo-2",
  import: "import",
  alter: "pencil",
  redis: "db-redis",
  lua: "code-xml",
  testdata: "wand-sparkles",
};

type Range = "all" | "today" | "7d" | "30d";
type Status = "all" | "ok" | "error";

const DAY = 24 * 3600 * 1000;

function rangeFrom(r: Range, now = Date.now()): number | null {
  if (r === "all") return null;
  const start = new Date(now);
  start.setHours(0, 0, 0, 0);
  const today = start.getTime();
  return r === "today" ? today : r === "7d" ? today - 6 * DAY : today - 29 * DAY;
}

const pad = (n: number) => String(n).padStart(2, "0");
const dayKey = (ts: number) => {
  const d = new Date(ts);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
};
const clock = (ts: number) => {
  const d = new Date(ts);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
};

function dayLabel(key: string, now = Date.now()): string {
  if (key === dayKey(now)) return t("journal.today");
  if (key === dayKey(now - DAY)) return t("journal.yesterday");
  return key;
}

/** 多行 SQL 压成一行(列表里只看开头)。 */
const oneLine = (sql: string) => sql.split(/\s+/).filter(Boolean).join(" ");

const isEnv = (e: string): e is Env => e === "local" || e === "staging" || e === "prod";

type Item = { kind: "day"; key: string } | { kind: "entry"; e: JournalSummary };

export function JournalView(props: { connId: string }) {
  const id = props.connId;
  const conn = () => conns.get(id);
  const source = signal<JournalSource | "">("");
  const status = signal<Status>("all");
  const text = signal("");
  const range = signal<Range>("all");
  const entries = signal<JournalSummary[]>([]);
  const loaded = signal(false);
  const error = signal<string | null>(null);
  const selected = signal<string | null>(null);
  const detail = signal<JournalDetail | null>(null);
  const undo = signal<UndoPrep | null>(null);
  const undoOpen = signal(false);
  const busy = signal(false);
  let alive = true;
  cleanup(() => {
    alive = false;
  });

  const filter = (): JournalFilter => ({
    source: source() || null,
    status: status() === "all" ? null : (status() as "ok" | "error"),
    text: text().trim() || null,
    from: rangeFrom(range()),
  });

  let seq = 0;
  async function reload() {
    const n = ++seq;
    try {
      const list = await journal.list(id, filter());
      if (!alive || n !== seq) return;
      entries.set(list);
      error.set(null);
    } catch (e) {
      if (alive && n === seq) error.set(errorText(e));
    } finally {
      if (alive && n === seq) loaded.set(true);
    }
    const sel = selected();
    if (sel) void loadDetail(sel);
  }

  async function loadDetail(entryId: string) {
    try {
      const d = await journal.get(id, entryId);
      if (alive && selected() === entryId) detail.set(d);
    } catch (e) {
      if (alive) toast.error(errorText(e));
    }
  }

  // 筛选变了、或有新的写操作记下:重读(文字输入稍等一下再读)
  let timer: ReturnType<typeof setTimeout> | undefined;
  effect(() => {
    journalRev();
    filter();
    untracked(() => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => void reload(), loaded() ? 150 : 0);
    });
  });
  cleanup(() => timer && clearTimeout(timer));

  function select(entryId: string) {
    selected.set(entryId);
    detail.set(null);
    void loadDetail(entryId);
  }

  const items = (): Item[] => {
    const out: Item[] = [];
    let day = "";
    for (const e of entries()) {
      const k = dayKey(e.ts);
      if (k !== day) {
        out.push({ kind: "day", key: k });
        day = k;
      }
      out.push({ kind: "entry", e });
    }
    return out;
  };

  async function startUndo() {
    const d = detail();
    const c = conn();
    if (!d || !c) return;
    const u = await journalUndoPrep(id, d.entry.id);
    if (!u || !alive) return;
    undo.set(u);
    undoOpen.set(true);
    checkJournalUndo(c.kind, id, u, (next) => {
      if (alive && undo()?.journalId === next.journalId) undo.set(next);
    });
  }

  async function confirmUndo() {
    const u = undo();
    const c = conn();
    if (!u || !c || busy()) return;
    busy.set(true);
    const ok = await runUndo(c.kind, id, u);
    if (!alive) return;
    busy.set(false);
    undoOpen.set(false);
    if (ok) undo.set(null);
  }

  async function exportAs(format: "md" | "csv") {
    try {
      const body = await journal.export(id, filter(), format);
      const name = `${(conn()?.name ?? "journal").replace(/[^\w.-]+/g, "_")}_journal_${shell.fileTimestamp()}.${format}`;
      const path = await shell.saveText(name, body);
      if (path) toast.success(t("journal.exported"));
    } catch (e) {
      toast.error(errorText(e));
    }
  }

  async function clearAll() {
    const { action } = await AlertDialog.show({
      title: t("journal.clear_title", { name: conn()?.name ?? "" }),
      description: t("journal.clear_desc"),
      actions: [
        { id: "cancel", label: t("journal.cancel"), role: "cancel" },
        { id: "clear", label: t("journal.clear"), role: "destructive" },
      ],
      closeOnEscape: true,
    });
    if (action !== "clear") return;
    try {
      await journal.clear(id);
      selected.set(null);
      detail.set(null);
      toast.success(t("journal.cleared"));
      void reload();
    } catch (e) {
      toast.error(errorText(e));
    }
  }

  const sourceItems = () => [
    { value: "", title: t("journal.source_all") },
    ...SOURCES.map((s) => ({ value: s, title: t(`journal.source.${s}`), icon: SOURCE_ICON[s] })),
  ];
  const rangeItems = (): { value: Range; title: string }[] => [
    { value: "all", title: t("journal.range_all") },
    { value: "today", title: t("journal.range_today") },
    { value: "7d", title: t("journal.range_7d") },
    { value: "30d", title: t("journal.range_30d") },
  ];
  const statuses = (): { value: Status; label: string }[] => [
    { value: "all", label: t("journal.status_all") },
    { value: "ok", label: t("journal.status_ok") },
    { value: "error", label: t("journal.status_error") },
  ];
  const filtering = () => !!source() || status() !== "all" || !!text().trim() || range() !== "all";

  return (
    <View testID="journal-view" class="min-h-0 flex-1 bg-background">
      <View class="shrink-0 gap-3 px-5 pb-3 pt-4">
        <View class="flex-row items-center gap-3">
          <Icon name="scroll-text" size={16} color="data" />
          <Text class={`${FS.base} font-bold text-foreground`}>{t("journal.heading", { name: conn()?.name ?? "" })}</Text>
          <If when={conn()}>{(c) => <EnvBadge env={c().env} />}</If>
          <View class="flex-1" />
          <Btn small variant="outline" icon="refresh-cw" label={t("journal.refresh")} onPress={() => void reload()} />
          <DropMenu
            testID="journal-export"
            trigger={<Btn small variant="outline" icon="download" label={t("journal.export")} />}
          >
            <MItem label={t("journal.export_md")} onSelect={() => void exportAs("md")} />
            <MItem label={t("journal.export_csv")} onSelect={() => void exportAs("csv")} />
          </DropMenu>
          <Btn testID="journal-clear" small variant="ghost" icon="trash" label={t("journal.clear")} onPress={() => void clearAll()} />
        </View>
        <View class="flex-row items-center gap-2">
          <SearchField
            testID="journal-search"
            value={text()}
            onChange={(v) => text.set(v)}
            placeholder={t("journal.search")}
            class="min-w-0 flex-1"
          />
          <Dropdown
            testID="journal-source"
            small
            items={sourceItems()}
            value={source()}
            label={source() ? t(`journal.source.${source()}`) : t("journal.source_all")}
            onChange={(v) => source.set(v as JournalSource | "")}
            style={{ width: 150 }}
          />
          <ViewSwitch
            options={statuses().map((s) => ({ ...s, testID: `journal-status-${s.value}` }))}
            value={status()}
            onChange={(v) => status.set(v)}
          />
          <Dropdown
            testID="journal-range"
            small
            items={rangeItems()}
            value={range()}
            label={rangeItems().find((r) => r.value === range())?.title ?? ""}
            onChange={(v) => range.set(v as Range)}
            style={{ width: 140 }}
          />
        </View>
      </View>
      <View class="min-h-0 flex-1 border-t border-border">
        <If when={error()}>
          {(e) => <Text class={`${FS.sm} px-5 py-3 text-negative`}>{e()}</Text>}
        </If>
        <If
          when={entries().length > 0}
          else={
            <If when={loaded()}>
              <EmptyState
                testID="journal-empty"
                icon="scroll-text"
                title={filtering() ? t("journal.empty_filtered") : t("journal.empty")}
                hint={t("journal.empty_hint")}
              />
            </If>
          }
        >
          <ResizablePanelGroup axis="horizontal" class="flex-1" handleClass="bg-border">
            <ResizablePanel size={460} minSize={300}>
              <List
                testID="journal-list"
                each={items()}
                keyExtractor={(it) => (it.kind === "day" ? `d:${it.key}` : it.e.id)}
                estimatedItemSize={52}
                class="flex-1"
              >
                {(it) => (
                  <If
                    when={it().kind === "entry" ? (it() as { e: JournalSummary }).e : null}
                    else={
                      <View class="bg-panel px-4 py-1.5">
                        <SectionLabel>
                          {dayLabel((it() as { key: string }).key)}
                        </SectionLabel>
                      </View>
                    }
                  >
                    {(e) => <Row e={e()} selected={selected() === e().id} onPress={() => select(e().id)} />}
                  </If>
                )}
              </List>
            </ResizablePanel>
            <ResizablePanel minSize={320}>
              <If
                when={detail()}
                else={
                  <View class="flex-1 items-center justify-center p-6">
                    <If when={selected()} else={<Hint>{t("journal.pick")}</Hint>}>
                      <Loading label={t("journal.loading")} />
                    </If>
                  </View>
                }
              >
                {(d) => <DetailPane d={d()} connId={id} onUndo={() => void startUndo()} onSelect={select} />}
              </If>
            </ResizablePanel>
          </ResizablePanelGroup>
        </If>
      </View>
      <UndoConfirm
        open={undoOpen()}
        undo={undo()}
        busy={busy()}
        onClose={() => undoOpen.set(false)}
        onConfirm={() => void confirmUndo()}
      />
    </View>
  );
}

function StatusChip(props: { e: { ok: boolean; undoneBy: string | null; undoRows: number | null }; testID?: string }) {
  const label = () =>
    !props.e.ok
      ? t("journal.chip_failed")
      : props.e.undoneBy
        ? t("journal.chip_undone")
        : props.e.undoRows !== null
          ? t("journal.chip_undoable")
          : "";
  const tone = () => (!props.e.ok ? "text-negative" : props.e.undoneBy ? "text-muted-foreground" : "text-data");
  return (
    <If when={label()}>
      <Text testID={props.testID} class={`${FS["2xs"]} shrink-0 rounded-full border border-border px-1.5 font-bold ${tone()}`}>
        {label()}
      </Text>
    </If>
  );
}

function Row(props: { e: JournalSummary; selected: boolean; onPress: () => void }) {
  const e = () => props.e;
  return (
    <View
      testID={`journal-entry:${e().id}`}
      onPress={props.onPress}
      class="gap-1 border-b border-border px-4 py-2"
      classList={{ "bg-list-active": props.selected, "hover:bg-list-hover": !props.selected }}
    >
      <View class="flex-row items-center gap-2">
        <Icon name={SOURCE_ICON[e().source] ?? "terminal"} size={13} color={e().ok ? "muted-foreground" : "negative"} />
        <Text class={`${FS.xs} ${MONO} text-fg-faint`}>{clock(e().ts)}</Text>
        <If when={isEnv(e().env) ? (e().env as Env) : null}>{(env) => <EnvBadge env={env()} />}</If>
        <View class="flex-1" />
        <If when={e().affected !== null && e().ok}>
          <Text class={`${FS.xs} text-muted-foreground`}>{t("journal.rows", { n: e().affected ?? 0 })}</Text>
        </If>
        <StatusChip e={e()} testID={`journal-chip:${e().id}`} />
      </View>
      <Text class={`${FS.xs} ${MONO} text-foreground line-clamp-2`}>{oneLine(e().sql)}</Text>
    </View>
  );
}

function DetailPane(props: { d: JournalDetail; connId: string; onUndo: () => void; onSelect: (id: string) => void }) {
  const d = () => props.d;
  const e = () => props.d.entry;
  const canUndo = () => e().ok && !!e().undo && e().undo!.statements.length > 0 && !d().undoneBy;
  const meta = () => {
    const parts = [
      `${dayKey(e().ts)} ${clock(e().ts)}`,
      t(`journal.source.${e().source}`),
      e().ok ? t("journal.status_ok") : t("journal.status_error"),
      `${e().elapsedMs} ms`,
    ];
    if (e().affected !== null) parts.splice(3, 0, t("journal.rows", { n: e().affected ?? 0 }));
    return parts.join(" · ");
  };
  return (
    <Scroll testID="journal-detail" class="flex-1" contentClass="gap-3 p-4">
      <View class="gap-1">
        <Text class={`${FS.xs} text-muted-foreground`}>{meta()}</Text>
        <If when={e().database}>
          <Text class={`${FS.xs} ${MONO} text-fg-faint line-clamp-1`}>{e().database}</Text>
        </If>
      </View>
      <View class="flex-row flex-wrap items-center gap-2">
        <Btn
          testID="journal-copy"
          small
          variant="outline"
          icon="copy"
          label={t("journal.copy_sql")}
          onPress={() =>
            void Clipboard.writeText(e().sql).then(
              () => toast.success(t("journal.copied")),
              (err) => toast.error(errorText(err)),
            )
          }
        />
        <If when={e().kind !== "redis"}>
          <Btn testID="journal-open" small variant="outline" icon="square-play" label={t("journal.open_in_editor")} onPress={() => openQueryOn(props.connId, e().sql)} />
        </If>
        <If when={canUndo()}>
          <Btn testID="journal-undo" small variant="primary" icon="undo-2" label={t("journal.undo", { rows: e().undo?.rows ?? 0 })} onPress={props.onUndo} />
        </If>
      </View>
      <If when={d().undoneBy}>
        {(by) => (
          <View testID="journal-undone-note" class="flex-row items-center gap-1.5">
            <Icon name="circle-check" size={13} color="success" />
            <Text class={`${FS.xs} text-muted-foreground`}>{t("journal.undone_note")}</Text>
            <Btn small variant="ghost" label={t("journal.show_undo")} onPress={() => props.onSelect(by())} />
          </View>
        )}
      </If>
      <If when={e().undoes.length > 0}>
        <View class="flex-row items-center gap-1.5">
          <Icon name="undo-2" size={13} color="muted-foreground" />
          <Text class={`${FS.xs} text-muted-foreground`}>{t("journal.is_undo_of", { count: e().undoes.length })}</Text>
          <Btn small variant="ghost" label={t("journal.show_original")} onPress={() => props.onSelect(e().undoes[0])} />
        </View>
      </If>
      <If when={!canUndo() && e().ok && !d().undoneBy && e().undoReason}>
        {(r) => <Hint>{t("undo.unavailable", { reason: r() })}</Hint>}
      </If>
      <If when={e().error}>
        {(err) => <Text testID="journal-error" class={`${FS.xs} ${MONO} text-negative`}>{err()}</Text>}
      </If>
      <View class="overflow-hidden rounded-(--radius) border border-border">
        <Editor
          class="bg-editor"
          style={{ height: Math.min(16, Math.max(3, e().sql.split("\n").length + 1)) * 20 }}
          value={e().sql}
          readOnly
          lineNumbers={false}
          folding={false}
          softWrap
          activeLine={false}
          language={e().kind === "redis" ? "text" : "sql"}
          fontSize={12}
        />
      </View>
      <If when={e().truncated}>
        <Hint>{t("journal.truncated")}</Hint>
      </If>
      <Each of={e().undo?.snapshots ?? []}>
        {(s) => {
          // 改动前的数据按连接的遮罩规则展示(敏感列打码,演示模式下也安全)
          const rows = () => maskRowsOf(props.connId, s().table, s().columns, s().rows) as (string | null)[][];
          return (
          <View testID="journal-snapshot" class="gap-1.5">
            <SectionLabel>
              {s().delete ? t("journal.rows_deleted", { table: s().table }) : t("journal.rows_before", { table: s().table })}
            </SectionLabel>
            <DataTable
              testID="journal-snapshot-table"
              accessibilityLabel={s().table}
              style={{ height: SNAP_HEADER + Math.max(1, rows().length) * SNAP_ROW + 2 }}
              columns={s().columns.map((c, i) => ({ key: String(i), title: c, width: 122, minWidth: 60, maxWidth: 600 }))}
              data={rows().map((r) => r.map((v) => v ?? "NULL"))}
              cellClasses={nullCells(rows())}
              rowHeight={SNAP_ROW}
              headerHeight={SNAP_HEADER}
              fontSize={11}
              rowSelectable={false}
              columnSelectable={false}
              movableColumns={false}
              verticalScrollbar={false}
              classes={{
                root: "rounded-(--radius) border border-border bg-background",
                header: "border-b border-border bg-panel",
                headerCell: `px-2 ${FS.xs} ${MONO} font-bold text-muted-foreground`,
                cell: `px-2 ${MONO} text-foreground`,
              }}
            />
            <If when={s().total > s().rows.length}>
              <Text class={`${FS.xs} text-fg-faint`}>{t("journal.preview_of", { shown: s().rows.length, total: s().total })}</Text>
            </If>
          </View>
          );
        }}
      </Each>
    </Scroll>
  );
}
