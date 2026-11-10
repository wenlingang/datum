// dialogs/ 对外接口。
// 【桩】由「对话框」子任务实现;签名是约定,改动需同步调用方。

import type { JSX } from "nsh";
import type { Connection } from "../lib/types";
import { toast } from "../store/toast";

/** 新建 / 编辑连接。`conn` 为空是新建;`database` 预填库名(切库后编辑)。 */
export function openConnectionForm(conn?: Connection, opts?: { database?: string }) {
  toast.info(`TODO connection form ${conn?.name ?? "new"} ${opts?.database ?? ""}`);
}

/** 设置(偏好、AI 服务商、知识库、关于与更新)。 */
export function openSettings(section?: "general" | "ai" | "about") {
  toast.info(`TODO settings ${section ?? ""}`);
}

/** 导入 SQL 文件到某连接。 */
export function openImport(connId: string) {
  toast.info(`TODO import ${connId}`);
}

/** 挂在根部的对话框宿主(各对话框的 Dialog 在这里渲染)。 */
export function DialogHost(): JSX.Element {
  return null as unknown as JSX.Element;
}
