// dialogs/ 对外接口:打开各对话框的函数 + 挂在根部的 DialogHost。
// 状态在 ./state.ts(每种对话框一个请求信号),对话框本身用 nsh/base 的 Dialog 渲染。

import type { JSX } from "nsh";
import type { Connection } from "../lib/types";
import { ConnectionFormDialog } from "./ConnectionForm";
import { ImportDialog } from "./Import";
import { SettingsDialog } from "./Settings";
import {
  closeConnForm,
  closeImport,
  closeSettings,
  dialogState,
  showConnForm,
  showImport,
  showSettings,
  type SettingsSection,
} from "./state";

export { aiSettingsRevision } from "./aiSettings";

/** 新建 / 编辑连接。`conn` 为空是新建;`database` 预填库名(切库后编辑)。 */
export function openConnectionForm(conn?: Connection, opts?: { database?: string }) {
  showConnForm(conn, opts?.database);
}

const SECTION: Record<"general" | "ai" | "about", SettingsSection> = {
  general: "appearance",
  ai: "ai",
  about: "about",
};

/** 设置(偏好、AI 服务商、知识库、关于与更新)。 */
export function openSettings(section?: "general" | "ai" | "about") {
  showSettings(SECTION[section ?? "ai"]);
}

/** 导入 SQL 文件到某连接。 */
export function openImport(connId: string) {
  showImport(connId);
}

/** 挂在根部的对话框宿主(各对话框的 Dialog 在这里渲染)。 */
export function DialogHost(): JSX.Element {
  return (
    <>
      <Each of={dialogState.connForm() ? [dialogState.connForm()!] : []} key={(r) => r.key}>
        {(r) => <ConnectionFormDialog open conn={r().conn} database={r().database} onClose={closeConnForm} />}
      </Each>
      <Each of={dialogState.settings() ? [dialogState.settings()!] : []} key={(r) => r.key}>
        {(r) => <SettingsDialog open section={r().section} onClose={closeSettings} />}
      </Each>
      <Each of={dialogState.import() ? [dialogState.import()!] : []} key={(r) => r.key}>
        {(r) => <ImportDialog open connId={r().connId} onClose={closeImport} />}
      </Each>
    </>
  );
}
