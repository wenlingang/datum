// 对话框的开合状态:同一时刻每种对话框至多一个。DialogHost 读这里决定渲染什么。
// 每次打开都换一个新的 `key`,对话框内容据此重建,草稿不会串到下一次。

import { signal } from "nsh";
import type { Connection } from "../lib/types";

export type SettingsSection = "ai" | "knowledge" | "appearance" | "about";

export interface ConnFormRequest {
  key: number;
  conn?: Connection;
  database?: string;
}

export interface SettingsRequest {
  key: number;
  section: SettingsSection;
}

/** 导入对话框的页:SQL 语句,或 CSV / JSON 数据文件(仅 SQLite)。 */
export type ImportMode = "sql" | "data";

export interface ImportRequest {
  key: number;
  connId: string;
  mode: ImportMode;
}

let nextKey = 1;

const connForm = signal<ConnFormRequest | null>(null);
const settings = signal<SettingsRequest | null>(null);
const importReq = signal<ImportRequest | null>(null);

export const dialogState = {
  connForm: () => connForm(),
  settings: () => settings(),
  import: () => importReq(),
};

export function showConnForm(conn?: Connection, database?: string) {
  connForm.set({ key: nextKey++, conn, database });
}
export const closeConnForm = () => connForm.set(null);

export function showSettings(section: SettingsSection) {
  settings.set({ key: nextKey++, section });
}
export const closeSettings = () => settings.set(null);

export function showImport(connId: string, mode: ImportMode = "sql") {
  importReq.set({ key: nextKey++, connId, mode });
}
export const closeImport = () => importReq.set(null);
