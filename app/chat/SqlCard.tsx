// AI 回复里的 SQL 卡片:可编辑(还原/格式化)、直接执行(结果回灌 AI)、编辑器运行、复制;
// 破坏性语句标红并在执行前确认。对应 GPUI 版 chat/sql_card.rs。

import { cleanup, signal, Text, View } from "nsh";
import { Dialog, Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { errorText, sql as sqlNative } from "../lib/native";
import type { DbKind, WriteImpact } from "../lib/types";
import { toast } from "../store/toast";
import { Btn, DbIcon, Icon } from "../ui/kit";
import { editorText } from "../ui/editor";
import { FS } from "../ui/text";
import { executeSql, runInEditor, target, undoCard } from "./actions";
import { askConfirm, cardState, closeConfirm, confirmRequest, type ConfirmRequest, updateCard } from "./cards";
import { chatDeps } from "./deps";
import { ResultTable } from "./ToolCard";
import { ErrorText } from "../ui/error";

type ImpactState = { state: "loading" } | { state: "ok"; impact: WriteImpact } | { state: "none" } | { state: "error"; error: string };

/** 确认框里预览的行数。 */
const PREVIEW_ROWS = 5;

/**
 * 执行前预估各条写语句的影响范围(与编辑器的确认框同一套:改写成只读的 COUNT 与预览,
 * 在数据库强制只读的会话里执行)。只读语句不列出。
 */
function ImpactPreview(props: { sql: string }) {
  const to = target();
  const writes = sqlNative.statements(props.sql).filter((s) => !sqlNative.isReadOnly(s));
  const states = signal<ImpactState[]>(writes.map(() => ({ state: "loading" })));
  let alive = true;
  cleanup(() => {
    alive = false;
  });
  void (async () => {
    for (let i = 0; i < writes.length; i++) {
      let next: ImpactState;
      try {
        const impact = to ? await chatDeps.writeImpact(to.kind, to.connId, writes[i]) : null;
        next = impact ? { state: "ok", impact } : { state: "none" };
      } catch (e) {
        next = { state: "error", error: errorText(e) };
      }
      if (!alive) return;
      states.update((l) => l.map((x, j) => (j === i ? next : x)));
    }
  })();
  const line = (s: ImpactState) => {
    if (s.state === "loading") return t("risk.impact_loading");
    if (s.state === "none") return t("risk.impact_none");
    if (s.state === "error") return t("risk.impact_failed", { error: s.error });
    const n = s.impact.count.toLocaleString();
    if (s.impact.count === 0) return s.impact.wholeTable ? t("risk.impact_table_empty") : t("risk.impact_zero");
    return s.impact.wholeTable ? t("risk.impact_table", { count: n }) : t("risk.impact_rows", { count: n });
  };
  return (
    <If when={writes.length > 0}>
      <View testID="sql-confirm-impact" class="gap-2">
        <Each of={states()}>
          {(s, i) => (
            <View class="gap-1.5 rounded-(--radius) border border-border bg-background px-3 py-2">
              <Text
                class={`${FS.xs}`}
                classList={{
                  "font-bold text-foreground": s().state === "ok" && (s() as { impact: WriteImpact }).impact.count > 0,
                  "text-negative": s().state === "error",
                  "text-muted-foreground": s().state !== "error" && !(s().state === "ok" && (s() as { impact: WriteImpact }).impact.count > 0),
                }}
              >
                {writes.length > 1 ? `${t("chat.stmt_n", { n: i() + 1 })} · ${line(s())}` : line(s())}
              </Text>
              <If when={s().state === "ok" && (s() as { impact: WriteImpact }).impact.rows.length > 0}>
                <ResultTable
                  columns={(s() as { impact: WriteImpact }).impact.columns}
                  rows={(s() as { impact: WriteImpact }).impact.rows.slice(0, PREVIEW_ROWS)}
                />
              </If>
            </View>
          )}
        </Each>
      </View>
    </If>
  );
}

/** 卡片内容高度自适应,超过约 14 行后内部滚动。 */
const MAX_LINES = 14;

/** Datum 外观的 SQL 编辑器(卡片与确认框共用)。 */
/** 卡片里的 SQL 编辑器(文本由卡片状态持有,接法见 ui/editor 的 `editorText`)。 */
function SqlEditor(props: {
  value: string;
  readOnly?: boolean;
  maxRows: number;
  onChange?: (value: string) => void;
  testID?: string;
}) {
  const text = editorText(
    () => props.value,
    (v) => props.onChange?.(v),
    { sql: true },
  );
  return (
    <Editor
      ref={text.ref}
      testID={props.testID}
      defaultValue={text.defaultValue}
      onChange={text.onChange}
      readOnly={props.readOnly}
      language="sql"
      lineNumbers
      softWrap
      folding={false}
      indentGuides={false}
      scrollBeyondLastLine={false}
      minRows={1}
      maxRows={props.maxRows}
      lineHeight={20}
      class="bg-editor"
      classes={{
        gutter: "bg-editor",
        lineNumber: "text-editor-line-number",
        activeLineNumber: "text-editor-active-line-number",
        activeLine: "bg-editor-active-line",
        content: "py-[7px]",
        selection: "bg-text-selection",
        caret: "bg-caret",
      }}
    />
  );
}

function summaryOf(statements: number) {
  return statements > 1
    ? `${t("sql_card.ai_generated")} · ${t("sql_card.n_statements", { count: statements })}`
    : t("sql_card.ai_generated");
}

export function SqlCard(props: {
  cardKey: string;
  /** AI 生成的原文(流式期间持续增长) */
  sql: string;
  streaming: boolean;
  kind: DbKind;
  /** 目标连接标签,用于确认框 */
  target: string;
}) {
  const state = () => cardState(props.cardKey);
  // 流式期间跟随增量;结束后用户的编辑不再被覆盖
  const value = () => (props.streaming ? props.sql : (state().value ?? props.sql));
  const dirty = () => value() !== props.sql;
  const statements = () => sqlNative.statements(value()).length;
  const destructive = () => sqlNative.analyze(value()).length > 0;
  const run = () => state().run;

  const request = (insert: boolean) => {
    const v = value();
    askConfirm({
      cardKey: props.cardKey,
      sql: v,
      insert,
      target: props.target,
      statements: sqlNative.statements(v).length,
      destructive: sqlNative.analyze(v).length > 0,
    });
  };
  const clickInsert = () => {
    const v = value();
    if (sqlNative.isReadOnly(v)) runInEditor(v, false);
    else request(true);
  };

  return (
    <View testID={`sql-card-${props.cardKey}`} class="overflow-hidden rounded-(--radius-lg) border border-ai bg-ai-tint">
      <View class="flex-row items-center gap-1.5 border-b border-border px-2 py-1">
        <Icon name="sparkles" size={12} color="ai" />
        <DbIcon kind={props.kind} size={13} />
        <Text class={`${FS.xs} font-bold text-ai`}>{summaryOf(statements())}</Text>
        <If when={dirty()}>
          <View class="flex-row items-center gap-1">
            <Icon name="pencil" size={12} color="data" />
            <Text class={`${FS.xs} text-data`}>{t("sql_card.edited")}</Text>
          </View>
        </If>
        <View class="flex-1" />
        <If when={dirty()}>
          <Btn
            small
            variant="ghost"
            icon="rotate-ccw"
            label={t("sql_card.revert")}
            tooltip={t("sql_card.revert_tooltip")}
            onPress={() => updateCard(props.cardKey, { value: undefined })}
          />
        </If>
        <If when={!props.streaming}>
          <Btn
            small
            variant="ghost"
            icon="text-align-start"
            label={t("sql_card.format")}
            tooltip={t("sql_card.format_tooltip")}
            onPress={() => updateCard(props.cardKey, { value: sqlNative.format(value()) })}
          />
        </If>
      </View>
      <View class="bg-editor">
        <SqlEditor
          testID={`sql-editor-${props.cardKey}`}
          value={value()}
          readOnly={props.streaming}
          maxRows={MAX_LINES}
          onChange={(v) => {
            if (!props.streaming && v !== value()) updateCard(props.cardKey, { value: v });
          }}
        />
      </View>
      <View class="flex-row items-center gap-1.5 border-t border-border px-2 py-1.5">
        <Btn
          small
          testID={`sql-run-${props.cardKey}`}
          variant={destructive() ? "danger" : "primary"}
          icon="play"
          label={
            run().kind === "running"
              ? t("sql_card.running")
              : run().kind === "done"
                ? t("sql_card.executed")
                : t("sql_card.run")
          }
          tooltip={destructive() ? t("sql_card.run_tooltip_destructive") : t("sql_card.run_tooltip")}
          disabled={run().kind === "done" || run().kind === "running" || props.streaming}
          onPress={() => request(false)}
        />
        <Btn
          small
          variant="ghost"
          icon="square-play"
          label={t("sql_card.run_in_editor")}
          tooltip={t("sql_card.run_in_editor_tooltip")}
          disabled={props.streaming}
          onPress={clickInsert}
        />
        <Btn
          small
          variant="ghost"
          icon="copy"
          label={t("sql_card.copy")}
          onPress={() => {
            chatDeps.copy(value());
            toast.success(t("sql_card.copied"));
          }}
        />
        <If when={destructive() && run().kind === "idle"}>
          <View class="flex-1" />
          <View class="flex-row items-center gap-1">
            <Icon name="triangle-alert" size={12} color="negative" />
            <Text class={`${FS.xs} text-negative`}>{t("sql_card.destructive")}</Text>
          </View>
        </If>
      </View>
      <CardResult run={run()} />
      <CardUndo cardKey={props.cardKey} />
    </View>
  );
}

/** 执行成功后的撤销:按钮 → 卡片内确认 → 事务里执行反向语句。不能撤销时说明原因。 */
function CardUndo(props: { cardKey: string }) {
  const st = () => cardState(props.cardKey);
  // 没有改到任何行(无反向语句、也无原因):无需撤销,什么都不显示
  const undo = () => {
    const u = st().run.kind === "done" ? (st().undo ?? null) : null;
    return u && (u.statements.length > 0 || u.reason) ? u : null;
  };
  const confirm = async () => {
    const u = undo();
    if (!u) return;
    updateCard(props.cardKey, { undoState: "running" });
    const ok = await undoCard(props.cardKey, u);
    updateCard(props.cardKey, { undoState: ok ? "done" : undefined });
    if (ok) toast.success(t("undo.done", { rows: u.rows }));
  };
  return (
    <If when={undo()}>
      {(u) => (
        <View class="border-t border-border px-2.5 py-1.5">
          <If
            when={u().statements.length > 0}
            else={<Text class={`${FS.xs} text-fg-faint`}>{t("undo.unavailable", { reason: u().reason ?? "" })}</Text>}
          >
            <If
              when={st().undoState === "done"}
              else={
                <If
                  when={st().undoState === "confirm" || st().undoState === "running"}
                  else={
                    <View class="flex-row items-center gap-2">
                      <Btn
                        small
                        variant="outline"
                        icon="undo-2"
                        label={t("undo.button")}
                        testID={`sql-undo-${props.cardKey}`}
                        onPress={() => updateCard(props.cardKey, { undoState: "confirm" })}
                      />
                      <Text class={`${FS.xs} text-muted-foreground`}>{t("undo.available", { rows: u().rows })}</Text>
                    </View>
                  }
                >
                  <View class="gap-1.5">
                    <Text class={`${FS.xs} text-foreground`}>{t("undo.card_confirm", { rows: u().rows })}</Text>
                    <View class="flex-row gap-2">
                      <Btn
                        small
                        variant="primary"
                        label={t("undo.confirm")}
                        disabled={st().undoState === "running"}
                        testID={`sql-undo-ok-${props.cardKey}`}
                        onPress={() => void confirm()}
                      />
                      <Btn small variant="ghost" label={t("undo.cancel")} onPress={() => updateCard(props.cardKey, { undoState: undefined })} />
                    </View>
                  </View>
                </If>
              }
            >
              <Text class={`${FS.xs} text-data`}>{`✓ ${t("undo.done", { rows: u().rows })}`}</Text>
            </If>
          </If>
        </View>
      )}
    </If>
  );
}

function CardResult(props: { run: ReturnType<typeof cardState>["run"] }) {
  const done = () => (props.run.kind === "done" ? props.run.summary : null);
  const failed = () => (props.run.kind === "failed" ? props.run.error : null);
  return (
    <>
      <If when={done()}>
        {(summary) => (
          <View class="flex-row items-center gap-1.5 border-t border-border px-2.5 py-1.5">
            <Icon name="check" size={12} color="data" />
            <Text class={`${FS.xs} flex-1 text-data`}>{summary()}</Text>
          </View>
        )}
      </If>
      <If when={failed()}>
        {(error) => (
          <View class="border-t border-border px-2.5 py-1.5">
            <ErrorText size="xs" mono text={error()} />
          </View>
        )}
      </If>
    </>
  );
}

async function confirmed(req: ConfirmRequest) {
  closeConfirm();
  if (req.insert) {
    runInEditor(req.sql, true);
    return;
  }
  updateCard(req.cardKey, { run: { kind: "running" } });
  const { ok, summary, undo } = await executeSql(req.sql);
  updateCard(req.cardKey, { run: ok ? { kind: "done", summary } : { kind: "failed", error: summary }, undo });
}

/** 执行前的确认框:目标连接、语句全文(只读)、破坏性警告。挂在面板上。 */
export function SqlConfirmDialog() {
  return (
    <If when={confirmRequest()}>
      {(req) => {
        const what = () =>
          req().statements > 1 ? t("sql_card.n_statements", { count: req().statements }) : t("sql_card.statement");
        return (
          <Dialog
            testID="sql-confirm"
            open
            animation="fade"
            onOpenChange={({ open }) => {
              if (!open) closeConfirm();
            }}
            class="m-auto w-[560px] gap-3 rounded-(--radius-lg) border border-border bg-popover p-5 shadow-lg"
            backdropClass="bg-overlay"
          >
            <Text class={`${FS.base} font-bold text-popover-foreground`}>
              {req().insert ? t("sql_card.run_in_editor") : t("sql_card.run_sql")}
            </Text>
            <Text class={`${FS.sm} text-popover-foreground`}>
              {req().insert
                ? t("sql_card.confirm_body_insert", { target: req().target, what: what() })
                : t("sql_card.confirm_body", { target: req().target, what: what() })}
            </Text>
            <View class="overflow-hidden rounded-(--radius) border border-border bg-editor">
              <SqlEditor value={req().sql} readOnly maxRows={16} />
            </View>
            <ImpactPreview sql={req().sql} />
            <If when={req().destructive}>
              <View class="flex-row items-center gap-1.5">
                <Icon name="triangle-alert" size={14} color="destructive" />
                <Text class={`${FS.xs} flex-1 font-bold text-destructive`}>{t("sql_card.destructive_warning")}</Text>
              </View>
            </If>
            <View class="flex-row justify-end gap-2 pt-1">
              <Btn label={t("sql_card.cancel")} variant="outline" onPress={closeConfirm} testID="sql-confirm-cancel" />
              <Btn
                testID="sql-confirm-ok"
                label={req().insert ? t("sql_card.confirm_run_in_editor") : t("sql_card.confirm_run")}
                variant={req().destructive ? "danger" : "primary"}
                onPress={() => void confirmed(req())}
              />
            </View>
          </Dialog>
        );
      }}
    </If>
  );
}
