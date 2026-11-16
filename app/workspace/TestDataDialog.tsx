// 生成测试数据(表右键「生成测试数据…」,PG / MySQL / SQLite):core 按列名、类型与约束自动挑生成器,
// 这里可逐列改生成器与参数(范围、空值比例、固定值、取值清单),也可让 AI 按表结构建议(只发结构)。
// 改动后自动预览实际插入的前 10 行(同一种子结果一致);插入在后台分批进行,可取消(回滚当前事务)。
// staging / prod 连接先过与危险语句一致的确认框;不超过 1000 行时也可复制成 INSERT 或在编辑器中打开。

import { cleanup, Clipboard, effect, Scroll, signal, Text, View } from "nsh";
import { DataTable, type DataTableColumn } from "nsh/base";
import { t } from "nsh/i18n";
import { DialogFrame, Dropdown } from "../dialogs/controls";
import { errorText, testData } from "../lib/native";
import type {
  RiskFinding,
  TdAnalysis,
  TdColumnMeta,
  TdColumnPlan,
  TdGenerator,
  TdLocale,
  TdPlan,
  TdPreview,
  TdProgress,
} from "../lib/types";
import { aiStore } from "../store/ai";
import { conns, refreshTables } from "../store/conns";
import { toast } from "../store/toast";
import { findTableTab } from "../store/workspace";
import { openQueryOn } from "../tab";
import { type AlterState, type ImpactState, modelOf, refresh, type RiskGate } from "../tab/model";
import { RiskGateDialog } from "../tab/RiskDialog";
import { ErrorText } from "../ui/error";
import { Btn, color, Icon, IconButton, SectionLabel, TextField, tip } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { closeTestData, showTestData, testDataTarget } from "./ops";

/** 与 core 一致的上限(testdata::plan)。 */
const MAX_ROWS = 1_000_000;
const SINGLE_TX_MAX = 50_000;
const COMMIT_EVERY = 10_000;
const SQL_MAX_ROWS = 1000;
const POLL_MS = 200;
const PREVIEW_DEBOUNCE_MS = 400;

/** 各生成器带哪些参数(数字参数与文本参数分开处理)。 */
const NUM_PARAMS: Record<string, (keyof TdGenerator)[]> = {
  integer: ["min", "max"],
  decimal: ["min", "max"],
  float: ["min", "max"],
  sequence: ["start", "step"],
  text: ["minLen", "maxLen"],
  bytes: ["len"],
};
const TEXT_PARAMS: Record<string, (keyof TdGenerator)[]> = {
  date: ["from", "to"],
  timestamp: ["from", "to"],
  fixed: ["value"],
};
const PARAM_LABEL: Record<string, string> = {
  min: "min",
  max: "max",
  start: "start",
  step: "step",
  minLen: "min_len",
  maxLen: "max_len",
  len: "len",
  from: "from",
  to: "to",
  value: "value",
};

interface Report {
  ok: boolean;
  text: string;
}

export function TestDataDialog() {
  return (
    <If when={testDataTarget()}>
      {(target) => (
        <Each of={[target()]} key={(x) => `${x.connId}\u0000${x.table}\u0000${x.key}`}>
          {(x) => <TestDataForm connId={x().connId} table={x().table} />}
        </Each>
      )}
    </If>
  );
}

function TestDataForm(props: { connId: string; table: string }) {
  const connId = props.connId;
  const table = props.table;
  const conn = conns.get(connId);
  const kind = conn?.kind ?? "sqlite";

  const analysis = signal<TdAnalysis | null>(null);
  const loadError = signal<string | null>(null);
  const rowsText = signal("100");
  const seedText = signal("");
  const locale = signal<TdLocale>("en");
  const columns = signal<TdColumnPlan[]>([]);
  /** 参数输入框的草稿(解析不了的数字保留原文并标红):`列\u0000参数` → 文本 */
  const drafts = signal<Record<string, string>>({});

  const preview = signal<TdPreview | null>(null);
  const previewError = signal<string | null>(null);
  const previewing = signal(false);
  const aiBusy = signal(false);
  const aiNote = signal<{ error: boolean; text: string } | null>(null);
  const running = signal(false);
  const progress = signal<TdProgress | null>(null);
  const report = signal<Report | null>(null);
  const gate = signal<RiskGate | null>(null);

  let alive = true;
  let timer: ReturnType<typeof setInterval> | undefined;
  let debounce: ReturnType<typeof setTimeout> | undefined;
  let seq = 0;
  cleanup(() => {
    alive = false;
    if (timer) clearInterval(timer);
    if (debounce) clearTimeout(debounce);
  });

  const meta = (name: string): TdColumnMeta | undefined => analysis()?.meta.columns.find((c) => c.name === name);
  const rows = () => {
    const n = Number(rowsText().trim().replace(/[,_\s]/g, ""));
    return Number.isInteger(n) && n >= 1 && n <= MAX_ROWS ? n : null;
  };
  const seed = () => {
    const s = seedText().trim();
    return /^\d{1,16}$/.test(s) && Number.isSafeInteger(Number(s)) ? Number(s) : null;
  };
  const draftsInvalid = () => Object.values(drafts()).some((v) => v.trim() === "" || !Number.isFinite(Number(v)));
  const plan = (): TdPlan | null => {
    const r = rows();
    const s = seed();
    if (r === null || s === null || !analysis() || draftsInvalid()) return null;
    return { table, rows: r, seed: s, locale: locale(), columns: columns() };
  };
  const busy = () => running() || aiBusy();

  async function load() {
    try {
      const a = await testData.analyze(kind, connId, table, locale());
      if (!alive) return;
      analysis.set(a);
      columns.set(a.plan.columns);
      seedText.set(String(a.plan.seed));
      loadError.set(null);
    } catch (e) {
      if (alive) loadError.set(errorText(e));
    }
  }
  void load();

  async function runPreview() {
    const p = plan();
    if (!p) return;
    const my = ++seq;
    previewing.set(true);
    try {
      const r = await testData.preview(kind, connId, { ...p, rows: Math.min(p.rows, 10) });
      if (!alive || my !== seq) return;
      preview.set(r);
      previewError.set(null);
    } catch (e) {
      if (!alive || my !== seq) return;
      preview.set(null);
      previewError.set(errorText(e));
    } finally {
      if (alive && my === seq) previewing.set(false);
    }
  }

  // 方案一变(列、种子、语言;行数不影响前 10 行)就稍等后重新预览
  effect(() => {
    columns();
    seedText();
    locale();
    drafts();
    if (!analysis()) return;
    if (debounce) clearTimeout(debounce);
    debounce = setTimeout(() => void runPreview(), PREVIEW_DEBOUNCE_MS);
  });

  function setColumn(name: string, patch: Partial<TdColumnPlan>) {
    columns.update((cs) => cs.map((c) => (c.name === name ? { ...c, ...patch } : c)));
    report.set(null);
  }

  function setKind(name: string, gk: string) {
    const m = meta(name);
    if (!m) return;
    const g = testData.generatorFor(m, gk);
    if (!g) return;
    drafts.update((d) => Object.fromEntries(Object.entries(d).filter(([k]) => !k.startsWith(`${name}\u0000`))));
    setColumn(name, { generator: g });
  }

  function setParam(name: string, key: keyof TdGenerator, raw: string, numeric: boolean) {
    const c = columns().find((x) => x.name === name);
    if (!c) return;
    const dk = `${name}\u0000${String(key)}`;
    if (numeric) {
      const n = Number(raw);
      if (raw.trim() !== "" && Number.isFinite(n)) {
        drafts.update((d) => Object.fromEntries(Object.entries(d).filter(([k]) => k !== dk)));
        setColumn(name, { generator: { ...c.generator, [key]: n } });
      } else {
        drafts.update((d) => ({ ...d, [dk]: raw }));
      }
      return;
    }
    if (key === "values") {
      const values = raw
        .split(",")
        .map((v) => v.trim())
        .filter((v) => v);
      setColumn(name, { generator: { ...c.generator, values } });
      return;
    }
    setColumn(name, { generator: { ...c.generator, [key]: raw } });
  }

  async function askAi() {
    if (busy()) return;
    aiBusy.set(true);
    aiNote.set(null);
    try {
      const s = await testData.aiSuggest(kind, connId, table);
      if (!alive) return;
      const by = new Map(s.columns.map((c) => [c.name, c]));
      columns.update((cs) => cs.map((c) => by.get(c.name) ?? c));
      drafts.set({});
      const parts = [t("testdata.ai_applied", { count: s.columns.length })];
      if (s.dropped.length) parts.push(t("testdata.ai_dropped", { items: s.dropped.join(", ") }));
      if (s.note) parts.push(s.note);
      aiNote.set({ error: false, text: parts.join(" · ") });
    } catch (e) {
      if (alive) aiNote.set({ error: true, text: errorText(e) });
    } finally {
      if (alive) aiBusy.set(false);
    }
  }

  async function copySql() {
    const p = plan();
    if (!p) return;
    try {
      const text = await testData.insertSql(kind, connId, p);
      await Clipboard.writeText(text);
      toast.success(t("testdata.copied"));
    } catch (e) {
      report.set({ ok: false, text: errorText(e) });
    }
  }

  async function openInEditor() {
    const p = plan();
    if (!p) return;
    try {
      const text = await testData.insertSql(kind, connId, p);
      closeTestData();
      openQueryOn(connId, text);
    } catch (e) {
      report.set({ ok: false, text: errorText(e) });
    }
  }

  function finish(p: TdProgress) {
    if (timer) clearInterval(timer);
    timer = undefined;
    running.set(false);
    if (p.cancelled) {
      report.set({
        ok: false,
        text: p.committed > 0 ? t("testdata.cancelled_partial", { count: p.committed.toLocaleString() }) : t("testdata.cancelled"),
      });
    } else if (p.error) {
      const kept = p.committed > 0 ? ` ${t("testdata.failed_partial", { count: p.committed.toLocaleString() })}` : "";
      report.set({ ok: false, text: p.error + kept });
    } else {
      report.set({ ok: true, text: t("testdata.done", { count: p.committed.toLocaleString(), table }) });
    }
    // 有行写进去了:刷新侧栏与已打开的这张表
    if (p.committed > 0) {
      if (conns.tables(connId)) void refreshTables(connId);
      const m = modelOf(findTableTab(connId, table));
      if (m) refresh(m);
    }
  }

  async function insert() {
    const p = plan();
    if (!p || busy()) return;
    running.set(true);
    report.set(null);
    progress.set(null);
    try {
      await testData.start(kind, connId, p);
    } catch (e) {
      running.set(false);
      if (alive) report.set({ ok: false, text: errorText(e) });
      return;
    }
    const poll = () => {
      if (!alive) return;
      const s = testData.progress(connId);
      if (!s) return;
      progress.set(s);
      if (s.done) finish(s);
    };
    timer = setInterval(poll, POLL_MS);
    poll();
  }

  /** 生成:staging / prod 先过与危险语句一致的确认框。 */
  function generate() {
    const p = plan();
    if (!p || busy()) return;
    const env = conns.get(connId)?.env ?? "local";
    if (env === "local") {
      void insert();
      return;
    }
    const written = p.columns.filter((c) => c.generator.kind !== "skip").map((c) => c.name);
    const finding: RiskFinding = {
      sql: `INSERT INTO ${table} (${written.join(", ")}) VALUES … -- ${p.rows.toLocaleString()} rows`,
      level: env === "prod" ? "danger" : "warning",
      kind: "INSERT",
      object: table,
    };
    gate.set({
      findings: [finding],
      targetLabel: conn?.name ?? "",
      database: conn ? (conn.kind === "sqlite" ? (conn.filePath ?? "") : conn.database) || null : null,
      relational: true,
      impacts: signal<ImpactState[]>([{ state: "none" }]),
      alters: signal<AlterState[]>([{ state: "none" }]),
      noImpact: true,
      note: t("testdata.risk_note", { count: p.rows.toLocaleString(), table }),
      onConfirm: () => void insert(),
    });
  }

  const percent = () => {
    const p = progress();
    return p && p.total > 0 ? Math.min(100, Math.round((p.rows / p.total) * 100)) : 0;
  };
  const kindItems = () =>
    (analysis()?.kinds ?? []).map((k) => ({ value: k, title: t(`testdata.kind.${k}`) }));

  return (
    <DialogFrame
      open
      onClose={() => {
        if (!running()) closeTestData();
      }}
      title={t("testdata.title", { table })}
      width={1000}
      closeOnBackdrop={false}
      testID="testdata-dialog"
    >
      <If
        when={analysis()}
        else={
          <View class="h-40 items-center justify-center">
            <If when={loadError()} else={<Text class={`${FS.sm} text-fg-faint`}>{t("testdata.loading")}</Text>}>
              {(e) => <ErrorText testID="testdata-load-error" text={e()} />}
            </If>
          </View>
        }
      >
        {(a) => (
          <View class="min-h-0 shrink gap-3.5">
            {/* 行数 · 种子 · 语言 · AI */}
            <View class="flex-row items-end gap-3">
              <View class="w-[140px] gap-2">
                <SectionLabel>{t("testdata.rows")}</SectionLabel>
                <TextField
                  testID="testdata-rows"
                  value={rowsText()}
                  mono
                  invalid={rows() === null}
                  disabled={running()}
                  onChange={({ value }) => {
                    rowsText.set(value);
                    report.set(null);
                  }}
                />
              </View>
              <View class="w-[220px] gap-2">
                <SectionLabel>{t("testdata.seed")}</SectionLabel>
                <View class="flex-row items-center gap-1.5">
                  <TextField
                    testID="testdata-seed"
                    class="min-w-0 flex-1"
                    value={seedText()}
                    mono
                    invalid={seed() === null}
                    disabled={running()}
                    onChange={({ value }) => seedText.set(value)}
                  />
                  <IconButton
                    icon="refresh-cw"
                    testID="testdata-new-seed"
                    tooltip={t("testdata.new_seed")}
                    disabled={running()}
                    onPress={() => seedText.set(String(testData.randomSeed()))}
                    class="h-8 w-8 border border-input"
                  />
                </View>
              </View>
              <View class="w-[150px] gap-2">
                <SectionLabel>{t("testdata.language")}</SectionLabel>
                <Dropdown
                  testID="testdata-locale"
                  items={[
                    { value: "en", title: t("testdata.locale_en") },
                    { value: "zh", title: t("testdata.locale_zh") },
                  ]}
                  value={locale()}
                  label={locale() === "zh" ? t("testdata.locale_zh") : t("testdata.locale_en")}
                  accessibilityLabel={t("testdata.language")}
                  disabled={running()}
                  onChange={(v) => locale.set(v as TdLocale)}
                />
              </View>
              <View class="flex-1" />
              <Btn
                testID="testdata-ai"
                variant="ai"
                icon={aiBusy() ? "loader-circle" : "wand-sparkles"}
                label={aiBusy() ? t("testdata.ai_busy") : t("testdata.ai_infer")}
                tooltip={aiStore.ready() ? t("testdata.ai_privacy") : t("testdata.ai_not_ready")}
                disabled={!aiStore.ready() || busy()}
                onPress={() => void askAi()}
              />
            </View>
            <If when={aiNote()}>
              {(n) => (
                <If
                  when={n().error}
                  else={
                    <Text testID="testdata-ai-note" class={`${FS.xs} text-muted-foreground`}>
                      {n().text}
                    </Text>
                  }
                >
                  <ErrorText testID="testdata-ai-note" size="xs" text={n().text} />
                </If>
              )}
            </If>

            {/* 父表还没有数据的外键 */}
            <Each of={a().parentEmpty}>
              {(pe) => (
                <View class="flex-row items-center gap-2 rounded-(--radius) border border-warning/40 bg-warning/10 px-2.5 py-1.5">
                  <Icon name="triangle-alert" size={13} color="warning" />
                  <Text testID="testdata-parent-empty" class={`${FS.xs} min-w-0 flex-1 text-foreground`}>
                    {t(pe().nullable ? "testdata.parent_empty_null" : "testdata.parent_empty", {
                      columns: pe().columns.join(", "),
                      parent: pe().parent,
                    })}
                  </Text>
                  <Btn
                    small
                    variant="outline"
                    label={t("testdata.generate_parent", { parent: pe().parent })}
                    disabled={running()}
                    onPress={() => showTestData(connId, pe().parent)}
                  />
                </View>
              )}
            </Each>

            {/* 各列的生成方式 */}
            <View class="overflow-hidden rounded-(--radius-lg) border border-border">
              <View class="flex-row items-center gap-3 border-b border-border bg-panel px-3 py-1.5">
                <SectionLabel class="w-[220px]">{t("testdata.col_column")}</SectionLabel>
                <SectionLabel class="w-[200px]">{t("testdata.col_generator")}</SectionLabel>
                <SectionLabel class="flex-1">{t("testdata.col_options")}</SectionLabel>
                <SectionLabel class="w-[64px]">{t("testdata.col_null")}</SectionLabel>
              </View>
              <Scroll class="shrink" style={{ maxHeight: 260 }} testID="testdata-columns">
                <Each of={columns()} key={(c) => c.name}>
                  {(c, i) => (
                    <ColumnRow
                      plan={c()}
                      index={i()}
                      meta={meta(c().name)}
                      fkTarget={(() => {
                        const m = meta(c().name);
                        const g = m?.fk ? a().meta.fks.find((f) => f.constraint === m.fk) : undefined;
                        return g ? `${g.refTable}(${g.refColumns.filter((x) => x).join(", ") || "PK"})` : "";
                      })()}
                      kinds={kindItems()}
                      drafts={drafts()}
                      disabled={running()}
                      onKind={(k) => setKind(c().name, k)}
                      onParam={(key, raw, numeric) => setParam(c().name, key, raw, numeric)}
                      onNull={(r) => setColumn(c().name, { nullRatio: r })}
                    />
                  )}
                </Each>
              </Scroll>
            </View>

            {/* 预览 */}
            <View class="gap-1.5">
              <View class="flex-row items-center gap-2">
                <SectionLabel>{t("testdata.preview")}</SectionLabel>
                <Text testID="testdata-preview-summary" class={`${FS.xs} flex-1 text-fg-faint`}>
                  {previewing()
                    ? t("testdata.preview_loading")
                    : preview()
                      ? t("testdata.preview_of", {
                          count: preview()!.rows.length,
                          rows: (rows() ?? 0).toLocaleString(),
                        })
                      : ""}
                </Text>
                <Btn
                  small
                  variant="ghost"
                  icon="refresh-cw"
                  testID="testdata-preview"
                  label={t("testdata.refresh")}
                  disabled={!plan() || previewing()}
                  onPress={() => void runPreview()}
                />
              </View>
              <View class="overflow-hidden rounded-(--radius-lg) border border-border bg-background" style={{ height: 200 }}>
                <If
                  when={preview()}
                  else={
                    <View class="flex-1 items-center justify-center px-6">
                      <If when={previewError()}>{(e) => <ErrorText testID="testdata-preview-error" text={e()} />}</If>
                    </View>
                  }
                >
                  {(pv) => <PreviewGrid preview={pv()} />}
                </If>
              </View>
            </View>

            <If when={running() ? (progress() ?? { rows: 0, total: rows() ?? 0 }) : null}>
              {(p) => (
                <View class="gap-1.5" testID="testdata-progress">
                  <View class="h-1.5 overflow-hidden rounded-full bg-raised">
                    <View class="h-full rounded-full bg-primary" style={{ width: `${percent()}%` }} />
                  </View>
                  <Text class={`${FS.xs} text-muted-foreground`}>
                    {t("testdata.progress", {
                      rows: p().rows.toLocaleString(),
                      total: p().total.toLocaleString(),
                      percent: percent(),
                    })}
                  </Text>
                </View>
              )}
            </If>

            <If when={report()}>
              {(r) => (
                <View class="rounded-(--radius-lg) px-3 py-2.5" style={{ backgroundColor: color(r().ok ? "selection" : "deleted") }}>
                  <If when={r().ok} else={<ErrorText testID="testdata-report" text={r().text} />}>
                    <Text testID="testdata-report" class={`${FS.sm} text-foreground`}>
                      {r().text}
                    </Text>
                  </If>
                </View>
              )}
            </If>

            <View class="flex-row items-center justify-end gap-2.5">
              <Text class={`${FS.xs} flex-1 text-fg-faint`}>
                {(rows() ?? 0) > SINGLE_TX_MAX
                  ? t("testdata.batched", { max: SINGLE_TX_MAX.toLocaleString(), every: COMMIT_EVERY.toLocaleString() })
                  : t("testdata.single_tx")}
              </Text>
              <If
                when={running()}
                else={<Btn testID="testdata-close" variant="outline" label={t("testdata.close")} onPress={closeTestData} />}
              >
                <Btn testID="testdata-cancel" variant="outline" label={t("testdata.cancel")} onPress={() => testData.cancel(connId)} />
              </If>
              <Btn
                testID="testdata-copy-sql"
                variant="outline"
                icon="copy"
                label={t("testdata.copy_sql")}
                tooltip={t("testdata.sql_limit", { max: SQL_MAX_ROWS.toLocaleString() })}
                disabled={!plan() || busy() || (rows() ?? 0) > SQL_MAX_ROWS}
                onPress={() => void copySql()}
              />
              <Btn
                testID="testdata-open-editor"
                variant="outline"
                icon="square-play"
                label={t("testdata.open_editor")}
                tooltip={t("testdata.sql_limit", { max: SQL_MAX_ROWS.toLocaleString() })}
                disabled={!plan() || busy() || (rows() ?? 0) > SQL_MAX_ROWS}
                onPress={() => void openInEditor()}
              />
              <Btn
                testID="testdata-run"
                variant="primary"
                icon={running() ? "loader-circle" : "play"}
                label={running() ? t("testdata.generating") : t("testdata.generate", { count: (rows() ?? 0).toLocaleString() })}
                disabled={!plan() || busy()}
                onPress={generate}
              />
            </View>
          </View>
        )}
      </If>
      <RiskGateDialog
        gate={gate()}
        onCancel={() => gate.set(null)}
        onConfirm={() => {
          const g = gate();
          gate.set(null);
          g?.onConfirm();
        }}
      />
    </DialogFrame>
  );
}

/** 一列:列名与约束、生成方式、参数、空值比例。 */
function ColumnRow(props: {
  plan: TdColumnPlan;
  index: number;
  meta: TdColumnMeta | undefined;
  fkTarget: string;
  kinds: { value: string; title: string }[];
  drafts: Record<string, string>;
  disabled: boolean;
  onKind: (kind: string) => void;
  onParam: (key: keyof TdGenerator, raw: string, numeric: boolean) => void;
  onNull: (ratio: number) => void;
}) {
  const g = () => props.plan.generator;
  const m = () => props.meta;
  const draft = (key: keyof TdGenerator) => props.drafts[`${props.plan.name}\u0000${String(key)}`];
  const numText = (key: keyof TdGenerator) => draft(key) ?? String(g()[key] ?? "");
  const badges = () => {
    const x = m();
    if (!x) return [];
    const out: string[] = [];
    if (x.primary) out.push(t("testdata.badge.pk"));
    else if (x.unique) out.push(t("testdata.badge.unique"));
    if (!x.nullable && !x.primary) out.push(t("testdata.badge.not_null"));
    return out;
  };
  const skipNote = () => {
    const x = m();
    if (x?.auto) return t(`testdata.auto.${x.auto}`);
    return x?.hasDefault ? t("testdata.uses_default") : t("testdata.left_null");
  };
  const nullable = () => !!m()?.nullable && !m()?.unique && !["skip", "null", "foreignKey"].includes(g().kind);
  const nullText = signal(String(Math.round(props.plan.nullRatio * 100)));
  return (
    <View
      testID={`testdata-col-${props.index}`}
      class="min-h-[44px] flex-row items-center gap-3 border-b border-border px-3 py-1.5"
      classList={{ "opacity-60": g().kind === "skip" }}
    >
      <View class="w-[220px] gap-0.5">
        <View class="flex-row items-center gap-1.5">
          <Text class={`${FS.sm} ${MONO} min-w-0 shrink font-bold text-foreground line-clamp-1`}>{props.plan.name}</Text>
          <Each of={badges()}>
            {(b) => (
              <View class="rounded-(--radius-sm) border border-border px-1">
                <Text class={`${FS["2xs"]} font-bold text-muted-foreground`}>{b()}</Text>
              </View>
            )}
          </Each>
        </View>
        <Text class={`${FS.xs} ${MONO} text-fg-faint line-clamp-1`}>{m()?.dataType ?? ""}</Text>
      </View>
      <Dropdown
        testID={`testdata-gen-${props.index}`}
        small
        style={{ width: 200 }}
        items={props.kinds}
        value={g().kind}
        label={t(`testdata.kind.${g().kind}`)}
        accessibilityLabel={t("testdata.generator_label", { column: props.plan.name })}
        disabled={props.disabled || !!m()?.auto}
        onChange={props.onKind}
      />
      <View class="min-w-0 flex-1 flex-row items-center gap-2">
        <Each of={NUM_PARAMS[g().kind] ?? []}>
          {(key) => (
            <View class="flex-row items-center gap-1">
              <Text class={`${FS.xs} text-muted-foreground`}>{t(`testdata.param.${PARAM_LABEL[key()]}`)}</Text>
              <TextField
                testID={`testdata-param-${props.index}-${String(key())}`}
                class="h-7 w-[92px]"
                mono
                value={numText(key())}
                invalid={draft(key()) !== undefined}
                disabled={props.disabled}
                onChange={({ value }) => props.onParam(key(), value, true)}
              />
            </View>
          )}
        </Each>
        <Each of={TEXT_PARAMS[g().kind] ?? []}>
          {(key) => (
            <View class="min-w-0 flex-1 flex-row items-center gap-1">
              <Text class={`${FS.xs} text-muted-foreground`}>{t(`testdata.param.${PARAM_LABEL[key()]}`)}</Text>
              <TextField
                testID={`testdata-param-${props.index}-${String(key())}`}
                class="h-7 min-w-0 flex-1"
                mono
                value={String(g()[key()] ?? "")}
                disabled={props.disabled}
                onChange={({ value }) => props.onParam(key(), value, false)}
              />
            </View>
          )}
        </Each>
        <If when={g().kind === "pickList"}>
          <TextField
            testID={`testdata-param-${props.index}-values`}
            class="h-7 min-w-0 flex-1"
            mono
            placeholder={t("testdata.param.values")}
            value={(g().values ?? []).join(", ")}
            disabled={props.disabled}
            onChange={({ value }) => props.onParam("values", value, false)}
          />
        </If>
        <If when={g().kind === "foreignKey"}>
          <Text class={`${FS.xs} ${MONO} text-muted-foreground line-clamp-1`}>{t("testdata.fk_from", { target: props.fkTarget })}</Text>
        </If>
        <If when={g().kind === "skip"}>
          <Text class={`${FS.xs} text-fg-faint line-clamp-1`}>{skipNote()}</Text>
        </If>
      </View>
      <View class="w-[64px]">
        <If when={nullable()}>
          <View tooltip={tip(t("testdata.null_ratio_label", { column: props.plan.name }))}>
            <TextField
              testID={`testdata-null-${props.index}`}
              class="h-7"
              mono
              value={nullText()}
              invalid={!/^\d{1,3}$/.test(nullText().trim()) || Number(nullText()) > 100}
              disabled={props.disabled}
              onChange={({ value }) => {
                nullText.set(value);
                const n = Number(value.trim());
                if (/^\d{1,3}$/.test(value.trim()) && n <= 100) props.onNull(n / 100);
              }}
            />
          </View>
        </If>
      </View>
    </View>
  );
}

/** 预览表格(前 10 行,单元格单行显示)。 */
function PreviewGrid(props: { preview: TdPreview }) {
  const columns = (): DataTableColumn[] =>
    props.preview.columns.map((name, i) => ({
      key: String(i),
      title: name,
      width: 150,
      minWidth: 60,
      maxWidth: 800,
      movable: false,
    }));
  return (
    <DataTable
      testID="testdata-preview-grid"
      class="flex-1"
      columns={columns()}
      data={props.preview.rows}
      rowHeight={30}
      headerHeight={32}
      stripe
      rowSelectable={false}
      columnSelectable={false}
      cellSelectable={false}
      movableColumns={false}
      renderCell={(c) => {
        const v = props.preview.rows[c.row]?.[c.column] ?? null;
        return (
          <Text
            class={`${FS.xs} ${MONO} px-2 line-clamp-1`}
            classList={{ "text-foreground": v !== null, "text-fg-faint": v === null }}
          >
            {v === null ? "NULL" : v.replace(/[\r\n]/g, " ")}
          </Text>
        );
      }}
      renderHeaderCell={(h) => (
        <Text class={`${FS.xs} ${MONO} px-2 font-bold text-muted-foreground line-clamp-1`}>
          {props.preview.columns[h.column] ?? ""}
        </Text>
      )}
      classes={{
        root: "bg-background",
        header: "border-b border-border bg-panel",
        headerCell: "px-0",
        row: "border-b border-border",
        rowOdd: "bg-list-even",
        cell: "px-0",
      }}
    />
  );
}
