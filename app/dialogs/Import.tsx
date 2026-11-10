// 导入数据库:选择目标库,从 .sql 文件读入或直接粘贴 SQL,单事务执行
// (全成功提交,任一失败回滚并报告第几条)。对应 GPUI 版 dialogs/import.rs。

import { cleanup, signal, Text, View, type Signal } from "nsh";
import { Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { db, dialogsNative, errorText, sql, store } from "../lib/native";
import { supportsSql } from "../lib/types";
import { conns, refreshTables } from "../store/conns";
import { Btn, DbIcon, color } from "../ui/kit";
import { FS } from "../ui/text";
import { DialogFrame, Dropdown, LoadingBtn } from "./controls";

interface Report {
  ok: boolean;
  text: string;
}

export function ImportDialog(props: { connId: string; open: boolean; onClose: () => void }) {
  const running = signal(false);
  return (
    <DialogFrame
      open={props.open}
      onClose={() => {
        if (!running()) props.onClose();
      }}
      title={t("import.title")}
      width={640}
      closeOnBackdrop={false}
      testID="import"
    >
      <ImportView connId={props.connId} running={running} onClose={props.onClose} />
    </DialogFrame>
  );
}

function ImportView(props: { connId: string; running: Signal<boolean>; onClose: () => void }) {
  // 只接受关系库
  const initial = conns.get(props.connId);
  const connId = signal<string | null>(initial && supportsSql(initial.kind) ? initial.id : null);
  const text = signal("");
  const fileName = signal<string | null>(null);
  const report = signal<Report | null>(null);
  let alive = true;
  cleanup(() => (alive = false));

  const running = () => props.running();
  const choices = () => conns.list().filter((c) => supportsSql(c.kind));
  const current = () => choices().find((c) => c.id === connId());
  const statements = () => {
    try {
      return sql.statements(text());
    } catch {
      return [];
    }
  };
  const count = () => statements().length;

  async function pickFile() {
    let path: string | null;
    try {
      path = await dialogsNative.pickFile(t("import.choose_file"), "SQL", ["sql"]);
    } catch (e) {
      report.set({ ok: false, text: errorText(e) });
      return;
    }
    if (!path || !alive) return;
    try {
      const content = store.readSqlFile(path);
      text.set(content);
      fileName.set(path.split(/[\\/]/).pop() ?? path);
      report.set(null);
    } catch (e) {
      report.set({ ok: false, text: errorText(e) });
    }
  }

  async function run() {
    const conn = current();
    const stmts = statements();
    if (!conn || stmts.length === 0 || running()) return;
    props.running.set(true);
    report.set(null);
    try {
      await db.connect(conn.kind, conn.id);
      const n = await db.importSql(conn.kind, conn.id, stmts);
      if (!alive) return;
      report.set({ ok: true, text: t("import.done", { count: n }) });
      // 结构可能已变:刷新侧栏表列表
      void refreshTables(conn.id);
    } catch (e) {
      if (alive) report.set({ ok: false, text: t("import.failed", { error: errorText(e) }) });
    } finally {
      props.running.set(false);
    }
  }

  const label = (s: string) => <Text class={`${FS.xs} font-bold text-muted-foreground`}>{s}</Text>;

  return (
    <View class="gap-3.5">
      <View class="gap-1.5">
        {label(t("import.target"))}
        <Dropdown
          testID="import-target"
          items={choices().map((c) => ({ value: c.id, title: c.name, leading: () => <DbIcon kind={c.kind} size={14} /> }))}
          value={connId()}
          label={current()?.name ?? ""}
          placeholder={t("import.choose_db")}
          accessibilityLabel={t("import.target")}
          disabled={running()}
          leading={current() ? <DbIcon kind={current()!.kind} size={16} /> : undefined}
          onChange={(id) => {
            connId.set(id);
            report.set(null);
          }}
        />
      </View>
      <View class="gap-1.5">
        <View class="flex-row items-center justify-between">
          {label(t("import.sql_content"))}
          <LoadingBtn
            testID="import-file"
            xsmall
            icon="file-up"
            label={t("import.choose_file")}
            disabled={running()}
            onPress={() => void pickFile()}
          />
        </View>
        <View class="overflow-hidden rounded-(--radius-lg) border border-border bg-editor" style={{ height: 220 }}>
          <Editor
            testID="import-sql"
            class="flex-1"
            value={text()}
            onChange={({ value }) => {
              text.set(value);
              report.set(null);
            }}
            readOnly={running()}
            language="sql"
            placeholder={t("import.placeholder")}
            scrollBeyondLastLine={false}
            softWrap
          />
        </View>
        <Text testID="import-summary" class={`${FS.xs} text-fg-faint`}>
          {(fileName() ? t("import.loaded", { file: fileName() }) : "") +
            (count() > 0 ? t("import.statements_n", { count: count() }) : t("import.no_statements"))}
        </Text>
      </View>
      <If when={report()}>
        {(r) => (
          <View class="rounded-(--radius-lg) px-3 py-2.5" style={{ backgroundColor: color(r().ok ? "selection" : "deleted") }}>
            <Text testID="import-report" class={`${FS.sm} ${r().ok ? "text-foreground" : "text-negative"}`}>
              {r().text}
            </Text>
          </View>
        )}
      </If>
      <View class="flex-row justify-end gap-2.5">
        <Btn testID="import-close" variant="outline" label={t("import.close")} disabled={running()} onPress={props.onClose} />
        <LoadingBtn
          testID="import-run"
          variant="primary"
          loading={running()}
          label={running() ? t("import.importing") : t("import.start")}
          disabled={!current() || count() === 0}
          onPress={() => void run()}
        />
      </View>
    </View>
  );
}
