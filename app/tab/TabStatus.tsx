// 状态栏里属于当前 tab 的部分:表浏览的 行数 · 列数 · 耗时(左),分页控件(右)。
// 对应 GPUI 版 workspace/chrome.rs 的 render_status_bar 中 table_stats 与 pager。

import { Text, View } from "nsh";
import { t } from "nsh/i18n";
import { workspace } from "../store/workspace";
import { Dot, IconButton } from "../ui/kit";
import { FS } from "../ui/text";
import { canNext, canPrev, gotoPage, modelOf, PAGE_SIZE, viewIndices } from "./model";

/** 表浏览 tab 的行列数与耗时(无错误且有结果时)。 */
export function TabStats() {
  const m = () => {
    const x = modelOf(workspace.activeTab());
    // Redis Stream 不按行列读取(编辑器里分页浏览),没有可报的行列数
    if (x?.redisValue()?.keyType === "stream") return undefined;
    return x && x.kind === "table" && x.err() === null && x.result() ? x : undefined;
  };
  return (
    <If when={m()}>
      {(x) => (
        <View class="flex-row items-center gap-2.5">
          <View class="h-3 w-px bg-hairline" />
          <Dot color="data" size={5} />
          <View class="flex-row items-center gap-1">
            <Text testID="stat-rows" class={`${FS.xs} font-mono font-bold text-foreground`}>
              {String(viewIndices(x()).length)}
            </Text>
            <Text class={`${FS.xs} text-muted-foreground`}>{t("workspace.rows_unit")}</Text>
            <Text class={`${FS.xs} font-mono font-bold text-foreground`}>{String(x().result()?.columns.length ?? 0)}</Text>
            <Text class={`${FS.xs} text-muted-foreground`}>{t("workspace.cols_unit")}</Text>
          </View>
          <If when={x().elapsedMs() !== null}>
            <Text class={`${FS.xs} font-mono text-fg-faint`}>{t("workspace.elapsed_ms", { ms: x().elapsedMs() ?? 0 })}</Text>
          </If>
        </View>
      )}
    </If>
  );
}

/** 分页控件(仅关系库的表浏览)。 */
export function Pager() {
  const m = () => {
    const x = modelOf(workspace.activeTab());
    return x && x.browseTable() !== null ? x : undefined;
  };
  return (
    <If when={m()}>
      {(x) => (
        <View class="flex-row items-center gap-1">
          <Text class={`${FS.xs} text-muted-foreground`}>{t("workspace.page_prefix")}</Text>
          <Text testID="page-no" class={`${FS.xs} font-bold text-foreground`}>
            {String(x().page() + 1)}
          </Text>
          <Text class={`${FS.xs} text-muted-foreground`}>{t("workspace.page_suffix", { size: PAGE_SIZE })}</Text>
          <IconButton
            icon="chevron-left"
            size={13}
            tooltip={t("workspace.prev_page")}
            disabled={!canPrev(x())}
            onPress={() => gotoPage(x(), Math.max(0, x().page() - 1))}
          />
          <IconButton
            icon="chevron-right"
            size={13}
            tooltip={t("workspace.next_page")}
            disabled={!canNext(x())}
            onPress={() => gotoPage(x(), x().page() + 1)}
          />
        </View>
      )}
    </If>
  );
}

/** 状态栏里属于 tab 的部分:左侧统计 + 右侧分页,中间撑开。外壳把它放在连接状态与版本号之间。 */
export function TabStatusView() {
  return (
    <View class="min-w-0 flex-1 flex-row items-center justify-between gap-2.5">
      <TabStats />
      <Pager />
    </View>
  );
}
