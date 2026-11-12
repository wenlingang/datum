// 主窗口工作区:顶栏 + [侧栏 | 标签页 | 详情栏 | AI 面板](可拖动宽度)+ 状态栏。
// 对应 GPUI 版 workspace/mod.rs 的 Render。标签页按连接隔离,见 store/workspace.ts。

import { View } from "nsh";
import { ResizablePanel, ResizablePanelGroup } from "nsh/base";
import { ChatPanel } from "../chat";
import { PANEL, setPanelWidth, workspace } from "../store/workspace";
import { DetailPanel, TabView } from "../tab";
import { installAppCommands } from "./commands";
import { DdlDialog } from "./DdlDialog";
import { EmptyWorkspace } from "./EmptyWorkspace";
import { Sidebar } from "./Sidebar";
import { StatusBar } from "./StatusBar";
import { TabStrip } from "./TabStrip";
import { TopBar } from "./TopBar";

/** 中间栏:标签条 + 当前 tab(无 tab 时为空白工作区)。 */
function Center() {
  return (
    <View class="flex-1 bg-background">
      <TabStrip />
      <View class="min-h-0 flex-1">
        <If when={workspace.activeTab()} else={<EmptyWorkspace />}>
          {(tab) => (
            // 切换 tab 时整块重建:每个 tab 的状态在 tab/ 的模型里,不在视图里
            <Each of={[tab()]} key={(x) => x.id}>
              {(x) => <TabView tab={x()} />}
            </Each>
          )}
        </If>
      </View>
    </View>
  );
}

/** 侧栏面板(普通布局与对话工作台共用)。 */
function SidebarPanel() {
  return (
    <ResizablePanel
      visible={workspace.sidebarOpen()}
      size={workspace.sidebarWidth()}
      minSize={PANEL.sidebar.min}
      maxSize={PANEL.sidebar.max}
      fixed
      onResize={({ size }) => setPanelWidth("sidebar", size)}
    >
      <Sidebar />
    </ResizablePanel>
  );
}

/** 普通布局:侧栏 | 标签页 | 详情 | AI 面板。 */
function Editing() {
  return (
    <ResizablePanelGroup handleClass="bg-border" handleAccessibilityLabel="resize">
      <SidebarPanel />
      <ResizablePanel minSize={240}>
        <Center />
      </ResizablePanel>
      <ResizablePanel
        visible={workspace.detailOpen()}
        size={workspace.detailWidth()}
        minSize={PANEL.detail.min}
        maxSize={PANEL.detail.max}
        fixed
        onResize={({ size }) => setPanelWidth("detail", size)}
      >
        <View class="flex-1 bg-background" testID="detail-panel">
          <DetailPanel />
        </View>
      </ResizablePanel>
      <ResizablePanel
        visible={workspace.aiOpen()}
        size={workspace.aiWidth()}
        minSize={PANEL.ai.min}
        maxSize={PANEL.ai.max}
        fixed
        onResize={({ size }) => setPanelWidth("ai", size)}
      >
        <View class="flex-1 bg-background" testID="ai-panel">
          <ChatPanel />
        </View>
      </ResizablePanel>
    </ResizablePanelGroup>
  );
}

/** 对话工作台:侧栏 | 铺满的 AI 对话(查询、结果、改数据都在对话里完成)。 */
function Conversing() {
  return (
    <ResizablePanelGroup handleClass="bg-border" handleAccessibilityLabel="resize">
      <SidebarPanel />
      <ResizablePanel minSize={360}>
        <View class="flex-1 bg-background" testID="ai-workbench">
          <ChatPanel wide />
        </View>
      </ResizablePanel>
    </ResizablePanelGroup>
  );
}

export function Workspace() {
  installAppCommands();
  return (
    <View class="flex-1 bg-background">
      <TopBar />
      <View class="min-h-0 flex-1">
        <If when={workspace.aiFullscreen()} else={<Editing />}>
          <Conversing />
        </If>
      </View>
      <StatusBar />
      <DdlDialog />
    </View>
  );
}
