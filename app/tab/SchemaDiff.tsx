// 表结构对比 tab:本连接是源库,选一个同类型的目标库后对比;左边列出每张表的差异,
// 右边是让目标库与源库一致的迁移脚本(删除类语句已注释),可复制或在目标库的编辑器里打开再执行。

import { Clipboard, Scroll, signal, Text, View } from "nsh";
import { Button, Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { connLabel } from "../chat/actions";
import { db, errorText } from "../lib/native";
import type { DiffReport, TableDiff } from "../lib/types";
import { conns } from "../store/conns";
import { toast } from "../store/toast";
import { ErrorText } from "../ui/error";
import { Btn, DbIcon, EnvBadge, Hint, Icon } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { openQueryOn } from "./index";

const STATUS_CLASS: Record<TableDiff["status"], string> = {
  added: "text-success border-success",
  removed: "text-negative border-negative",
  changed: "text-warning border-warning",
};

export function SchemaDiff(props: { connId: string }) {
  const source = () => conns.get(props.connId);
  const targetId = signal<string | null>(null);
  const report = signal<DiffReport | null>(null);
  const error = signal<string | null>(null);
  const busy = signal(false);
  const target = () => conns.get(targetId());
  // 同一种数据库的其他连接才能对比
  const candidates = () => conns.list().filter((c) => c.id !== props.connId && c.kind === source()?.kind);

  async function compare() {
    const src = source();
    const dst = target();
    if (!src || !dst) return;
    busy.set(true);
    error.set(null);
    try {
      // 两边都要先连上(目标库通常还没打开过)
      for (const c of [src, dst]) {
        if (!conns.isConnected(c.id)) await db.connect(c.kind, c.id);
      }
      report.set(await db.schemaDiff(src.kind, src.id, dst.id));
    } catch (e) {
      error.set(errorText(e));
      report.set(null);
    } finally {
      busy.set(false);
    }
  }

  async function copy() {
    const r = report();
    if (!r) return;
    await Clipboard.writeText(r.sql);
    toast.success(t("sdiff.copied"));
  }

  return (
    <View testID="schema-diff" class="min-h-0 flex-1 bg-background">
      <View class="shrink-0 gap-3 border-b border-border px-5 py-4">
        <View class="flex-row flex-wrap items-center gap-2">
          <Icon name="chevrons-up-down" size={16} color="data" />
          <Text class={`${FS.base} font-bold text-foreground`}>{t("sdiff.title")}</Text>
          <Text class={`${FS.sm} text-muted-foreground`}>{t("sdiff.subtitle")}</Text>
        </View>
        <View class="flex-row flex-wrap items-center gap-3">
          <View class="flex-row items-center gap-2 rounded-(--radius) border border-border bg-panel px-3 py-1.5">
            <Text class={`${FS.xs} text-muted-foreground`}>{t("sdiff.source")}</Text>
            <If when={source()}>
              {(c) => (
                <>
                  <DbIcon kind={c().kind} size={13} />
                  <Text class={`${FS.sm} font-bold text-foreground`}>{connLabel(c())}</Text>
                  <EnvBadge env={c().env} />
                </>
              )}
            </If>
          </View>
          <Icon name="chevron-right" size={14} />
          <View class="flex-row flex-wrap items-center gap-1.5">
            <Text class={`${FS.xs} text-muted-foreground`}>{t("sdiff.target")}</Text>
            <If when={candidates().length > 0} else={<Hint class={FS.xs}>{t("sdiff.no_candidates")}</Hint>}>
              <Each of={candidates()} key={(c) => c.id}>
                {(c) => (
                  <Button
                    testID={`sdiff-target:${c().name}`}
                    selected={targetId() === c().id}
                    onPress={() => {
                      targetId.set(c().id);
                      report.set(null);
                    }}
                    class="h-7 flex-row items-center gap-1.5 rounded-(--radius) border border-border px-2.5 hover:bg-list-hover selected:border-primary selected:bg-selection"
                  >
                    <Text class={`${FS.sm} text-foreground`}>{connLabel(c())}</Text>
                    <EnvBadge env={c().env} />
                  </Button>
                )}
              </Each>
            </If>
          </View>
          <View class="flex-1" />
          <Btn
            testID="sdiff-run"
            small
            variant="primary"
            icon={busy() ? "loader-circle" : "play"}
            label={report() ? t("sdiff.rerun") : t("sdiff.run")}
            disabled={!target() || busy()}
            onPress={() => void compare()}
          />
        </View>
      </View>

      <If when={error()}>{(e) => <ErrorText class="px-5 pt-3" text={e()} />}</If>
      <If
        when={report()}
        else={
          <View class="flex-1 items-center justify-center p-6">
            <Hint>{busy() ? t("sdiff.running") : t("sdiff.empty")}</Hint>
          </View>
        }
      >
        {(r) => (
          <View class="min-h-0 flex-1 flex-row">
            <Scroll class="w-[42%] min-w-[280px] border-r border-border" contentClass="gap-3 px-5 py-4">
              <Text testID="sdiff-summary" class={`${FS.sm} text-muted-foreground`}>
                {r().tables.length === 0
                  ? t("sdiff.identical", { n: r().same })
                  : t("sdiff.summary", {
                      added: r().tables.filter((x) => x.status === "added").length,
                      changed: r().tables.filter((x) => x.status === "changed").length,
                      removed: r().tables.filter((x) => x.status === "removed").length,
                      same: r().same,
                    })}
              </Text>
              <Each of={r().tables} key={(x) => x.name}>
                {(x) => (
                  <View testID={`sdiff-table:${x().name}`} class="gap-1.5 rounded-(--radius) border border-border bg-panel px-3 py-2.5">
                    <View class="flex-row items-center gap-2">
                      <Text class={`${FS.sm} ${MONO} flex-1 font-bold text-foreground line-clamp-1`}>{x().name}</Text>
                      <Text class={`${FS["2xs"]} rounded-full border px-1.5 font-bold ${STATUS_CLASS[x().status]}`}>
                        {t(`sdiff.status_${x().status}`)}
                      </Text>
                    </View>
                    <Each of={x().changes}>
                      {(line) => <Text class={`${FS.xs} text-muted-foreground`}>· {line()}</Text>}
                    </Each>
                  </View>
                )}
              </Each>
            </Scroll>
            <View class="min-w-0 flex-1">
              <View class="h-10 shrink-0 flex-row items-center gap-2 border-b border-border px-3">
                <Text class={`${FS.xs} flex-1 font-bold text-muted-foreground`}>{t("sdiff.script")}</Text>
                <Btn testID="sdiff-copy" small variant="ghost" icon="copy" label={t("sdiff.copy")} onPress={() => void copy()} />
                <Btn
                  testID="sdiff-open"
                  small
                  variant="outline"
                  icon="square-play"
                  label={t("sdiff.open_in_editor")}
                  tooltip={t("sdiff.open_tooltip")}
                  disabled={r().tables.length === 0 || !target()}
                  onPress={() => target() && openQueryOn(target()!.id, r().sql)}
                />
              </View>
              <Editor
                testID="sdiff-sql"
                class="flex-1 bg-editor"
                value={r().sql}
                readOnly
                language="sql"
                fontSize={13}
                softWrap
              />
            </View>
          </View>
        )}
      </If>
    </View>
  );
}
