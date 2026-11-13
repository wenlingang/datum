// SQLite 改表结构表单:列出当前列,可改列名 / 类型 / 非空 / 默认值 / 主键、删列、加列、调整顺序,增删外键。
// 每次改动后由 core 生成脚本预览(能用 ALTER TABLE 完成的直接 ALTER,否则按官方 12 步重建表);
// 执行前照常走危险语句审查,执行时 core 先核对脚本与预览一致,再在一个事务里执行,失败整体回滚。

import { Clipboard, Scroll, signal, Text, View } from "nsh";
import { Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { CheckBox, DialogFrame, Dropdown } from "../dialogs/controls";
import { db, errorText, sql as sqlText } from "../lib/native";
import type { AlterFk, AlterOutcome, AlterPlan, AlterScript, RiskFinding } from "../lib/types";
import { conns, refreshTables } from "../store/conns";
import { toast } from "../store/toast";
import { closeTab, findTableTab } from "../store/workspace";
import { openTable } from "../tab";
import { type AlterState, type ImpactState, modelOf, reloadAfterSchemaChange, type RiskGate } from "../tab/model";
import { RiskGateDialog } from "../tab/RiskDialog";
import { ErrorText } from "../ui/error";
import { Btn, Hint, Icon, IconButton, SectionLabel, TextField, tip } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { alterTarget, closeAlterTable } from "./ops";

/** 表单里的一列;`key` 只用于列表复用,`orig` 对应原表列(新列为 null)。 */
interface Row {
  key: number;
  orig: string | null;
  name: string;
  dataType: string;
  notNull: boolean;
  /** 空串表示没有默认值 */
  def: string;
  pk: boolean;
  generated: boolean;
}

const FK_ACTIONS = ["NO ACTION", "CASCADE", "SET NULL", "SET DEFAULT", "RESTRICT"];

/** 逗号分隔的列名 → 数组(去空白、去空项)。 */
const splitCols = (s: string) =>
  s
    .split(",")
    .map((x) => x.trim())
    .filter((x) => x);

export function AlterTableDialog() {
  return (
    <If when={alterTarget()}>
      {(target) => (
        <Each of={[target()]} key={(x) => `${x.connId}\u0000${x.table}`}>
          {(x) => <AlterForm connId={x().connId} table={x().table} />}
        </Each>
      )}
    </If>
  );
}

function AlterForm(props: { connId: string; table: string }) {
  const connId = props.connId;
  let seq = 0;
  const loading = signal(true);
  const loadError = signal<string | null>(null);
  const newName = signal(props.table);
  const rows = signal<Row[]>([]);
  const fks = signal<AlterFk[]>([]);
  // 加外键的草稿
  const fkCols = signal("");
  const fkTable = signal("");
  const fkRefCols = signal("");
  const fkDelete = signal("NO ACTION");

  const script = signal<AlterScript | null>(null);
  const previewError = signal<string | null>(null);
  const busy = signal(false);
  const applyError = signal<string | null>(null);
  const violations = signal<AlterOutcome["violations"]>([]);
  const gate = signal<RiskGate | null>(null);

  const plan = (): AlterPlan => ({
    table: props.table,
    newName: newName().trim(),
    columns: rows().map((r) => ({
      orig: r.orig,
      name: r.name.trim(),
      dataType: r.dataType.trim(),
      notNull: r.notNull,
      default: r.def.trim() ? r.def.trim() : null,
      pk: r.pk,
      generated: r.generated,
    })),
    fks: fks(),
  });

  void (async () => {
    try {
      const snap = await db.sqliteAlterLoad(connId, props.table);
      rows.set(
        snap.columns.map((c) => ({
          key: seq++,
          orig: c.orig ?? c.name,
          name: c.name,
          dataType: c.dataType,
          notNull: c.notNull,
          def: c.default ?? "",
          pk: c.pk,
          generated: c.generated,
        })),
      );
      fks.set(snap.fks.map((f) => ({ ...f, onDelete: f.onDelete.toUpperCase(), onUpdate: f.onUpdate.toUpperCase() })));
      loading.set(false);
      schedulePreview();
    } catch (e) {
      loadError.set(errorText(e));
      loading.set(false);
    }
  })();

  // 改动后稍等再生成预览:连续打字只算最后一次;过期的结果用递增的 token 丢弃
  let timer: ReturnType<typeof setTimeout> | undefined;
  let token = 0;
  function schedulePreview() {
    if (timer) clearTimeout(timer);
    applyError.set(null);
    violations.set([]);
    timer = setTimeout(() => void preview(), 200);
  }
  async function preview() {
    const my = ++token;
    try {
      const s = await db.sqliteAlterPreview(connId, plan());
      if (my !== token) return;
      script.set(s);
      previewError.set(null);
    } catch (e) {
      if (my !== token) return;
      script.set(null);
      previewError.set(errorText(e));
    }
  }

  const update = (key: number, patch: Partial<Row>) => {
    const before = rows().find((r) => r.key === key);
    rows.update((list) => list.map((r) => (r.key === key ? { ...r, ...patch } : r)));
    // 列改名:外键里引用这一列的地方跟着改(自引用外键的被引列也是)
    if (before && patch.name !== undefined && patch.name !== before.name) {
      const from = before.name;
      const to = patch.name;
      const self = (f: AlterFk) => f.refTable === props.table || f.refTable === newName();
      fks.update((list) =>
        list.map((f) => ({
          ...f,
          columns: f.columns.map((c) => (c === from ? to : c)),
          refColumns: self(f) ? f.refColumns.map((c) => (c === from ? to : c)) : f.refColumns,
        })),
      );
    }
    schedulePreview();
  };

  const move = (key: number, delta: number) => {
    rows.update((list) => {
      const i = list.findIndex((r) => r.key === key);
      const j = i + delta;
      if (i < 0 || j < 0 || j >= list.length) return list;
      const next = list.slice();
      [next[i], next[j]] = [next[j], next[i]];
      return next;
    });
    schedulePreview();
  };

  const remove = (key: number) => {
    const row = rows().find((r) => r.key === key);
    rows.update((list) => list.filter((r) => r.key !== key));
    // 删掉的列上的外键一起去掉,否则生成器会报「外键列不存在」
    if (row) fks.update((list) => list.filter((f) => !f.columns.includes(row.name)));
    schedulePreview();
  };

  const addColumn = () => {
    rows.update((list) => [
      ...list,
      { key: seq++, orig: null, name: `column_${list.length + 1}`, dataType: "TEXT", notNull: false, def: "", pk: false, generated: false },
    ]);
    schedulePreview();
  };

  const addFk = () => {
    const columns = splitCols(fkCols());
    if (columns.length === 0 || !fkTable().trim()) return;
    fks.update((list) => [
      ...list,
      { columns, refTable: fkTable().trim(), refColumns: splitCols(fkRefCols()), onDelete: fkDelete(), onUpdate: "NO ACTION" },
    ]);
    fkCols.set("");
    fkTable.set("");
    fkRefCols.set("");
    fkDelete.set("NO ACTION");
    schedulePreview();
  };

  const removeFk = (i: number) => {
    fks.update((list) => list.filter((_, j) => j !== i));
    schedulePreview();
  };

  const copyScript = async () => {
    const s = script();
    if (!s) return;
    await Clipboard.writeText(s.sql);
    toast.success(t("alter.copied"));
  };

  /** 执行:先过危险语句审查(与编辑器执行一致:有命中就弹确认框),确认后再执行。 */
  const execute = () => {
    const s = script();
    if (!s || s.mode === "noop" || busy()) return;
    const conn = conns.get(connId);
    const strict = conn?.env === "prod";
    // 逐条审查:触发器体里有分号,整段交给切分器会切错
    const findings: RiskFinding[] = s.statements.flatMap((stmt) => sqlText.analyze(stmt, strict));
    if (findings.length === 0) {
      void apply();
      return;
    }
    gate.set({
      findings,
      targetLabel: conn?.name ?? "",
      database: conn?.filePath ? conn.filePath : null,
      relational: true,
      impacts: signal<ImpactState[]>(findings.map(() => ({ state: "none" as const }))),
      alters: signal<AlterState[]>(findings.map(() => ({ state: "none" as const }))),
      noImpact: true,
      note: s.mode === "rebuild" ? t("alter.rebuild_note") : undefined,
      onConfirm: () => void apply(),
    });
  };

  async function apply() {
    const s = script();
    if (!s) return;
    busy.set(true);
    applyError.set(null);
    violations.set([]);
    try {
      const out = await db.sqliteAlterApply(connId, plan(), s.sql);
      if (out.applied) {
        const name = newName().trim();
        toast.success(t("alter.done", { name }));
        if (conns.tables(connId)) void refreshTables(connId);
        // 已打开的该表的表浏览 tab:改了表名就关旧开新(同 Redis 改名);否则按新的列重新加载
        const opened = findTableTab(connId, props.table);
        if (opened) {
          if (name !== props.table) {
            closeTab(opened.id);
            openTable(connId, name);
          } else {
            const m = modelOf(opened);
            if (m) reloadAfterSchemaChange(m);
          }
        }
        closeAlterTable();
        return;
      }
      violations.set(out.violations);
    } catch (e) {
      applyError.set(errorText(e));
    } finally {
      busy.set(false);
    }
  }

  const modeLabel = () => {
    const s = script();
    if (!s) return "";
    return s.mode === "noop" ? t("alter.mode_noop") : s.mode === "simple" ? t("alter.mode_simple") : t("alter.mode_rebuild");
  };

  return (
    <DialogFrame
      open
      onClose={closeAlterTable}
      title={t("alter.title", { name: props.table })}
      width={960}
      closeOnBackdrop={false}
      testID="alter-dialog"
    >
      <If when={!loading()} else={<Hint>{t("alter.loading")}</Hint>}>
        <If when={!loadError()} else={<ErrorText text={loadError() ?? ""} />}>
          <Scroll class="min-h-0 shrink" style={{ maxHeight: 380 }} contentClass="gap-3 pr-1">
            <View class="flex-row items-center gap-3">
              <SectionLabel>{t("alter.table_name")}</SectionLabel>
              <TextField
                value={newName()}
                onChange={({ value }) => {
                  newName.set(value);
                  schedulePreview();
                }}
                autoCorrect={false}
                testID="alter-table-name"
                class="font-mono w-[280px]"
              />
            </View>

            <View class="gap-1">
              <View class="flex-row items-center gap-2 px-1">
                <Text class={`${FS["2xs"]} w-[200px] font-bold text-muted-foreground`}>{t("alter.col_name")}</Text>
                <Text class={`${FS["2xs"]} w-[150px] font-bold text-muted-foreground`}>{t("alter.col_type")}</Text>
                <Text class={`${FS["2xs"]} w-[64px] text-center font-bold text-muted-foreground`}>{t("alter.col_not_null")}</Text>
                <Text class={`${FS["2xs"]} w-[40px] text-center font-bold text-muted-foreground`}>{t("alter.col_pk")}</Text>
                <View class="flex-1 flex-row" tooltip={tip(t("alter.default_tip"))}>
                  <Text class={`${FS["2xs"]} font-bold text-muted-foreground`}>{t("alter.col_default")}</Text>
                </View>
                <View class="w-[88px]" />
              </View>
              <Each of={rows()} key={(r) => r.key}>
                {(r, i) => (
                  <View testID={`alter-row:${i()}`} class="flex-row items-center gap-2 rounded-(--radius) px-1 py-0.5">
                    <View class="w-[200px] flex-row items-center gap-1.5">
                      <TextField
                        value={r().name}
                        onChange={({ value }) => update(r().key, { name: value })}
                        autoCorrect={false}
                        accessibilityLabel={t("alter.col_name")}
                        testID={`alter-name:${i()}`}
                        class="font-mono h-7 min-w-0 flex-1"
                      />
                      <If when={r().orig === null}>
                        <Text class={`${FS["2xs"]} rounded-full border border-success px-1.5 font-bold text-success`}>
                          {t("alter.badge_new")}
                        </Text>
                      </If>
                      <If when={r().orig !== null && r().orig !== r().name.trim() ? r().orig : null}>
                        {(o) => (
                          <View tooltip={tip(() => t("alter.renamed_from", { name: o() }))} class="shrink-0">
                            <Icon name="pencil" size={11} color="warning" />
                          </View>
                        )}
                      </If>
                    </View>
                    <TextField
                      value={r().dataType}
                      onChange={({ value }) => update(r().key, { dataType: value })}
                      autoCorrect={false}
                      readonly={r().generated}
                      accessibilityLabel={t("alter.col_type")}
                      testID={`alter-type:${i()}`}
                      class="font-mono h-7 w-[150px]"
                    />
                    <View class="w-[64px] items-center">
                      <CheckBox
                        checked={r().notNull}
                        onChange={(v) => update(r().key, { notNull: v })}
                        label={t("alter.col_not_null")}
                        testID={`alter-nn:${i()}`}
                      />
                    </View>
                    <View class="w-[40px] items-center">
                      <CheckBox
                        checked={r().pk}
                        onChange={(v) => update(r().key, { pk: v })}
                        label={t("alter.col_pk")}
                        testID={`alter-pk:${i()}`}
                      />
                    </View>
                    <If
                      when={!r().generated}
                      else={<Text class={`${FS.xs} flex-1 text-muted-foreground`}>{t("alter.generated")}</Text>}
                    >
                      <TextField
                        value={r().def}
                        onChange={({ value }) => update(r().key, { def: value })}
                        autoCorrect={false}
                        placeholder={t("alter.no_default")}
                        accessibilityLabel={t("alter.col_default")}
                        testID={`alter-default:${i()}`}
                        class="font-mono h-7 min-w-0 flex-1"
                      />
                    </If>
                    <View class="w-[88px] flex-row justify-end">
                      <IconButton icon="chevron-up" size={13} label={t("alter.move_up")} tooltip={t("alter.move_up")} disabled={i() === 0} onPress={() => move(r().key, -1)} />
                      <IconButton
                        icon="chevron-down"
                        size={13}
                        label={t("alter.move_down")}
                        tooltip={t("alter.move_down")}
                        disabled={i() === rows().length - 1}
                        onPress={() => move(r().key, 1)}
                      />
                      <IconButton
                        icon="trash"
                        size={13}
                        color="negative"
                        label={t("alter.drop_column")}
                        tooltip={t("alter.drop_column")}
                        testID={`alter-drop:${i()}`}
                        onPress={() => remove(r().key)}
                      />
                    </View>
                  </View>
                )}
              </Each>
              <View class="flex-row">
                <Btn small variant="ghost" icon="plus" label={t("alter.add_column")} testID="alter-add-column" onPress={addColumn} />
              </View>
            </View>

            <View class="gap-1.5">
              <SectionLabel>{t("alter.fks")}</SectionLabel>
              <If when={fks().length > 0} else={<Hint class={FS.xs}>{t("alter.no_fks")}</Hint>}>
                <Each of={fks()}>
                  {(f, i) => (
                    <View testID={`alter-fk:${i()}`} class="flex-row items-center gap-2 rounded-(--radius) border border-border bg-panel px-2.5 py-1">
                      <Icon name="link-2" size={12} color="muted-foreground" />
                      <Text class={`${FS.xs} ${MONO} min-w-0 flex-1 text-foreground line-clamp-1`}>
                        {`(${f().columns.join(", ")}) → ${f().refTable}${f().refColumns.length ? `(${f().refColumns.join(", ")})` : ""}`}
                        {f().onDelete && f().onDelete !== "NO ACTION" ? `  ON DELETE ${f().onDelete}` : ""}
                        {f().onUpdate && f().onUpdate !== "NO ACTION" ? `  ON UPDATE ${f().onUpdate}` : ""}
                      </Text>
                      <IconButton
                        icon="trash"
                        size={12}
                        color="negative"
                        label={t("alter.drop_fk")}
                        tooltip={t("alter.drop_fk")}
                        testID={`alter-fk-drop:${i()}`}
                        onPress={() => removeFk(i())}
                      />
                    </View>
                  )}
                </Each>
              </If>
              <View class="flex-row items-center gap-2">
                <TextField
                  value={fkCols()}
                  onChange={({ value }) => fkCols.set(value)}
                  autoCorrect={false}
                  placeholder={t("alter.fk_columns")}
                  testID="alter-fk-cols"
                  class="font-mono h-7 w-[170px]"
                />
                <Icon name="chevron-right" size={13} />
                <TextField
                  value={fkTable()}
                  onChange={({ value }) => fkTable.set(value)}
                  autoCorrect={false}
                  placeholder={t("alter.fk_table")}
                  testID="alter-fk-table"
                  class="font-mono h-7 w-[170px]"
                />
                <TextField
                  value={fkRefCols()}
                  onChange={({ value }) => fkRefCols.set(value)}
                  autoCorrect={false}
                  placeholder={t("alter.fk_ref_columns")}
                  testID="alter-fk-ref"
                  class="font-mono h-7 w-[170px]"
                />
                <Dropdown
                  small
                  items={FK_ACTIONS.map((a) => ({ value: a, title: `ON DELETE ${a}` }))}
                  value={fkDelete()}
                  label={`ON DELETE ${fkDelete()}`}
                  onChange={(v) => fkDelete.set(v)}
                  accessibilityLabel={t("alter.fk_on_delete")}
                  style={{ width: 190 }}
                />
                <Btn
                  small
                  variant="outline"
                  icon="plus"
                  label={t("alter.add_fk")}
                  testID="alter-add-fk"
                  disabled={!fkCols().trim() || !fkTable().trim()}
                  onPress={addFk}
                />
              </View>
            </View>
          </Scroll>

          <View class="gap-2">
            <View class="flex-row items-center gap-2">
              <SectionLabel>{t("alter.script")}</SectionLabel>
              <If when={script()}>
                <Text
                  testID="alter-mode"
                  class={`${FS["2xs"]} rounded-full border px-1.5 font-bold`}
                  classList={{
                    "border-border text-muted-foreground": script()?.mode === "noop",
                    "border-success text-success": script()?.mode === "simple",
                    "border-warning text-warning": script()?.mode === "rebuild",
                  }}
                >
                  {modeLabel()}
                </Text>
              </If>
              <View class="flex-1" />
              <Btn small variant="ghost" icon="copy" testID="alter-copy" label={t("alter.copy")} disabled={!script()} onPress={() => void copyScript()} />
            </View>
            <If when={previewError()}>{(e) => <ErrorText text={e()} />}</If>
            <If when={(script()?.warnings.length ?? 0) > 0}>
              <View testID="alter-warnings" class="gap-1 rounded-(--radius) border border-warning/40 bg-warning/10 px-2.5 py-1.5">
                <Each of={script()?.warnings ?? []}>
                  {(w) => (
                    <View class="flex-row items-start gap-1.5">
                      <Icon name="triangle-alert" size={12} color="warning" class="mt-0.5" />
                      <Text class={`${FS.xs} min-w-0 flex-1 text-foreground`}>{w()}</Text>
                    </View>
                  )}
                </Each>
              </View>
            </If>
            <View class="h-[170px] overflow-hidden rounded-(--radius) border border-border">
              <Editor
                testID="alter-sql"
                value={script()?.sql ?? ""}
                language="sql"
                readOnly
                softWrap
                fontSize={12}
                class="flex-1 bg-editor"
              />
            </View>
            <If when={applyError()}>{(e) => <ErrorText text={e()} />}</If>
            <If when={violations().length > 0}>
              <View testID="alter-violations" class="gap-1 rounded-(--radius) border border-destructive/40 bg-destructive/10 px-2.5 py-1.5">
                <Text class={`${FS.xs} font-bold text-negative`}>{t("alter.fk_failed", { count: violations().length })}</Text>
                <Each of={violations().slice(0, 20)}>
                  {(v) => (
                    <Text class={`${FS.xs} ${MONO} text-foreground`}>
                      {t("alter.violation", { table: v().table, rowid: v().rowid ?? "—", parent: v().parent })}
                    </Text>
                  )}
                </Each>
              </View>
            </If>
          </View>

          <View class="flex-row items-center justify-end gap-2">
            <Btn label={t("alter.cancel")} variant="outline" onPress={closeAlterTable} />
            <Btn
              label={busy() ? t("alter.running") : t("alter.execute")}
              variant="primary"
              icon={busy() ? "loader-circle" : "play"}
              testID="alter-execute"
              disabled={!script() || script()?.mode === "noop" || busy()}
              onPress={execute}
            />
          </View>
        </If>
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
