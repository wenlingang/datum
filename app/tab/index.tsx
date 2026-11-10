// tab/ 对外接口(外壳、AI 面板只经这里与标签页交互)。
// 【桩】由「查询与表浏览」子任务实现;签名是约定,改动需同步调用方。

import { Text, View } from "nsh";
import { toast } from "../store/toast";
import type { WorkTab } from "../store/workspace";

/** 标签页主体(编辑器 + 结果 / 表浏览网格 / ER 图占位)。 */
export function TabView(props: { tab: WorkTab }) {
  return (
    <View class="flex-1 items-center justify-center">
      <Text class="text-muted-foreground">{props.tab.title()}</Text>
    </View>
  );
}

/** 右侧详情栏:行详情 / 表结构 / Redis key 详情(跟随当前 tab)。 */
export function DetailPanel() {
  return <View class="flex-1" />;
}

/** 状态栏里属于当前 tab 的部分:行列数、耗时、分页控件等。 */
export function TabStatus() {
  return <View />;
}

/** 打开(或聚焦已打开的)表浏览 tab;Redis 为 key,Mongo 为集合。 */
export function openTable(connId: string, table: string): WorkTab | undefined {
  toast.info(`TODO openTable ${connId} ${table}`);
  return undefined;
}

/** 在当前连接下新开查询 tab,可预填 SQL。未选连接时提示并返回 undefined。 */
export function newQueryTab(sql?: string): WorkTab | undefined {
  toast.info(`TODO newQueryTab ${sql ?? ""}`);
  return undefined;
}

/** 集合右键「插入文档」:打开集合 tab 并弹出新增文档编辑器。 */
export function insertDocument(connId: string, coll: string) {
  toast.info(`TODO insertDocument ${connId} ${coll}`);
}

/** 打开 ER 图 tab(画布 TODO)。 */
export function openErd(connId: string) {
  toast.info(`TODO openErd ${connId}`);
}

/** AI 生成的 SQL「编辑器运行」:只在 connId 上执行;`confirmed` 为卡片已做过破坏性确认。 */
export function insertAndRun(sql: string, connId: string, confirmed: boolean) {
  toast.info(`TODO insertAndRun ${connId} ${confirmed} ${sql}`);
}

/** 当前 tab 编辑器里选中的文本(引用给 AI)。 */
export function activeSelectionText(): string {
  return "";
}

/** 快捷键/菜单命令转给当前 tab:运行、格式化、解释、历史、过滤、提交编辑、刷新。 */
export type TabCommand = "run" | "format" | "explain" | "history" | "filter" | "commit" | "refresh" | "sendToAi";
export function runTabCommand(cmd: TabCommand) {
  toast.info(`TODO ${cmd}`);
}
