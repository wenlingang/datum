// 标签页主体:查询 tab(编辑器 + 工具条 / 状态行 + 结果,上下可拖动)、表浏览 tab(纯网格)、
// ER 图占位;MongoDB 不支持,只显示一句提示。对应 GPUI 版 tab/mod.rs 的 Render。

import { effect, Text, untracked, View } from "nsh";
import { ResizablePanel, ResizablePanelGroup } from "nsh/base";
import { t } from "nsh/i18n";
import type { WorkTab } from "../store/workspace";
import { Icon } from "../ui/kit";
import { FS } from "../ui/text";
import { CellDialog } from "./CellDialog";
import { FilterBar } from "./FilterBar";
import { conns } from "../store/conns";
import { modelOf, refresh, type TabModel } from "./model";
import { Results, StatusLine } from "./Results";
import { Diagnostics } from "./Diagnostics";
import { RedisInsight } from "./RedisInsight";
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

function ModelView(props: { m: TabModel }) {
  const m = props.m;
  // 恢复会话的表浏览 tab:显示出来且连接就绪后才去查数据
  effect(() => {
    if (!m.pendingLoad || !conns.isConnected(m.connId)) return;
    m.pendingLoad = false;
    untracked(() => refresh(m));
  });
  return (
    <View testID="work-tab" class="flex-1 bg-background">
      <If when={m.filterOpen() && m.result() !== null}>
        <FilterBar m={m} />
      </If>
      <View class="min-h-0 flex-1">
        <If when={m.kind === "query"} else={<Results m={m} />}>
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
  const m = modelOf(props.tab);
  if (props.tab.kind === "redis") return <RedisInsight connId={props.tab.connId} />;
  if (props.tab.kind === "diag") return <Diagnostics connId={props.tab.connId} />;
  if (props.tab.kind === "erd" || !m) return <Placeholder icon="network" text={t("erd.wip")} />;
  if (m.dbKind === "mongo") return <Placeholder icon="db-mongo" text={t("tab.mongo_unsupported")} />;
  return <ModelView m={m} />;
}
