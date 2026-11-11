// 标签页主体:查询 tab(编辑器 + 工具条 / 状态行 + 结果,上下可拖动)、表浏览 tab(纯网格)、
// MongoDB 不支持,只显示一句提示;ER 图 tab 交给 erd/ErdView。对应 GPUI 版 tab/mod.rs 的 Render。

import { effect, Text, untracked, View } from "nsh";
import { ResizablePanel, ResizablePanelGroup } from "nsh/base";
import { t } from "nsh/i18n";
import type { WorkTab } from "../store/workspace";
import { Icon } from "../ui/kit";
import { FS } from "../ui/text";
import { ErdView } from "../erd/ErdView";
import { erdModelOf } from "../erd/model";
import { CellDialog } from "./CellDialog";
import { FilterBar } from "./FilterBar";
import { conns, refreshTables } from "../store/conns";
import { closeTab } from "../store/workspace";
import { ErrorText } from "../ui/error";
import { Hint } from "../ui/kit";
import { openTable } from "./index";
import { modelOf, refresh, refreshRedisKey, type TabModel } from "./model";
import { RedisKeyEditor } from "./RedisKeyEditor";
import { Results, StatusLine } from "./Results";
import { Diagnostics } from "./Diagnostics";
import { SchemaDiff } from "./SchemaDiff";
import { RedisInsight } from "./RedisInsight";
import { RedisLive } from "./RedisLive";
import { RedisLua } from "./RedisLua";
import { luaModelOf } from "./luaModel";
import { RiskDialog } from "./RiskDialog";
import { UndoDialog } from "./UndoDialog";
import { ParamsDialog } from "./ParamsDialog";
import { SqlEditor } from "./SqlEditor";
import { Toolbar } from "./Toolbar";

/** 尚未移植的形态:居中图标 + 一行说明。 */
export function Placeholder(props: { icon: "network" | "db-mongo"; text: string }) {
  return (
    <View class="flex-1 items-center justify-center gap-3 bg-editor p-6">
      <Icon name={props.icon} size={40} color="fg-faint" />
      <Text class={`${FS.sm} font-mono font-bold text-fg-soft`}>{props.text}</Text>
    </View>
  );
}

function QueryBody(props: { m: TabModel }) {
  return (
    <ResizablePanelGroup axis="vertical" class="flex-1" handleClass="bg-border">
      <ResizablePanel size={220} minSize={90}>
        <View class="flex-1">
          <SqlEditor m={props.m} autoFocus />
          <Toolbar m={props.m} />
        </View>
      </ResizablePanel>
      <ResizablePanel minSize={90}>
        <View class="flex-1">
          <StatusLine m={props.m} />
          <Results m={props.m} />
        </View>
      </ResizablePanel>
    </ResizablePanelGroup>
  );
}

/** Redis key 标签页:按类型的编辑器(值与详情仍由模型读取,详情栏照旧显示)。 */
function RedisKeyBody(props: { m: TabModel; tabId: string }) {
  const m = props.m;
  const key = m.table ?? "";
  return (
    <View class="min-h-0 flex-1 px-4 py-3">
      <If when={m.err() !== null && m.redisValue() === null}>
        <ErrorText testID="query-error" mono tone="text-destructive" text={m.err() ?? ""} />
      </If>
      <If when={m.redisValue() !== null} else={<If when={m.err() === null}><Hint>{t("tab.running")}</Hint></If>}>
        <RedisKeyEditor
          connId={m.connId}
          node={null}
          value={m.redisValue()}
          detail={m.redisDetail()}
          reload={() => refreshRedisKey(m, key)}
          onRenamed={(next) => {
            // 新名字开一个标签页,关掉旧名字的;侧栏的 key 列表重读
            closeTab(props.tabId);
            openTable(m.connId, next);
            void refreshTables(m.connId);
          }}
        />
      </If>
    </View>
  );
}

function ModelView(props: { m: TabModel; tabId: string }) {
  const m = props.m;
  // 恢复会话的表浏览 tab:显示出来且连接就绪后才去查数据
  effect(() => {
    if (!m.pendingLoad || !conns.isConnected(m.connId)) return;
    m.pendingLoad = false;
    untracked(() => refresh(m));
  });
  return (
    <View testID="work-tab" class="flex-1 bg-background">
      {/* Redis key 标签页的编辑器自带筛选,不显示网格的筛选条 */}
      <If when={m.filterOpen() && m.result() !== null && !(m.dbKind === "redis" && m.kind === "table")}>
        <FilterBar m={m} />
      </If>
      <View class="min-h-0 flex-1">
        <If
          when={m.kind === "query"}
          else={m.dbKind === "redis" ? <RedisKeyBody m={m} tabId={props.tabId} /> : <Results m={m} />}
        >
          <QueryBody m={m} />
        </If>
      </View>
      <RiskDialog m={m} />
      <UndoDialog m={m} />
      <ParamsDialog m={m} />
      <CellDialog m={m} />
    </View>
  );
}

export function TabBody(props: { tab: WorkTab }) {
  const erd = erdModelOf(props.tab.state);
  if (erd) return <ErdView m={erd} />;
  const m = modelOf(props.tab);
  if (props.tab.kind === "redis") return <RedisInsight connId={props.tab.connId} />;
  if (props.tab.kind === "rlive") return <RedisLive connId={props.tab.connId} />;
  if (props.tab.kind === "rlua") {
    const lm = luaModelOf(props.tab);
    return lm ? <RedisLua m={lm} /> : <Placeholder icon="network" text={t("erd.wip")} />;
  }
  if (props.tab.kind === "diag") return <Diagnostics connId={props.tab.connId} />;
  if (props.tab.kind === "diff") return <SchemaDiff connId={props.tab.connId} />;
  if (!m) return <Placeholder icon="network" text={t("erd.wip")} />;
  if (m.dbKind === "mongo") return <Placeholder icon="db-mongo" text={t("tab.mongo_unsupported")} />;
  return <ModelView m={m} tabId={props.tab.id} />;
}
