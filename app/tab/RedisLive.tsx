// Redis 实时:Pub/Sub 订阅与发布、MONITOR。
//
// 每个会话在 core(rds::live)里独占一条专用连接:订阅 / MONITOR 模式的连接不能再执行别的命令,不能占用共享的
// 多路复用连接;专用连接走与普通连接相同的地址(含 SSH 隧道)、认证与 TLS。消息进 core 的有界缓冲(最近 5000 条,
// 另有字节上限,超出丢最旧的并计数),这里按序号轮询增量,与分析扫描的进度轮询同一个模式。
// nsh 有原生模块向 TSX 推送的回调(Callback.post),这里没用:MONITOR 每秒可能上万条,逐条推送会淹没界面线程,
// 轮询天然合批、限速,缓冲留在 Rust 里也好设上限。
//
// 暂停的取舍:core 照常接收(订阅不断、不漏消息,仍受缓冲上限约束),界面只是不拉取、不刷新;继续时一次补齐,
// 暂停期间超出上限被丢掉的条数会提示出来。停止接收则用「停止」。
// 「停止」只停接收(退订、关专用连接、结束后台任务),缓冲留着还能看详情;开始新会话、关 tab(组件清理)时
// 关闭并移除会话、释放缓冲。断开连接时 core 也会停掉它名下的所有会话。

import { cleanup, List, signal, Text, View } from "nsh";
import { Dialog } from "nsh/base";
import { t } from "nsh/i18n";
import { CheckBox, DialogFrame, Dropdown, Segmented } from "../dialogs/controls";
import { db, errorText } from "../lib/native";
import type { RedisLiveDetail, RedisLiveItem, RedisLivePoll, RedisMonitorFilter, RedisSub, RedisViewFormat } from "../lib/types";
import { conns } from "../store/conns";
import { toast } from "../store/toast";
import { viewFormat } from "../store/redisFormats";
import { ErrorText } from "../ui/error";
import { Btn, Hint, Icon, IconButton, TextField } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { formatBytes } from "./RedisInsight";
import { createDecoded, DecodedBody, DecodeInfo, formatName, FormatPicker, ViewToggle } from "./RedisValueView";

/** 轮询间隔。 */
const POLL_MS = 300;
/** 界面里最多保留的条数(与 core 的缓冲上限一致)。 */
const MAX_ROWS = 5000;
/** 一次轮询拉取的条数;有更多时同一轮接着拉。 */
const BATCH = 1000;
/** 列表一行最多排版的字符数(只显示一行,再多只是白白排版)。 */
const ROW_CHARS = 300;
/** MONITOR 可选时长(秒),默认 60。 */
const DURATIONS = [30, 60, 300, 600];

type Mode = "pubsub" | "monitor";

const pad = (n: number, w = 2) => String(n).padStart(w, "0");

/** `HH:MM:SS.mmm`(本地时间)。 */
export function clock(ms: number): string {
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`;
}

function durationLabel(s: number): string {
  return s < 60 ? t("rlive.seconds", { n: s }) : t("rlive.minutes", { n: s / 60 });
}

/** 列表里内容的格式标签:文本不标。 */
function FormatTag(props: { format: RedisViewFormat }) {
  return (
    <If when={props.format !== "text"}>
      <Text class={`${FS["2xs"]} shrink-0 rounded-full bg-raised px-1.5 text-data`}>{formatName(props.format)}</Text>
    </If>
  );
}

function Header(props: { mode: Mode }) {
  const cell = (label: string, cls: string) => <Text class={`${FS.xs} ${cls} font-bold text-muted-foreground`}>{label}</Text>;
  return (
    <View class="flex-row gap-3 border-b border-border bg-panel px-3 py-1.5">
      {cell(t("rlive.col_time"), "w-[92px]")}
      <If
        when={props.mode === "pubsub"}
        else={
          <>
            {cell(t("rlive.col_client"), "w-[150px]")}
            {cell(t("rlive.col_db"), "w-[36px] text-right")}
            {cell(t("rlive.col_command"), "flex-1")}
          </>
        }
      >
        {cell(t("rlive.col_channel"), "w-[220px]")}
        {cell(t("rlive.col_message"), "flex-1")}
        {cell(t("rlive.col_size"), "w-[72px] text-right")}
      </If>
    </View>
  );
}

function Row(props: { mode: Mode; item: RedisLiveItem; onOpen: () => void }) {
  const it = () => props.item;
  return (
    <View
      class="min-h-[26px] flex-row items-center gap-3 border-b border-border px-3 hover:bg-list-hover"
      onPress={props.onOpen}
      accessibilityLabel={`${t("rlive.view")} ${it().seq}`}
    >
      <Text class={`${FS.xs} ${MONO} w-[92px] text-muted-foreground`}>{clock(it().time)}</Text>
      <If
        when={props.mode === "pubsub"}
        else={
          <>
            <Text class={`${FS.xs} ${MONO} w-[150px] text-muted-foreground line-clamp-1`}>{it().client ?? ""}</Text>
            <Text class={`${FS.xs} ${MONO} w-[36px] text-right text-muted-foreground`}>{it().db ?? ""}</Text>
            <Text class={`${FS.xs} ${MONO} min-w-0 flex-1 text-foreground line-clamp-1`}>
              <Text class="font-bold text-data">{it().command ?? ""}</Text>
              {` ${it().text.slice(it().text.indexOf(" ") + 1 || it().text.length).slice(0, ROW_CHARS)}`}
            </Text>
          </>
        }
      >
        <View class="w-[220px] gap-0.5">
          <Text class={`${FS.xs} ${MONO} text-foreground line-clamp-1`}>{it().channel}</Text>
          <If when={it().pattern}>
            {(p) => <Text class={`${FS["2xs"]} ${MONO} text-fg-faint line-clamp-1`}>{p()}</Text>}
          </If>
        </View>
        <View class="min-w-0 flex-1 flex-row items-center gap-2">
          <FormatTag format={it().format} />
          <Text class={`${FS.xs} ${MONO} min-w-0 flex-1 text-foreground line-clamp-1`}>
            {it().binary ? t("rlive.binary") : it().text.slice(0, ROW_CHARS)}
          </Text>
        </View>
        <Text class={`${FS.xs} ${MONO} w-[72px] text-right text-muted-foreground`}>{formatBytes(it().size)}</Text>
      </If>
    </View>
  );
}

/** 一条消息的详情:保存下来的完整内容(最多 1 MB),走现有的格式识别(格式按频道前缀记住)。 */
function DetailDialog(props: { connId: string; mode: Mode; detail: RedisLiveDetail | "gone" | null; onClose: () => void }) {
  const d = () => (props.detail && props.detail !== "gone" ? props.detail : null);
  const keyName = () => d()?.channel || d()?.command || "";
  const format = () => viewFormat(props.connId, keyName());
  const decoded = createDecoded(() => ({ text: d()?.text ?? "", b64: d()?.b64 ?? null }), format);
  const mode = signal<"tree" | "text">("tree");
  const meta = () => {
    const x = d();
    if (!x) return "";
    return props.mode === "pubsub"
      ? `${clock(x.time)} · ${x.channel}${x.pattern ? ` · ${t("rlive.via_pattern", { pattern: x.pattern })}` : ""}`
      : `${clock(x.time)} · ${x.client} · db${x.db ?? "?"}`;
  };
  return (
    <DialogFrame testID="rlive-detail" open={props.detail !== null} onClose={props.onClose} title={t("rlive.detail_title")} width={820}>
      <If when={d()} else={<Hint>{t("rlive.detail_gone")}</Hint>}>
        {(x) => (
          <View class="h-[480px] gap-2">
            <Text class={`${FS.xs} ${MONO} text-muted-foreground line-clamp-1`}>{meta()}</Text>
            <View class="flex-row items-center gap-2">
              <FormatPicker connId={props.connId} keyName={keyName()} />
              <DecodeInfo decoded={decoded()} requested={format()} />
              <If when={decoded()?.tree != null}>
                <ViewToggle mode={mode()} onChange={(m) => mode.set(m)} />
              </If>
            </View>
            <View class="min-h-0 flex-1 overflow-hidden rounded-(--radius) border border-border">
              <If when={decoded()} else={<View class="flex-1 bg-editor" />}>
                {(dd) => <DecodedBody decoded={dd()} mode={mode()} />}
              </If>
            </View>
            <View class="flex-row items-center gap-3">
              <Text class={`${FS.xs} ${MONO} text-fg-faint`}>{t("rlive.size", { size: formatBytes(x().size) })}</Text>
              <If when={x().stored < x().size}>
                <Text testID="rlive-detail-truncated" class={`${FS.xs} text-warning`}>
                  {t("rlive.detail_truncated", { kept: formatBytes(x().stored) })}
                </Text>
              </If>
            </View>
          </View>
        )}
      </If>
    </DialogFrame>
  );
}

/** 确认框:prod 上发布、开启 MONITOR(性能警告;prod 上要再勾选确认)。 */
function ConfirmDialog(props: {
  testID: string;
  open: boolean;
  title: string;
  body: string;
  detail?: string;
  prodNote?: string;
  /** prod 上开 MONITOR:勾选后才能确认 */
  ack?: string;
  okLabel: string;
  onClose: () => void;
  onConfirm: () => void;
}) {
  const acked = signal(false);
  return (
    <Dialog
      testID={props.testID}
      open={props.open}
      onOpenChange={(e) => {
        if (!e.open) props.onClose();
        acked.set(false);
      }}
      closeOnEscape
      animation="fade"
      backdropClass="bg-overlay"
      class="m-auto w-[540px] max-w-[92%] gap-3 rounded-(--radius-lg) border border-border bg-popover p-5 shadow-lg"
    >
      <View class="flex-row items-center gap-2">
        <Icon name="triangle-alert" size={16} color="warning" />
        <Text class={`${FS.base} min-w-0 flex-1 font-bold text-popover-foreground`}>{props.title}</Text>
      </View>
      <Text class={`${FS.sm} text-popover-foreground`}>{props.body}</Text>
      <If when={props.detail}>{(x) => <Text class={`${FS.xs} ${MONO} text-muted-foreground line-clamp-3`}>{x()}</Text>}</If>
      <If when={props.prodNote}>{(x) => <Text class={`${FS.xs} font-bold text-negative`}>{x()}</Text>}</If>
      <If when={props.ack}>
        {(x) => (
          <View class="flex-row items-center gap-2">
            <CheckBox testID={`${props.testID}-ack`} checked={acked()} onChange={(v) => acked.set(v)} label={x()} />
            <Text class={`${FS.sm} text-popover-foreground`} onPress={() => acked.update((v) => !v)}>
              {x()}
            </Text>
          </View>
        )}
      </If>
      <View class="flex-row justify-end gap-2 pt-1">
        <Btn variant="outline" label={t("rlive.cancel")} onPress={props.onClose} testID={`${props.testID}-cancel`} />
        <Btn
          variant="danger"
          disabled={!!props.ack && !acked()}
          label={props.okLabel}
          onPress={() => {
            acked.set(false);
            props.onConfirm();
          }}
          testID={`${props.testID}-ok`}
        />
      </View>
    </Dialog>
  );
}

export function RedisLive(props: { connId: string }) {
  const id = props.connId;
  const conn = () => conns.get(id);
  const prod = () => conn()?.env === "prod";

  const mode = signal<Mode>("pubsub");
  const session = signal<string | null>(null);
  const status = signal<RedisLivePoll | null>(null);
  /** 最新的在前 */
  const rows = signal<RedisLiveItem[]>([]);
  const paused = signal(false);
  /** 界面没来得及拿到就被缓冲丢掉的条数(暂停太久、来得太快) */
  const missed = signal(0);
  const error = signal<string | null>(null);
  const busy = signal(false);
  const now = signal(Date.now());

  // Pub/Sub 输入
  const subKind = signal<"channel" | "pattern">("channel");
  const subName = signal("");
  const pubChannel = signal("");
  const pubMessage = signal("");
  const confirmPublish = signal(false);

  // MONITOR 输入
  const duration = signal(60);
  const filterCommand = signal("");
  const filterKey = signal("");
  const confirmMonitor = signal(false);

  const detail = signal<RedisLiveDetail | "gone" | null>(null);

  let lastSeq = 0;
  let timer: ReturnType<typeof setInterval> | undefined;
  let polling = false;
  let alive = true;

  const running = () => status()?.running === true;

  function stopTimer() {
    if (timer) clearInterval(timer);
    timer = undefined;
  }

  function startTimer() {
    stopTimer();
    timer = setInterval(() => {
      now.set(Date.now());
      if (!paused()) void poll();
    }, POLL_MS);
  }

  /** 拉增量;有更多时接着拉(最多几轮,剩下的下次再拉)。 */
  async function poll() {
    const sid = session();
    if (!sid || polling) return;
    polling = true;
    try {
      for (let round = 0; round < 5; round++) {
        const p = await db.redisLivePoll(sid, lastSeq, BATCH);
        if (!alive || session() !== sid) return;
        if (p.firstSeq > lastSeq + 1 && p.count > 0) missed.update((n) => n + (p.firstSeq - lastSeq - 1));
        if (p.items.length > 0) {
          lastSeq = p.items[p.items.length - 1].seq;
          const fresh = p.items.slice().reverse();
          rows.update((old) => {
            const next = fresh.concat(old);
            return next.length > MAX_ROWS ? next.slice(0, MAX_ROWS) : next;
          });
        }
        status.set(p);
        if (!p.more) break;
      }
      const p = status();
      if (p && !p.running && !p.more) stopTimer();
    } catch (e) {
      // 会话已不在(断开连接时 core 停掉了它)
      if (alive && session() === sid) {
        error.set(errorText(e));
        const p = status();
        if (p) status.set({ ...p, running: false, ended: p.ended ?? "stopped" });
        stopTimer();
      }
    } finally {
      polling = false;
    }
  }

  /** 停止接收:会话留着(缓冲还在,能看详情),拉一次最终状态。 */
  async function stopSession() {
    const sid = session();
    if (!sid) return;
    await db.redisLiveStop(sid).catch(() => {});
    // 轮询拿到「已结束」后自己停
    paused.set(false);
    void poll();
  }

  /** 关闭并移除当前会话(释放缓冲)。 */
  async function closeSession() {
    const sid = session();
    stopTimer();
    session.set(null);
    if (sid) await db.redisLiveClose(sid).catch(() => {});
  }

  cleanup(() => {
    alive = false;
    stopTimer();
    const sid = session();
    if (sid) void db.redisLiveClose(sid).catch(() => {});
  });

  async function beginSession(start: () => Promise<string>) {
    await closeSession();
    status.set(null);
    const sid = await start();
    if (!alive) {
      void db.redisLiveClose(sid).catch(() => {});
      return;
    }
    session.set(sid);
    lastSeq = 0;
    missed.set(0);
    rows.set([]);
    paused.set(false);
    error.set(null);
    startTimer();
    void poll();
  }

  // ---------------------------------------------------------------- Pub/Sub

  async function subscribe() {
    const name = subName().trim();
    if (!name) return;
    const sub: RedisSub = { pattern: subKind() === "pattern", name };
    busy.set(true);
    error.set(null);
    try {
      const sid = session();
      if (sid && running()) {
        await db.redisLiveSubscribe(sid, [sub]);
        void poll();
      } else {
        await beginSession(() => db.redisPubsubStart(id, [sub]));
      }
      subName.set("");
    } catch (e) {
      error.set(errorText(e));
    } finally {
      busy.set(false);
    }
  }

  async function unsubscribe(sub: RedisSub) {
    const sid = session();
    if (!sid) return;
    try {
      await db.redisLiveUnsubscribe(sid, [sub]);
      void poll();
    } catch (e) {
      error.set(errorText(e));
    }
  }

  async function publish() {
    const channel = pubChannel().trim();
    if (!channel) return;
    confirmPublish.set(false);
    try {
      const n = await db.redisPublish(id, channel, pubMessage());
      toast.success(t("rlive.published", { n }));
    } catch (e) {
      toast.error(errorText(e));
    }
  }

  function requestPublish() {
    if (!pubChannel().trim()) return;
    if (prod()) confirmPublish.set(true);
    else void publish();
  }

  // ---------------------------------------------------------------- MONITOR

  const filter = (): RedisMonitorFilter => ({ command: filterCommand().trim(), key: filterKey() });

  async function startMonitor() {
    confirmMonitor.set(false);
    busy.set(true);
    error.set(null);
    try {
      await beginSession(() => db.redisMonitorStart(id, duration(), filter()));
    } catch (e) {
      error.set(errorText(e));
    } finally {
      busy.set(false);
    }
  }

  /** 过滤条件改了:进行中的会话立即换(本地过滤,缓冲里不符合的一并拿掉),界面列表同步筛掉。 */
  let filterTimer: ReturnType<typeof setTimeout> | undefined;
  cleanup(() => filterTimer && clearTimeout(filterTimer));
  function filterChanged() {
    if (filterTimer) clearTimeout(filterTimer);
    filterTimer = setTimeout(() => {
      const sid = session();
      if (!sid || mode() !== "monitor") return;
      const f = filter();
      void db
        .redisLiveFilter(sid, f)
        .then(() => {
          const cmds = f.command
            .split(/[\s,]+/)
            .filter(Boolean)
            .map((c) => c.toUpperCase());
          rows.update((old) =>
            old.filter((r) => (cmds.length === 0 || cmds.includes(r.command ?? "")) && (!f.key || r.text.includes(f.key))),
          );
        })
        .catch((e) => error.set(errorText(e)));
    }, 300);
  }

  // ---------------------------------------------------------------- 通用

  async function switchMode(m: Mode) {
    if (m === mode()) return;
    await closeSession();
    rows.set([]);
    status.set(null);
    missed.set(0);
    error.set(null);
    mode.set(m);
  }

  async function clear() {
    rows.set([]);
    missed.set(0);
    const sid = session();
    if (!sid) return;
    try {
      await db.redisLiveClear(sid);
      // 清空前还没拉到的不算「丢失」:从清空后的第一条接着拉
      const p = await db.redisLivePoll(sid, lastSeq, 1);
      lastSeq = Math.max(lastSeq, p.firstSeq - 1);
    } catch {
      // 会话已不在
    }
  }

  async function openDetail(item: RedisLiveItem) {
    const sid = session();
    if (!sid) {
      detail.set("gone");
      return;
    }
    try {
      detail.set((await db.redisLiveDetail(sid, item.seq)) ?? "gone");
    } catch {
      // 会话已不在(断开连接时 core 移除了它):列表里的预览就是能看到的全部
      detail.set({
        seq: item.seq,
        time: item.time,
        channel: item.channel,
        pattern: item.pattern ?? null,
        text: item.text,
        b64: null,
        size: item.size,
        stored: item.truncated ? item.text.length : item.size,
        client: item.client ?? "",
        db: item.db ?? null,
        command: item.command ?? "",
      });
    }
  }

  const remaining = () => {
    const p = status();
    if (!p?.deadline || !p.running) return 0;
    return Math.max(0, Math.ceil((p.deadline - now()) / 1000));
  };

  const statusText = () => {
    const p = status();
    if (!p) return "";
    const count = t("rlive.count", { shown: rows().length.toLocaleString(), received: p.received.toLocaleString() });
    if (p.running) {
      if (paused()) return `${t("rlive.paused")} · ${count}`;
      return p.kind === "monitor" ? `${t("rlive.monitoring", { s: remaining() })} · ${count}` : `${t("rlive.receiving")} · ${count}`;
    }
    const why =
      p.ended === "timeout"
        ? t("rlive.ended_timeout")
        : p.ended === "closed"
          ? t("rlive.ended_closed")
          : p.ended === "error"
            ? t("rlive.ended_error")
            : t("rlive.ended_stopped");
    return `${why} · ${count}`;
  };

  return (
    <View testID="redis-live" class="min-h-0 flex-1 gap-3 bg-background px-5 py-4">
      <View class="flex-row items-center gap-3">
        <Icon name="radio" size={16} color="data" />
        <Text class={`${FS.base} min-w-0 flex-1 font-bold text-foreground line-clamp-1`}>
          {t("rlive.heading", { name: conn()?.name ?? "" })}
        </Text>
        <View class="w-[260px]">
          <Segmented
            testID="rlive-mode"
            options={[
              { value: "pubsub", label: "Pub/Sub" },
              { value: "monitor", label: "MONITOR" },
            ]}
            value={mode()}
            onChange={(m) => void switchMode(m)}
          />
        </View>
      </View>

      <If
        when={mode() === "pubsub"}
        else={
          <View class="gap-2">
            <View class="flex-row items-start gap-2 rounded-(--radius) border border-warning bg-panel px-3 py-2">
              <Icon name="triangle-alert" size={14} color="warning" />
              <Text testID="rlive-monitor-warning" class={`${FS.xs} min-w-0 flex-1 text-foreground`}>
                {t("rlive.monitor_warning")}
              </Text>
            </View>
            <View class="flex-row items-center gap-2">
              <Dropdown
                testID="rlive-duration"
                small
                disabled={running()}
                items={DURATIONS.map((s) => ({ value: String(s), title: durationLabel(s) }))}
                value={String(duration())}
                label={durationLabel(duration())}
                accessibilityLabel={t("rlive.duration")}
                tooltip={t("rlive.duration")}
                onChange={(v) => duration.set(Number(v))}
                style={{ width: 120 }}
              />
              <TextField
                testID="rlive-filter-command"
                class="h-7 w-[180px] font-mono"
                value={filterCommand()}
                placeholder={t("rlive.filter_command")}
                onChange={({ value }) => {
                  filterCommand.set(value);
                  filterChanged();
                }}
              />
              <TextField
                testID="rlive-filter-key"
                class="h-7 min-w-0 flex-1 font-mono"
                value={filterKey()}
                placeholder={t("rlive.filter_key")}
                onChange={({ value }) => {
                  filterKey.set(value);
                  filterChanged();
                }}
              />
              <If
                when={running()}
                else={
                  <Btn
                    testID="rlive-monitor-start"
                    small
                    variant="primary"
                    icon="play"
                    disabled={busy()}
                    label={t("rlive.monitor_start")}
                    onPress={() => confirmMonitor.set(true)}
                  />
                }
              >
                <Btn testID="rlive-stop" small icon="circle-x" label={t("rlive.stop")} onPress={() => void stopSession()} />
              </If>
            </View>
            <Hint class={FS.xs}>{t("rlive.filter_hint")}</Hint>
          </View>
        }
      >
        <View class="gap-2">
          <View class="flex-row items-center gap-2">
            <Dropdown
              testID="rlive-sub-kind"
              small
              items={[
                { value: "channel", title: t("rlive.channel") },
                { value: "pattern", title: t("rlive.pattern") },
              ]}
              value={subKind()}
              label={subKind() === "channel" ? t("rlive.channel") : t("rlive.pattern")}
              accessibilityLabel={t("rlive.sub_kind")}
              onChange={(v) => subKind.set(v as "channel" | "pattern")}
              style={{ width: 110 }}
            />
            <TextField
              testID="rlive-sub-name"
              class="h-7 min-w-0 flex-1 font-mono"
              value={subName()}
              placeholder={subKind() === "channel" ? t("rlive.channel_placeholder") : t("rlive.pattern_placeholder")}
              onChange={({ value }) => subName.set(value)}
              onSubmit={() => void subscribe()}
            />
            <Btn
              testID="rlive-subscribe"
              small
              variant="primary"
              icon="plus"
              disabled={busy() || !subName().trim()}
              label={t("rlive.subscribe")}
              onPress={() => void subscribe()}
            />
            <If when={running()}>
              <Btn testID="rlive-stop" small icon="circle-x" label={t("rlive.stop")} onPress={() => void stopSession()} />
            </If>
          </View>
          <If when={running() && (status()?.subs.length ?? 0) > 0}>
            <View testID="rlive-subs" class="flex-row flex-wrap gap-1.5">
              <Each of={status()?.subs ?? []} key={(s) => `${s.pattern ? "p" : "c"}:${s.name}`}>
                {(s) => (
                  <View class="h-6 flex-row items-center gap-1.5 rounded-full border border-border bg-panel pl-2.5 pr-1">
                    <View class={`h-1.5 w-1.5 rounded-full ${s().active ? "bg-success" : "bg-fg-faint"}`} />
                    <Text class={`${FS.xs} ${MONO} text-foreground`}>{s().name}</Text>
                    <If when={s().pattern}>
                      <Text class={`${FS["2xs"]} text-fg-faint`}>{t("rlive.pattern")}</Text>
                    </If>
                    <IconButton
                      icon="x"
                      size={11}
                      class="p-0.5"
                      label={`${t("rlive.unsubscribe")} ${s().name}`}
                      tooltip={t("rlive.unsubscribe")}
                      onPress={() => void unsubscribe(s())}
                    />
                  </View>
                )}
              </Each>
            </View>
          </If>
          <View class="flex-row items-center gap-2">
            <Text class={`${FS.xs} w-[110px] font-bold text-muted-foreground`}>{t("rlive.publish_to")}</Text>
            <TextField
              testID="rlive-pub-channel"
              class="h-7 w-[220px] font-mono"
              value={pubChannel()}
              placeholder={t("rlive.channel_placeholder")}
              onChange={({ value }) => pubChannel.set(value)}
            />
            <TextField
              testID="rlive-pub-message"
              class="h-7 min-w-0 flex-1 font-mono"
              value={pubMessage()}
              placeholder={t("rlive.message_placeholder")}
              onChange={({ value }) => pubMessage.set(value)}
              onSubmit={requestPublish}
            />
            <Btn
              testID="rlive-publish"
              small
              icon="send"
              disabled={!pubChannel().trim()}
              label={t("rlive.publish")}
              onPress={requestPublish}
            />
          </View>
          <Hint class={FS.xs}>{t("rlive.pubsub_hint")}</Hint>
        </View>
      </If>

      <If when={error()}>{(e) => <ErrorText testID="rlive-error" text={e()} />}</If>
      <If when={status()?.error}>{(e) => <ErrorText testID="rlive-server-error" text={e() ?? ""} />}</If>

      <View class="flex-row items-center gap-2">
        <Text testID="rlive-status" class={`${FS.xs} min-w-0 flex-1 text-muted-foreground line-clamp-1`}>
          {statusText()}
        </Text>
        <If when={missed() > 0}>
          <Text testID="rlive-missed" class={`${FS.xs} text-warning`}>
            {t("rlive.missed", { n: missed().toLocaleString(), max: MAX_ROWS.toLocaleString() })}
          </Text>
        </If>
        <If when={running()}>
          <Btn
            testID="rlive-pause"
            small
            variant="ghost"
            icon={paused() ? "play" : "pause"}
            label={paused() ? t("rlive.resume") : t("rlive.pause")}
            tooltip={t("rlive.pause_tip")}
            onPress={() => {
              paused.update((v) => !v);
              if (!paused()) void poll();
            }}
          />
        </If>
        <Btn testID="rlive-clear" small variant="ghost" icon="eraser" label={t("rlive.clear")} onPress={() => void clear()} />
      </View>

      <View class="min-h-0 flex-1 overflow-hidden rounded-(--radius) border border-border">
        <Header mode={mode()} />
        <List
          testID="rlive-list"
          each={rows()}
          keyExtractor={(r) => String(r.seq)}
          estimatedItemSize={26}
          class="min-h-0 flex-1"
          empty={
            <Hint class={`${FS.xs} p-3`}>
              {status() ? t("rlive.empty_waiting") : mode() === "pubsub" ? t("rlive.empty_pubsub") : t("rlive.empty_monitor")}
            </Hint>
          }
        >
          {(r) => (
            <Row
              mode={mode()}
              item={r()}
              onOpen={() => void openDetail(r())}
            />
          )}
        </List>
      </View>

      <DetailDialog connId={id} mode={mode()} detail={detail()} onClose={() => detail.set(null)} />
      <ConfirmDialog
        testID="rlive-publish-confirm"
        open={confirmPublish()}
        title={t("rlive.publish_confirm_title")}
        body={t("rlive.publish_confirm_body", { channel: pubChannel().trim() })}
        detail={pubMessage().slice(0, 300)}
        prodNote={t("rlive.prod_note")}
        okLabel={t("rlive.publish")}
        onClose={() => confirmPublish.set(false)}
        onConfirm={() => void publish()}
      />
      <ConfirmDialog
        testID="rlive-monitor-confirm"
        open={confirmMonitor()}
        title={t("rlive.monitor_confirm_title")}
        body={t("rlive.monitor_confirm_body", { duration: durationLabel(duration()) })}
        prodNote={prod() ? t("rlive.monitor_prod_note") : undefined}
        ack={prod() ? t("rlive.monitor_prod_ack") : undefined}
        okLabel={t("rlive.monitor_start")}
        onClose={() => confirmMonitor.set(false)}
        onConfirm={() => void startMonitor()}
      />
    </View>
  );
}
