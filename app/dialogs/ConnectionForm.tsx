// 新建/编辑连接:类型、名称、环境、主机端口、用户密码、数据库(SQLite 为文件;Mongo 可用连接串),
// 带连接串预览与真实连通性测试。对应 GPUI 版 dialogs/connection_form.rs。

import { cleanup, signal, Text, View } from "nsh";
import { Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { dialogsNative, errorText, store } from "../lib/native";
import { type Connection, DB_KINDS, ENVS, KIND_LABEL, SSL_MODES } from "../lib/types";
import { activate, loadTables, saveConnection } from "../store/conns";
import { toast } from "../store/toast";
import { Btn, DbIcon, envLabel, envToken, Icon, IconButton, TextField } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { connectionAndPassword, draftFrom, previewString, sslLabel, validate, withKind, type ConnDraft } from "./connection";
import { DialogFrame, Dropdown, Field, LoadingBtn, MaskedField, Segmented } from "./controls";

const TEST_TIMEOUT_SECS = 10;

type TestStatus =
  | { kind: "idle" }
  | { kind: "testing" }
  | { kind: "success"; ms: number }
  | { kind: "failed"; error: string };

export function ConnectionFormDialog(props: {
  conn?: Connection;
  database?: string;
  open: boolean;
  onClose: () => void;
}) {
  // 打开时的快照:每次打开都会重建这个组件
  const editing = !!props.conn;
  const id = props.conn?.id ?? crypto.randomUUID();
  return (
    <DialogFrame
      open={props.open}
      onClose={props.onClose}
      title={editing ? t("conn_form.edit_title") : t("conn_form.new_title")}
      width={600}
      testID="conn-form"
    >
      <ConnectionForm conn={props.conn} database={props.database} id={id} editing={editing} onDone={props.onClose} />
    </DialogFrame>
  );
}

function ConnectionForm(props: { conn?: Connection; database?: string; id: string; editing: boolean; onDone: () => void }) {
  const draft = signal<ConnDraft>(draftFrom(props.conn, props.database));
  const test = signal<TestStatus>({ kind: "idle" });
  const error = signal<string | null>(null);
  let attempt = 0;
  let fadeTimer: ReturnType<typeof setTimeout> | undefined;
  cleanup(() => {
    attempt++;
    if (fadeTimer) clearTimeout(fadeTimer);
  });

  const d = () => draft();
  const set = (patch: Partial<ConnDraft>) => draft.update((v) => ({ ...v, ...patch }));
  const sqlite = () => d().kind === "sqlite";
  const mongo = () => d().kind === "mongo";
  const uriMode = () => mongo() && d().uriMode;
  const preview = () => {
    try {
      return previewString(d());
    } catch {
      return "";
    }
  };

  async function pickFile() {
    try {
      const path = await dialogsNative.pickFile(t("conn_form.pick_sqlite"), null, []);
      if (path) set({ filePath: path });
    } catch (e) {
      toast.error(errorText(e));
    }
  }

  async function runTest() {
    if (test().kind === "testing") return;
    const mine = ++attempt;
    if (fadeTimer) clearTimeout(fadeTimer);
    let conn: Connection;
    let password: string | null;
    try {
      ({ conn, password } = connectionAndPassword(d(), props.id, props.editing));
    } catch (e) {
      test.set({ kind: "failed", error: errorText(e) });
      return;
    }
    // 编辑时没改密码:用已保存的密码测试(连接串模式下密码框是隐藏的)
    if (password == null) {
      try {
        password = store.savedPassword(props.id);
      } catch {
        password = "";
      }
    }
    test.set({ kind: "testing" });
    const started = Date.now();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timeout = new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error(t("conn_form.timeout", { secs: TEST_TIMEOUT_SECS }))), TEST_TIMEOUT_SECS * 1000);
    });
    let result: TestStatus;
    try {
      await Promise.race([store.testConnection(conn, password ?? ""), timeout]);
      result = { kind: "success", ms: Date.now() - started };
    } catch (e) {
      result = { kind: "failed", error: errorText(e) };
    } finally {
      clearTimeout(timer);
    }
    if (mine !== attempt) return;
    test.set(result);
    // 成功后几秒淡回 idle
    if (result.kind === "success") {
      fadeTimer = setTimeout(() => {
        if (mine === attempt && test().kind === "success") test.set({ kind: "idle" });
      }, 4000);
    }
  }

  function save() {
    let conn: Connection;
    let password: string | null;
    try {
      ({ conn, password } = connectionAndPassword(d(), props.id, props.editing));
    } catch (e) {
      error.set(errorText(e));
      return;
    }
    const invalid = validate(conn);
    if (invalid) {
      error.set(invalid);
      return;
    }
    let saved: Connection;
    try {
      saved = saveConnection(conn, password);
    } catch (e) {
      toast.error(t("conn_form.save_failed", { error: errorText(e) }));
      return;
    }
    activate(saved.id);
    void loadTables(saved.id);
    props.onDone();
  }

  const testIcon = () => {
    switch (test().kind) {
      case "idle":
        return "activity" as const;
      case "testing":
        return "loader-circle" as const;
      case "success":
        return "check" as const;
      case "failed":
        return "circle-alert" as const;
    }
  };
  const testLabel = () => {
    switch (test().kind) {
      case "idle":
        return t("conn_form.test");
      case "testing":
        return t("conn_form.testing");
      case "success":
        return t("conn_form.test_success");
      case "failed":
        return t("conn_form.test_failed");
    }
  };
  const testNote = () => {
    const s = test();
    if (s.kind === "success") return { text: t("conn_form.test_success_note", { ms: s.ms }), color: "text-data" };
    if (s.kind === "failed") return { text: s.error, color: "text-negative" };
    return null;
  };

  return (
    <View class="gap-4">
      <View class="flex-row gap-4">
        <Field label={t("conn_form.type")}>
          <Dropdown
            testID="conn-kind"
            center
            items={DB_KINDS.map((k) => ({ value: k, title: KIND_LABEL[k], leading: () => <DbIcon kind={k} size={14} /> }))}
            value={d().kind}
            label={KIND_LABEL[d().kind]}
            accessibilityLabel={t("conn_form.type")}
            leading={<Icon name={`db-${d().kind}`} size={16} color="primary" />}
            onChange={(k) => set(withKind(d(), k as ConnDraft["kind"]))}
          />
        </Field>
        <Field label={t("conn_form.name")}>
          <TextField
            testID="conn-name"
            autoFocus
            value={d().name}
            placeholder={t("conn_form.name_placeholder")}
            onChange={({ value }) => set({ name: value })}
          />
        </Field>
      </View>

      <If when={sqlite()}>
        <Field label={t("conn_form.file_path")} class="">
          <View class="flex-row gap-2">
            <TextField
              testID="conn-file"
              class="flex-1"
              value={d().filePath}
              placeholder={t("conn_form.file_placeholder")}
              onChange={({ value }) => set({ filePath: value })}
            />
            <IconButton
              icon="folder-open"
              tooltip={t("conn_form.choose_file")}
              onPress={pickFile}
              color="foreground"
              class="h-8 w-8 border border-input"
            />
          </View>
        </Field>
      </If>

      <If when={!sqlite()}>
        <Field label={t("conn_form.env")} class="">
          <Segmented
            testID="conn-env"
            options={ENVS.map((e) => ({ value: e, label: envLabel(e), color: envToken(e) }))}
            value={d().env}
            onChange={(env) => set({ env })}
          />
        </Field>
        <If when={mongo()}>
          <Field label={t("conn_form.mode")} class="">
            <Segmented
              testID="conn-mode"
              options={[
                { value: "form", label: t("conn_form.mode_form") },
                { value: "uri", label: t("conn_form.mode_uri") },
              ]}
              value={d().uriMode ? "uri" : "form"}
              onChange={(m) => {
                set({ uriMode: m === "uri" });
                error.set(null);
              }}
            />
          </Field>
        </If>
      </If>

      <If when={uriMode()}>
        <Field label={t("conn_form.uri")} class="">
          <View class="overflow-hidden rounded-(--radius) border border-border bg-background" style={{ height: 84 }}>
            <Editor
              testID="conn-uri"
              class="flex-1"
              value={d().uri}
              onChange={({ value }) => set({ uri: value })}
              language="text"
              lineNumbers={false}
              softWrap
              activeLine={false}
              indentGuides={false}
              scrollBeyondLastLine={false}
              placeholder="mongodb+srv://user:password@cluster0.example.mongodb.net/app"
            />
          </View>
        </Field>
        <Text class={`${FS.xs} text-fg-faint`}>{t("conn_form.uri_hint")}</Text>
      </If>

      <If when={!sqlite() && !uriMode()}>
        <View class="flex-row gap-4">
          <Field label={t("conn_form.host")}>
            <TextField testID="conn-host" value={d().host} placeholder="127.0.0.1" onChange={({ value }) => set({ host: value })} />
          </Field>
          <Field label={t("conn_form.port")} class="shrink-0" style={{ width: 132 }}>
            <TextField
              testID="conn-port"
              value={d().port}
              placeholder="5432"
              keyboardType="number-pad"
              onChange={({ value }) => set({ port: value })}
            />
          </Field>
        </View>
        <View class="flex-row gap-4">
          <Field label={t("conn_form.user")}>
            <TextField testID="conn-user" value={d().user} placeholder="postgres" onChange={({ value }) => set({ user: value })} />
          </Field>
          <Field label={t("conn_form.password")}>
            <MaskedField
              testID="conn-password"
              value={d().password}
              placeholder={props.editing ? t("conn_form.password_keep") : t("conn_form.password_none")}
              onChange={({ value }) => set({ password: value })}
            />
          </Field>
        </View>
        <View class="flex-row gap-4">
          <Field label={t("conn_form.database")}>
            <TextField
              testID="conn-database"
              value={d().database}
              placeholder="postgres"
              onChange={({ value }) => set({ database: value })}
            />
          </Field>
          <If when={mongo()}>
            <Field label={t("conn_form.auth_source")} class="shrink-0" style={{ width: 150 }}>
              <TextField testID="conn-auth" value={d().authSource} placeholder="admin" onChange={({ value }) => set({ authSource: value })} />
            </Field>
          </If>
          <Field label={t("conn_form.ssl_mode")} class="shrink-0" style={{ width: 200 }}>
            <Dropdown
              testID="conn-ssl"
              center
              items={SSL_MODES.map((m) => ({ value: m, title: sslLabel(m) }))}
              value={d().ssl}
              label={sslLabel(d().ssl)}
              accessibilityLabel={t("conn_form.ssl_mode")}
              tooltip={d().kind === "redis" ? t("conn_form.ssl_redis_hint") : t("conn_form.ssl_hint")}
              leading={<Icon name={d().ssl === "disable" ? "lock-open" : "lock"} size={15} color="foreground" />}
              onChange={(m) => set({ ssl: m as ConnDraft["ssl"] })}
            />
          </Field>
        </View>
      </If>

      <View class="flex-row items-center gap-2.5 rounded-(--radius) border border-border bg-background px-3.5 py-3">
        <Icon name="link-2" size={14} color="fg-faint" />
        <Text testID="conn-preview" class={`${FS.xs} ${MONO} flex-1 text-muted-foreground line-clamp-1`}>
          {preview()}
        </Text>
      </View>

      <If when={error()}>
        {(e) => (
          <Text testID="conn-error" class={`${FS.xs} text-negative`}>
            {e()}
          </Text>
        )}
      </If>

      <View class="flex-row items-center justify-between gap-3 pt-2">
        <View class="min-w-0 flex-1 flex-row items-center gap-3">
          <LoadingBtn
            testID="conn-test"
            icon={testIcon()}
            label={testLabel()}
            loading={test().kind === "testing"}
            onPress={runTest}
          />
          <If when={testNote()}>
            {(n) => (
              <Text testID="conn-test-note" class={`${FS.xs} flex-1 font-semibold line-clamp-1 ${n().color}`}>
                {n().text}
              </Text>
            )}
          </If>
        </View>
        <View class="shrink-0 flex-row gap-2">
          <Btn variant="ghost" label={t("conn_form.cancel")} onPress={props.onDone} testID="conn-cancel" />
          <Btn variant="primary" label={t("conn_form.save")} onPress={save} testID="conn-save" />
        </View>
      </View>
    </View>
  );
}
