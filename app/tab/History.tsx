// 查询历史浮层:按数据库隔离,只展示当前 tab 所在连接的条目。
// 单击展开完整 SQL,双击再次执行(同连接)或填入编辑器(跨连接)。对应 GPUI 版 tab/history.rs。

import { List, signal, Text, View } from "nsh";
import { Editor } from "nsh/base";
import { t } from "nsh/i18n";
import type { HistoryEntry } from "../lib/types";
import { conns } from "../store/conns";
import { clearHistory, history } from "../store/history";
import { toast } from "../store/toast";
import { Btn, Icon } from "../ui/kit";
import { FS } from "../ui/text";
import { run, setSql, type TabModel } from "./model";

/** 多行 SQL 压缩为单行用于列表展示。 */
const compress = (sql: string) => sql.split(/\s+/).filter(Boolean).join(" ");

const pad = (n: number) => String(n).padStart(2, "0");

/** 相对时间:1 分钟内「刚刚」,1 小时内「x 分钟前」,当天 HH:mm,更早带日期。 */
export function fmtTime(ts: number, now = Date.now()): string {
  const diff = now - ts;
  if (diff < 60_000) return t("history.just_now");
  if (diff < 3_600_000) return t("history.minutes_ago", { n: Math.floor(diff / 60_000) });
  const d = new Date(ts);
  const today = new Date(now);
  const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  return d.toDateString() === today.toDateString() ? hm : `${d.getMonth() + 1}/${d.getDate()} ${hm}`;
}

function meta(e: HistoryEntry): string {
  const extra =
    e.rowCount != null
      ? ` · ${t("history.n_rows", { n: e.rowCount })}`
      : e.affected != null
        ? ` · ${t("history.affected", { n: e.affected })}`
        : "";
  return `· ${fmtTime(e.executedAt)} · ${e.elapsedMs}ms${extra}`;
}

/** 双击:同连接立即执行;跨连接只填入不执行 —— 历史里一条来自 staging 的 UPDATE
 *  被双击到 prod 连接的 tab 上静默执行是不可接受的。 */
function pick(m: TabModel, e: HistoryEntry) {
  m.historyOpen.set(false);
  setSql(m, e.sql);
  if (e.connId === m.connId) run(m, e.sql, true, false);
  else toast.info(t("history.from_other_conn", { name: e.connName }));
}

export function HistoryPanel(props: { m: TabModel }) {
  const m = props.m;
  const expanded = signal<string | null>(null);
  const entries = () => history().filter((e) => e.connId === m.connId);
  const height = () => Math.min(420, entries().length * 50 + (expanded() ? 150 : 0));
  return (
    <View class="w-[480px]">
      <View class="flex-row items-center justify-between border-b border-border px-3 py-2">
        <View class="min-w-0 flex-1 flex-row items-center gap-1.5">
          <Icon name="rotate-ccw-clock" size={13} />
          <Text class={`${FS.xs} text-muted-foreground`}>{t("history.title")}</Text>
          <Text class={`${FS.xs} min-w-0 flex-1 font-mono text-fg-faint line-clamp-1`}>
            · {conns.get(m.connId)?.name ?? ""}
          </Text>
        </View>
        <If when={entries().length > 0}>
          <Btn
            small
            variant="ghost"
            label={t("history.clear")}
            tooltip={t("history.clear_tooltip")}
            onPress={() => {
              clearHistory(m.connId);
              toast.success(t("history.cleared"));
            }}
          />
        </If>
      </View>
      <If
        when={entries().length > 0}
        else={<Text class={`${FS.xs} px-4 py-6 text-center text-fg-faint`}>{t("history.empty")}</Text>}
      >
        <List each={entries()} keyExtractor={(e) => e.id} estimatedItemSize={50} style={{ height: height() }}>
          {(e) => (
            <View
              testID={`hist:${e().id}`}
              onPress={(p) => {
                if (p.clickCount >= 2) pick(m, e());
                else expanded.update((x) => (x === e().id ? null : e().id));
              }}
              class="border-b border-border px-3 py-2 hover:bg-list-hover"
            >
              <Text class={`${FS.xs} font-mono text-foreground line-clamp-1`}>{compress(e().sql)}</Text>
              <View class="mt-0.5 flex-row gap-1">
                <Text class={`${FS.xs} text-muted-foreground`}>{e().connName}</Text>
                <Text class={`${FS.xs} text-fg-faint`}>{meta(e())}</Text>
              </View>
              <If when={expanded() === e().id}>
                <View class="mt-1.5 h-[140px] overflow-hidden rounded-(--radius) border border-border">
                  <Editor
                    class="flex-1 bg-editor"
                    value={e().sql}
                    readOnly
                    lineNumbers={false}
                    folding={false}
                    softWrap
                    language={e().kind === "redis" ? "text" : "sql"}
                    fontSize={12}
                  />
                </View>
              </If>
            </View>
          )}
        </List>
      </If>
    </View>
  );
}
