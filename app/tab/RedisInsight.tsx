// Redis 分析 tab:概览(INFO)、整库扫描(最大的 key、前缀占用、TTL / 类型分布、热 key)与慢日志。
// 扫描在 core 后台进行,这里轮询进度;离开 tab 不取消(回来接着看),「取消」按钮才中止。

import { cleanup, type JSX, Scroll, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";
import { db, errorText } from "../lib/native";
import type { RedisGroupStat, RedisKeyStat, RedisOverview, RedisReport, RedisSlowEntry } from "../lib/types";
import { conns } from "../store/conns";
import { Btn, Hint, Icon, SectionLabel } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import { openTable } from "./index";
import { ErrorText } from "../ui/error";

type Load<T> = { state: "loading" } | { state: "ok"; value: T } | { state: "error"; error: string };

/** 按加载状态显示:加载中 / 出错 / 内容。 */
function Loaded<T>(props: { load: Load<T>; error?: (e: string) => string; children: (v: T) => JSX.Element }) {
  const err = () => {
    const l = props.load;
    return l.state === "error" ? l.error : "";
  };
  return (
    <If
      when={props.load.state === "ok"}
      else={
        <If when={props.load.state === "error"} else={<Hint>{t("rinsight.loading")}</Hint>}>
          <ErrorText text={props.error ? props.error(err()) : err()} />
        </If>
      }
    >
      {props.children((props.load as { value: T }).value)}
    </If>
  );
}

/** 轮询扫描进度的间隔。 */
const POLL_MS = 300;

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

export function formatTtl(ms: number): string {
  if (ms < 0) return t("rinsight.ttl_never");
  const s = Math.round(ms / 1000);
  if (s < 60) return t("rinsight.ttl_s", { n: s });
  if (s < 3600) return t("rinsight.ttl_m", { n: Math.round(s / 60) });
  if (s < 86400) return t("rinsight.ttl_h", { n: Math.round(s / 3600) });
  return t("rinsight.ttl_d", { n: Math.round(s / 86400) });
}

const pct = (part: number, whole: number) => (whole > 0 ? Math.round((part / whole) * 1000) / 10 : 0);

function Card(props: { label: string; value: string; testID?: string }) {
  return (
    <View testID={props.testID} class="min-w-[150px] flex-1 gap-1 rounded-(--radius) border border-border bg-panel px-3 py-2">
      <Text class={`${FS.xs} text-muted-foreground`}>{props.label}</Text>
      <Text class={`${FS.base} font-bold text-foreground`}>{props.value}</Text>
    </View>
  );
}

function Section(props: { title: string; children: JSX.Element; right?: JSX.Element }) {
  return (
    <View class="gap-2">
      <View class="flex-row items-center gap-2">
        <SectionLabel class="flex-1">{props.title}</SectionLabel>
        {props.right}
      </View>
      {props.children}
    </View>
  );
}

/** 一行:名称 + 数值 + 占比条。 */
function BarRow(props: { name: string; detail: string; share: number; mono?: boolean }) {
  return (
    <View class="gap-1">
      <View class="flex-row items-center gap-3">
        <Text class={`${FS.sm} ${props.mono ? MONO : ""} min-w-0 flex-1 text-foreground line-clamp-1`}>{props.name}</Text>
        <Text class={`${FS.xs} ${MONO} text-muted-foreground`}>{props.detail}</Text>
      </View>
      <View class="h-1 rounded-full bg-raised">
        <View class="h-1 rounded-full bg-primary" style={{ width: `${Math.max(0.5, Math.min(100, props.share))}%` }} />
      </View>
    </View>
  );
}

function Overview(props: { o: RedisOverview }) {
  const o = props.o;
  const dash = (v: string | null | undefined) => (v === null || v === undefined || v === "" ? "—" : v);
  const maxMem = () => (o.maxMemory === "0B" || o.maxMemory === null ? t("rinsight.unlimited") : o.maxMemory);
  return (
    <View class="flex-row flex-wrap gap-2">
      <Card testID="ri-used-memory" label={t("rinsight.used_memory")} value={dash(o.usedMemory)} />
      <Card label={t("rinsight.peak_memory")} value={dash(o.usedMemoryPeak)} />
      <Card label={t("rinsight.max_memory")} value={`${maxMem()} · ${dash(o.maxMemoryPolicy)}`} />
      <Card label={t("rinsight.fragmentation")} value={dash(o.fragmentationRatio)} />
      <Card label={t("rinsight.hit_rate")} value={o.hitRate === null ? "—" : `${o.hitRate}%`} />
      <Card
        testID="ri-keys"
        label={t("rinsight.keys")}
        value={t("rinsight.keys_value", { keys: (o.keys ?? 0).toLocaleString(), expires: (o.expires ?? 0).toLocaleString() })}
      />
      <Card label={t("rinsight.clients")} value={dash(o.connectedClients)} />
      <Card label={t("rinsight.evicted")} value={dash(o.evictedKeys)} />
    </View>
  );
}

function KeyTable(props: { connId: string; keys: RedisKeyStat[]; metric: "bytes" | "freq"; testID: string }) {
  return (
    <View testID={props.testID} class="rounded-(--radius) border border-border">
      <View class="flex-row gap-3 border-b border-border bg-panel px-3 py-1.5">
        <Text class={`${FS.xs} flex-1 font-bold text-muted-foreground`}>key</Text>
        <Text class={`${FS.xs} w-[70px] font-bold text-muted-foreground`}>{t("rinsight.col_type")}</Text>
        <Text class={`${FS.xs} w-[90px] text-right font-bold text-muted-foreground`}>
          {props.metric === "bytes" ? t("rinsight.col_memory") : t("rinsight.col_freq")}
        </Text>
        <Text class={`${FS.xs} w-[90px] text-right font-bold text-muted-foreground`}>TTL</Text>
      </View>
      <Each of={props.keys}>
        {(k) => (
          <View
            class="flex-row gap-3 px-3 py-1 hover:bg-list-hover"
            onPress={() => openTable(props.connId, k().key)}
            accessibilityLabel={k().key}
          >
            <Text class={`${FS.sm} ${MONO} min-w-0 flex-1 text-foreground line-clamp-1`}>{k().key}</Text>
            <Text class={`${FS.xs} w-[70px] text-muted-foreground`}>{k().type}</Text>
            <Text class={`${FS.xs} ${MONO} w-[90px] text-right text-foreground`}>
              {props.metric === "bytes" ? formatBytes(k().bytes ?? 0) : String(k().freq ?? 0)}
            </Text>
            <Text class={`${FS.xs} w-[90px] text-right text-muted-foreground`}>{formatTtl(k().ttlMs)}</Text>
          </View>
        )}
      </Each>
    </View>
  );
}

function Groups(props: { groups: RedisGroupStat[]; r: RedisReport; prefix?: boolean; testID: string }) {
  const byBytes = () => props.r.memorySupported && props.r.totalBytes > 0;
  return (
    <View testID={props.testID} class="gap-2">
      <Each of={props.groups}>
        {(g) => (
          <BarRow
            mono={props.prefix}
            name={props.prefix && g().name === "" ? t("rinsight.no_prefix") : props.prefix ? `${g().name}:*` : g().name}
            detail={
              byBytes()
                ? t("rinsight.group_detail", {
                    keys: g().keys.toLocaleString(),
                    bytes: formatBytes(g().bytes),
                    pct: pct(g().bytes, props.r.totalBytes),
                  })
                : t("rinsight.group_detail_keys", { keys: g().keys.toLocaleString(), pct: pct(g().keys, props.r.scanned) })
            }
            share={byBytes() ? pct(g().bytes, props.r.totalBytes) : pct(g().keys, props.r.scanned)}
          />
        )}
      </Each>
    </View>
  );
}

function TtlDist(props: { r: RedisReport }) {
  const rows = () => {
    const b = props.r.ttl;
    return [
      [t("rinsight.ttl_never"), b.persistent],
      [t("rinsight.ttl_within_hour"), b.withinHour],
      [t("rinsight.ttl_within_day"), b.withinDay],
      [t("rinsight.ttl_within_week"), b.withinWeek],
      [t("rinsight.ttl_longer"), b.longer],
    ] as [string, number][];
  };
  return (
    <View testID="ri-ttl" class="gap-2">
      <Each of={rows()}>
        {(row) => (
          <BarRow
            name={row()[0]}
            detail={`${row()[1].toLocaleString()} · ${pct(row()[1], props.r.scanned)}%`}
            share={pct(row()[1], props.r.scanned)}
          />
        )}
      </Each>
    </View>
  );
}

function SlowLog(props: { entries: RedisSlowEntry[] }) {
  return (
    <If when={props.entries.length > 0} else={<Hint>{t("rinsight.slowlog_empty")}</Hint>}>
      <View testID="ri-slowlog" class="rounded-(--radius) border border-border">
        <View class="flex-row gap-3 border-b border-border bg-panel px-3 py-1.5">
          <Text class={`${FS.xs} w-[150px] font-bold text-muted-foreground`}>{t("rinsight.col_time")}</Text>
          <Text class={`${FS.xs} w-[80px] text-right font-bold text-muted-foreground`}>{t("rinsight.col_duration")}</Text>
          <Text class={`${FS.xs} flex-1 font-bold text-muted-foreground`}>{t("rinsight.col_command")}</Text>
          <Text class={`${FS.xs} w-[160px] font-bold text-muted-foreground`}>{t("rinsight.col_client")}</Text>
        </View>
        <Each of={props.entries}>
          {(e) => (
            <View class="flex-row gap-3 px-3 py-1">
              <Text class={`${FS.xs} ${MONO} w-[150px] text-muted-foreground`}>
                {new Date(e().time * 1000).toLocaleString()}
              </Text>
              <Text class={`${FS.xs} ${MONO} w-[80px] text-right text-foreground`}>
                {(e().micros / 1000).toFixed(1)} ms
              </Text>
              <Text class={`${FS.xs} ${MONO} min-w-0 flex-1 text-foreground line-clamp-2`}>{e().command}</Text>
              <Text class={`${FS.xs} ${MONO} w-[160px] text-muted-foreground line-clamp-1`}>{e().client}</Text>
            </View>
          )}
        </Each>
      </View>
    </If>
  );
}

export function RedisInsight(props: { connId: string }) {
  const id = props.connId;
  const overview = signal<Load<RedisOverview>>({ state: "loading" });
  const slowlog = signal<Load<RedisSlowEntry[]>>({ state: "loading" });
  const report = signal<RedisReport | null>(null);
  const startError = signal<string | null>(null);
  let timer: ReturnType<typeof setInterval> | undefined;
  let alive = true;
  cleanup(() => {
    alive = false;
    if (timer) clearInterval(timer);
  });

  async function loadOverview() {
    overview.set({ state: "loading" });
    slowlog.set({ state: "loading" });
    try {
      const v = await db.redisOverview(id);
      if (alive) overview.set({ state: "ok", value: v });
    } catch (e) {
      if (alive) overview.set({ state: "error", error: errorText(e) });
    }
    try {
      const v = await db.redisSlowlog(id);
      if (alive) slowlog.set({ state: "ok", value: v });
    } catch (e) {
      if (alive) slowlog.set({ state: "error", error: errorText(e) });
    }
  }

  async function poll() {
    try {
      const r = await db.redisAnalysisReport(id);
      if (!alive) return;
      report.set(r);
      if (!r || r.done) {
        if (timer) clearInterval(timer);
        timer = undefined;
      }
    } catch {
      // 轮询失败(连接断开等)下次再试
    }
  }

  function startPolling() {
    if (timer) clearInterval(timer);
    timer = setInterval(() => void poll(), POLL_MS);
    void poll();
  }

  async function start() {
    startError.set(null);
    try {
      await db.redisAnalysisStart(id);
      startPolling();
    } catch (e) {
      startError.set(errorText(e));
    }
  }

  void loadOverview();
  // 之前开始过(离开再回来):接着显示 / 轮询
  startPolling();

  const running = () => {
    const r = report();
    return r !== null && !r.done;
  };
  const progress = () => {
    const r = report();
    if (!r) return 0;
    return r.total > 0 ? Math.min(100, pct(r.scanned, r.total)) : r.done ? 100 : 0;
  };
  const conn = () => conns.get(id);

  return (
    <Scroll testID="redis-insight" class="min-h-0 flex-1 bg-background" contentClass="gap-6 px-5 py-4">
      <View class="flex-row items-center gap-3">
        <Icon name="activity" size={16} color="data" />
        <Text class={`${FS.base} flex-1 font-bold text-foreground`}>
          {t("rinsight.heading", { name: conn()?.name ?? "", db: conn()?.database || "0" })}
        </Text>
        <Btn small icon="refresh-cw" label={t("rinsight.refresh")} onPress={() => void loadOverview()} />
      </View>

      <Section title={t("rinsight.overview")}>
        <Loaded load={overview()}>{(o) => <Overview o={o} />}</Loaded>
      </Section>

      <Section
        title={t("rinsight.scan")}
        right={
          <If
            when={running()}
            else={
              <Btn
                testID="ri-start"
                small
                variant="primary"
                icon="play"
                label={report() ? t("rinsight.rescan") : t("rinsight.start")}
                onPress={() => void start()}
              />
            }
          >
            <Btn testID="ri-cancel" small icon="x" label={t("rinsight.cancel")} onPress={() => void db.redisAnalysisCancel(id)} />
          </If>
        }
      >
        <Hint class={FS.xs}>{t("rinsight.scan_hint", { max: (200000).toLocaleString() })}</Hint>
        <If when={startError()}>{(e) => <ErrorText text={e()} />}</If>
        <If when={report()}>
          {(r) => (
            <View class="gap-5">
              <View class="gap-1.5">
                <View class="h-1.5 rounded-full bg-raised">
                  <View class="h-1.5 rounded-full bg-primary" style={{ width: `${progress()}%` }} />
                </View>
                <Text testID="ri-progress" class={`${FS.xs} text-muted-foreground`}>
                  {r().done
                    ? r().error
                      ? t("rinsight.scan_failed", { error: r().error ?? "" })
                      : r().cancelled
                        ? t("rinsight.scan_cancelled", { scanned: r().scanned.toLocaleString() })
                        : r().truncated
                          ? t("rinsight.scan_truncated", { scanned: r().scanned.toLocaleString(), total: r().total.toLocaleString() })
                          : t("rinsight.scan_done", { scanned: r().scanned.toLocaleString(), bytes: formatBytes(r().totalBytes) })
                    : t("rinsight.scanning", { scanned: r().scanned.toLocaleString(), total: r().total.toLocaleString() })}
                </Text>
                <If when={!r().memorySupported}>
                  <Text class={`${FS.xs} text-warning`}>{t("rinsight.no_memory_usage")}</Text>
                </If>
              </View>

              <If when={r().scanned > 0}>
                <View class="gap-5">
                  <If when={r().memorySupported}>
                    <Section title={t("rinsight.biggest")}>
                      <KeyTable testID="ri-biggest" connId={id} keys={r().biggest} metric="bytes" />
                    </Section>
                  </If>
                  <Section title={t("rinsight.prefixes")}>
                    <Groups testID="ri-prefixes" groups={r().prefixes} r={r()} prefix />
                  </Section>
                  <View class="flex-row gap-6">
                    <View class="min-w-0 flex-1">
                      <Section title={t("rinsight.ttl")}>
                        <TtlDist r={r()} />
                      </Section>
                    </View>
                    <View class="min-w-0 flex-1">
                      <Section title={t("rinsight.types")}>
                        <Groups testID="ri-types" groups={r().types} r={r()} />
                      </Section>
                    </View>
                  </View>
                  <Section title={t("rinsight.hottest")}>
                    <If when={r().lfu} else={<Hint class={FS.xs}>{t("rinsight.hot_needs_lfu")}</Hint>}>
                      <KeyTable testID="ri-hottest" connId={id} keys={r().hottest} metric="freq" />
                    </If>
                  </Section>
                </View>
              </If>
            </View>
          )}
        </If>
      </Section>

      <Section title={t("rinsight.slowlog")}>
        <Loaded load={slowlog()} error={(error) => t("rinsight.slowlog_failed", { error })}>
          {(entries) => <SlowLog entries={entries} />}
        </Loaded>
      </Section>
    </Scroll>
  );
}
