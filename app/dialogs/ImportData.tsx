// 导入 CSV / JSON 成 SQLite 表(只对 SQLite 开放):选文件 → 预览前几行与推断的列类型 →
// 改列名 / 类型 / 取舍、选新建表或追加到已有表 → 后台单事务导入,轮询进度,可取消(取消即回滚)。
// 解析与插入都在 core(sqlite/import_data.rs),文件流式读取。

import { cleanup, Scroll, signal, Text, View, type Signal } from "nsh";
import { t } from "nsh/i18n";
import { db, dialogsNative, errorText } from "../lib/native";
import type {
  ImportColType,
  ImportColumnSpec,
  ImportDelimiter,
  ImportEncoding,
  ImportPreview,
  ImportProgress,
  ImportQuote,
  ImportSource,
} from "../lib/types";
import { conns, loadTables, refreshTables } from "../store/conns";
import { formatBytes } from "../tab/RedisInsight";
import { Btn, DbIcon, IconButton, TextField, color } from "../ui/kit";
import { ErrorText } from "../ui/error";
import { FS, MONO } from "../ui/text";
import { CheckBox, Dropdown, Field, LoadingBtn, Segmented } from "./controls";

/** 轮询导入进度的间隔。 */
const POLL_MS = 200;
/** 列下方展示的样例行数。 */
const SAMPLE_ROWS = 8;
const TYPES: ImportColType[] = ["INTEGER", "REAL", "TEXT"];
const JSON_EXT = /\.(json|jsonl|ndjson)$/i;

interface Report {
  ok: boolean;
  text: string;
}

export function ImportDataView(props: { connId: string; running: Signal<boolean>; onClose: () => void }) {
  const sqliteConns = () => conns.list().filter((c) => c.kind === "sqlite");
  const initial = conns.get(props.connId);
  const connId = signal<string | null>(initial?.kind === "sqlite" ? initial.id : null);
  const current = () => sqliteConns().find((c) => c.id === connId());

  const path = signal("");
  const format = signal<"csv" | "json">("csv");
  const delimiter = signal<ImportDelimiter>("auto");
  const quote = signal<ImportQuote>("double");
  const encoding = signal<ImportEncoding>("utf8");
  const hasHeader = signal(true);

  const preview = signal<ImportPreview | null>(null);
  const previewError = signal<string | null>(null);
  const loadingPreview = signal(false);
  const columns = signal<ImportColumnSpec[]>([]);

  const mode = signal<"create" | "append">("create");
  const tableName = signal("");
  const appendTable = signal<string | null>(null);
  /** 追加目标表的列名(小写);null = 还没取到 */
  const tableCols = signal<Set<string> | null>(null);

  const progress = signal<ImportProgress | null>(null);
  const report = signal<Report | null>(null);

  let alive = true;
  let timer: ReturnType<typeof setInterval> | undefined;
  let debounce: ReturnType<typeof setTimeout> | undefined;
  let seq = 0;
  /** 表名跟随的文件:换了文件才用新文件名覆盖用户改过的表名 */
  let namedFor = "";
  cleanup(() => {
    alive = false;
    if (timer) clearInterval(timer);
    if (debounce) clearTimeout(debounce);
  });

  const running = () => props.running();
  const busy = () => running() || loadingPreview();

  function source(): ImportSource {
    return {
      path: path().trim(),
      format: format(),
      delimiter: delimiter(),
      quote: quote(),
      encoding: encoding(),
      hasHeader: hasHeader(),
    };
  }

  async function loadPreview() {
    const src = source();
    report.set(null);
    if (!src.path) {
      preview.set(null);
      previewError.set(null);
      columns.set([]);
      return;
    }
    const my = ++seq;
    loadingPreview.set(true);
    try {
      const p = await db.sqliteImportPreview(src);
      if (!alive || my !== seq) return;
      preview.set(p);
      previewError.set(null);
      columns.set(p.columns.map((c, i) => ({ index: i, key: c.key, name: c.name, type: c.type, include: true })));
      if (namedFor !== src.path) {
        namedFor = src.path;
        tableName.set(p.tableName);
      }
    } catch (e) {
      if (!alive || my !== seq) return;
      preview.set(null);
      columns.set([]);
      previewError.set(errorText(e));
    } finally {
      if (alive && my === seq) loadingPreview.set(false);
    }
  }

  /** 换文件:按扩展名选格式,再预览(手输路径时稍等输入停下)。 */
  function setPath(p: string, wait: boolean) {
    path.set(p);
    format.set(JSON_EXT.test(p.trim()) ? "json" : "csv");
    if (debounce) clearTimeout(debounce);
    if (wait) debounce = setTimeout(() => void loadPreview(), 350);
    else void loadPreview();
  }

  async function pickFile() {
    let picked: string | null;
    try {
      picked = await dialogsNative.pickFile(t("import_data.choose_file"), "CSV / JSON", [
        "csv",
        "tsv",
        "txt",
        "json",
        "jsonl",
        "ndjson",
      ]);
    } catch (e) {
      report.set({ ok: false, text: errorText(e) });
      return;
    }
    if (picked && alive) setPath(picked, false);
  }

  /** 改解析选项后重新预览(列的修改会按新结果重置)。 */
  function option<T>(sig: Signal<T>) {
    return (v: T) => {
      sig.set(v);
      void loadPreview();
    };
  }

  function updateColumn(i: number, patch: Partial<ImportColumnSpec>) {
    columns.update((cs) => cs.map((c, k) => (k === i ? { ...c, ...patch } : c)));
    report.set(null);
  }

  async function chooseAppendTable(name: string) {
    appendTable.set(name);
    tableCols.set(null);
    const id = connId();
    if (!id) return;
    try {
      const d = await db.tableDetail("sqlite", id, name);
      if (alive && appendTable() === name) tableCols.set(new Set(d.columns.map((c) => c.name.toLowerCase())));
    } catch (e) {
      if (alive) report.set({ ok: false, text: errorText(e) });
    }
  }

  function setMode(m: "create" | "append") {
    mode.set(m);
    report.set(null);
    const id = connId();
    if (m === "append" && id && !conns.tables(id)) void loadTables(id);
  }

  const included = () => columns().filter((c) => c.include);
  /** 追加模式下这一列在目标表里找不到(会被跳过) */
  const missing = (c: ImportColumnSpec) =>
    mode() === "append" && c.include && tableCols() !== null && !tableCols()!.has(c.name.trim().toLowerCase());
  const target = () => (mode() === "create" ? tableName().trim() : (appendTable() ?? ""));
  const canRun = () => !!current() && !!preview() && included().length > 0 && !!target() && !busy();

  function finish(p: ImportProgress) {
    if (timer) clearInterval(timer);
    timer = undefined;
    props.running.set(false);
    if (p.cancelled) report.set({ ok: false, text: t("import_data.cancelled") });
    else if (p.error) report.set({ ok: false, text: t("import.failed", { error: p.error }) });
    else {
      let text = t("import_data.done", { count: p.rows.toLocaleString(), table: target() });
      if (p.skipped.length) text += ` ${t("import_data.skipped", { columns: p.skipped.join(", ") })}`;
      report.set({ ok: true, text });
      const id = connId();
      if (id) void refreshTables(id);
    }
  }

  async function run() {
    const conn = current();
    if (!conn || !canRun()) return;
    props.running.set(true);
    report.set(null);
    progress.set(null);
    try {
      await db.connect(conn.kind, conn.id);
      await db.sqliteImportStart(conn.id, {
        source: source(),
        table: target(),
        mode: mode(),
        columns: columns().map((c) => ({ ...c, name: c.name.trim() })),
      });
    } catch (e) {
      props.running.set(false);
      if (alive) report.set({ ok: false, text: t("import.failed", { error: errorText(e) }) });
      return;
    }
    const poll = () => {
      if (!alive) return;
      const p = db.sqliteImportProgress(conn.id);
      if (!p) return;
      progress.set(p);
      if (p.done) finish(p);
    };
    timer = setInterval(poll, POLL_MS);
    poll();
  }

  const delimiterLabel = (d: ImportDelimiter) => t(`import_data.delimiter_${d}`);
  const summary = () => {
    const p = preview();
    if (!p) return "";
    const parts = [
      p.format === "json"
        ? p.jsonLines
          ? t("import_data.json_lines")
          : t("import_data.json_array")
        : `CSV · ${delimiterLabel(p.delimiter ?? "comma")}${delimiter() === "auto" ? ` (${t("import_data.detected")})` : ""}`,
      t("import_data.columns_n", { count: p.columns.length }),
      t("import_data.sampled", { count: p.sampled }),
      formatBytes(p.fileSize),
    ];
    return parts.join(" · ");
  };
  const percent = () => {
    const p = progress();
    if (!p || p.totalBytes <= 0) return 0;
    return Math.min(100, Math.round((p.bytes / p.totalBytes) * 100));
  };

  return (
    <View class="gap-3.5">
      <View class="flex-row gap-3">
        <Field label={t("import.target")} style={{ width: 220 }} class="">
          <Dropdown
            testID="import-data-target"
            items={sqliteConns().map((c) => ({ value: c.id, title: c.name, leading: () => <DbIcon kind={c.kind} size={14} /> }))}
            value={connId()}
            label={current()?.name ?? ""}
            placeholder={t("import_data.choose_sqlite")}
            accessibilityLabel={t("import.target")}
            disabled={running()}
            leading={current() ? <DbIcon kind={current()!.kind} size={16} /> : undefined}
            onChange={(id) => {
              connId.set(id);
              appendTable.set(null);
              tableCols.set(null);
              report.set(null);
              if (mode() === "append" && !conns.tables(id)) void loadTables(id);
            }}
          />
        </Field>
        <Field label={t("import_data.file")}>
          <View class="flex-row gap-2">
            <TextField
              testID="import-data-path"
              class="flex-1"
              value={path()}
              placeholder={t("import_data.file_placeholder")}
              disabled={running()}
              onChange={({ value }) => setPath(value, true)}
            />
            <IconButton
              icon="folder-open"
              tooltip={t("import_data.choose_file")}
              onPress={() => void pickFile()}
              color="foreground"
              class="h-8 w-8 border border-input"
            />
          </View>
        </Field>
      </View>

      <If when={format() === "csv"}>
        <View class="flex-row items-end gap-3">
          <Field label={t("import_data.delimiter")}>
            <Dropdown
              testID="import-data-delimiter"
              items={(["auto", "comma", "tab", "semicolon"] as ImportDelimiter[]).map((d) => ({ value: d, title: delimiterLabel(d) }))}
              value={delimiter()}
              label={delimiterLabel(delimiter())}
              accessibilityLabel={t("import_data.delimiter")}
              disabled={busy()}
              onChange={(v) => option(delimiter)(v as ImportDelimiter)}
            />
          </Field>
          <Field label={t("import_data.quote")}>
            <Dropdown
              testID="import-data-quote"
              items={(["double", "single", "none"] as ImportQuote[]).map((q) => ({ value: q, title: t(`import_data.quote_${q}`) }))}
              value={quote()}
              label={t(`import_data.quote_${quote()}`)}
              accessibilityLabel={t("import_data.quote")}
              disabled={busy()}
              onChange={(v) => option(quote)(v as ImportQuote)}
            />
          </Field>
          <Field label={t("import_data.encoding")}>
            <Dropdown
              testID="import-data-encoding"
              items={(["utf8", "gbk"] as ImportEncoding[]).map((e) => ({ value: e, title: t(`import_data.encoding_${e}`) }))}
              value={encoding()}
              label={t(`import_data.encoding_${encoding()}`)}
              accessibilityLabel={t("import_data.encoding")}
              disabled={busy()}
              onChange={(v) => option(encoding)(v as ImportEncoding)}
            />
          </Field>
          <View class="h-8 flex-row items-center gap-2">
            <CheckBox
              testID="import-data-header"
              checked={hasHeader()}
              label={t("import_data.has_header")}
              onChange={(v) => {
                if (!busy()) option(hasHeader)(v);
              }}
            />
            <Text class={`${FS.sm} text-foreground`}>{t("import_data.has_header")}</Text>
          </View>
        </View>
      </If>

      <View class="flex-row items-start gap-3">
        <Field label={t("import_data.into")} style={{ width: 220 }} class="">
          <Segmented
            testID="import-data-mode"
            options={[
              { value: "create", label: t("import_data.mode_create") },
              { value: "append", label: t("import_data.mode_append") },
            ]}
            value={mode()}
            onChange={(m) => {
              if (!running()) setMode(m);
            }}
          />
        </Field>
        <Field label={t("import_data.table")}>
          <If
            when={mode() === "create"}
            else={
              <Dropdown
                testID="import-data-append-table"
                items={(connId() ? (conns.tables(connId()!) ?? []) : []).map((n) => ({ value: n, title: n, icon: "table" }))}
                value={appendTable()}
                label={appendTable() ?? ""}
                placeholder={t("import_data.choose_table")}
                accessibilityLabel={t("import_data.table")}
                disabled={running() || !connId()}
                onChange={(n) => void chooseAppendTable(n)}
              />
            }
          >
            <TextField
              testID="import-data-table"
              value={tableName()}
              placeholder={t("import_data.table_placeholder")}
              disabled={running()}
              mono
              onChange={({ value }) => {
                tableName.set(value);
                report.set(null);
              }}
            />
          </If>
        </Field>
      </View>

      <View class="gap-1.5">
        <View class="flex-row items-center gap-2">
          <Text class={`${FS.xs} font-bold text-fg-faint`}>{t("import_data.columns")}</Text>
          <Text testID="import-data-summary" class={`${FS.xs} flex-1 text-fg-faint line-clamp-1`}>
            {loadingPreview() ? t("import_data.loading") : summary()}
          </Text>
        </View>
        <If when={preview()?.badEncoding}>
          <Text testID="import-data-encoding-hint" class={`${FS.xs}`} style={{ color: color("warning") }}>
            {t("import_data.bad_encoding")}
          </Text>
        </If>
        <View class="overflow-hidden rounded-(--radius-lg) border border-border bg-background" style={{ height: 300 }}>
          <If
            when={preview()}
            else={
              <View class="flex-1 items-center justify-center px-6">
                <If when={previewError()} else={<Text class={`${FS.sm} text-fg-faint`}>{t("import_data.empty")}</Text>}>
                  {(e) => <ErrorText testID="import-data-preview-error" text={e()} />}
                </If>
              </View>
            }
          >
            {(p) => (
              <Scroll horizontal class="flex-1" contentClass="flex-row">
                <Each of={columns()} key={(c) => c.index}>
                  {(c, i) => (
                    <View
                      class="w-[168px] border-r border-border"
                      classList={{ "opacity-50": !c().include }}
                      testID={`import-col-${i()}`}
                    >
                      <View class="gap-1.5 border-b border-border bg-panel p-2">
                        <View class="flex-row items-center gap-1.5">
                          <CheckBox
                            testID={`import-col-include-${i()}`}
                            checked={c().include}
                            label={t("import_data.include_column", { name: c().name })}
                            onChange={(v) => {
                              if (!running()) updateColumn(i(), { include: v });
                            }}
                          />
                          <TextField
                            testID={`import-col-name-${i()}`}
                            class="min-w-0 flex-1"
                            value={c().name}
                            mono
                            invalid={c().include && (!c().name.trim() || missing(c()))}
                            disabled={running()}
                            onChange={({ value }) => updateColumn(i(), { name: value })}
                          />
                        </View>
                        <Dropdown
                          testID={`import-col-type-${i()}`}
                          small
                          items={TYPES.map((ty) => ({ value: ty, title: ty }))}
                          value={c().type}
                          label={c().type}
                          accessibilityLabel={t("import_data.column_type", { name: c().name })}
                          disabled={running()}
                          onChange={(v) => updateColumn(i(), { type: v as ImportColType })}
                        />
                        <Text class={`${FS["2xs"]} line-clamp-1`} style={{ color: color(missing(c()) ? "warning" : "fg-faint") }}>
                          {missing(c())
                            ? t("import_data.not_in_table")
                            : c().key && c().key !== c().name
                              ? t("import_data.from_key", { key: c().key })
                              : " "}
                        </Text>
                      </View>
                      <Each of={p().rows.slice(0, SAMPLE_ROWS)}>
                        {(r) => (
                          <Text
                            class={`${FS.xs} ${MONO} h-6 px-2 pt-1 line-clamp-1`}
                            classList={{ "text-foreground": r()[c().index] != null, "text-fg-faint": r()[c().index] == null }}
                          >
                            {r()[c().index] ?? "NULL"}
                          </Text>
                        )}
                      </Each>
                    </View>
                  )}
                </Each>
              </Scroll>
            )}
          </If>
        </View>
      </View>

      <If when={running() ? (progress() ?? { rows: 0, bytes: 0, totalBytes: 0 }) : null}>
        {(p) => (
          <View class="gap-1.5" testID="import-data-progress">
            <View class="h-1.5 overflow-hidden rounded-full bg-raised">
              <View class="h-full rounded-full bg-primary" style={{ width: `${percent()}%` }} />
            </View>
            <Text class={`${FS.xs} text-muted-foreground`}>
              {t("import_data.progress", { rows: p().rows.toLocaleString(), percent: percent() })}
            </Text>
          </View>
        )}
      </If>

      <If when={report()}>
        {(r) => (
          <View class="rounded-(--radius-lg) px-3 py-2.5" style={{ backgroundColor: color(r().ok ? "selection" : "deleted") }}>
            <If when={r().ok} else={<ErrorText testID="import-data-report" text={r().text} />}>
              <Text testID="import-data-report" class={`${FS.sm} text-foreground`}>
                {r().text}
              </Text>
            </If>
          </View>
        )}
      </If>

      <View class="flex-row items-center justify-end gap-2.5">
        <Text class={`${FS.xs} flex-1 text-fg-faint`}>{t("import_data.single_tx")}</Text>
        <If
          when={running()}
          else={<Btn testID="import-close" variant="outline" label={t("import.close")} onPress={props.onClose} />}
        >
          <Btn
            testID="import-data-cancel"
            variant="outline"
            label={t("import_data.cancel")}
            onPress={() => {
              const id = connId();
              if (id) db.sqliteImportCancel(id);
            }}
          />
        </If>
        <LoadingBtn
          testID="import-data-run"
          variant="primary"
          loading={running()}
          label={running() ? t("import.importing") : t("import.start")}
          disabled={!canRun()}
          onPress={() => void run()}
        />
      </View>
    </View>
  );
}
