// 自动更新:检查、下载、安装重启,以及顶栏设置按钮的红点。对应 GPUI 版 store.rs 的 UpdateStore。
//
// 打包运行时启动 10 秒后首检,之后每 4 小时静默轮询一次;开发运行(`nsh run`)里
// `checkForUpdate` 恒为 null、`shellInfo().version` 为 null,视为「不支持自动更新」。

import { openUrl, signal } from "nsh";
import { t } from "nsh/i18n";
import { type AppUpdate, checkForUpdate, downloadUpdate, installAndRestart, shellInfo } from "nsh/updater";
import { prefsStore } from "../lib/native";
import { setUpdateAvailable } from "./workspace";

export type UpdateStatus =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "upToDate" }
  | { kind: "available"; update: AppUpdate }
  /** 下载进度百分比;服务器没给总大小时为 null */
  | { kind: "downloading"; pct: number | null }
  | { kind: "installing" }
  | { kind: "failed"; error: string };

const FIRST_CHECK_MS = 10_000;
const POLL_INTERVAL_MS = 4 * 60 * 60 * 1000;

const statusSignal = signal<UpdateStatus>({ kind: "idle" });

/** 状态的唯一写入口:同时把「有更新」同步给顶栏设置按钮的红点。 */
const status = Object.assign(() => statusSignal(), {
  set(s: UpdateStatus) {
    statusSignal.set(s);
    setUpdateAvailable(s.kind === "available" || s.kind === "downloading" || s.kind === "installing");
  },
});

function shellVersion(): string | null {
  try {
    return shellInfo().version ?? null;
  } catch {
    return null;
  }
}

/** 是否支持应用内自更新(打包、且 nsh.json 配了 updater 才支持)。 */
const supported = shellVersion() != null;

export const updates = {
  status: () => status(),
  supported: () => supported,
  /** 当前版本:打包时的版本,开发运行回退到原生模块的版本号。 */
  version: () => shellVersion() ?? prefsStore.appVersion(),
};

const busy = (s: UpdateStatus) => s.kind === "checking" || s.kind === "downloading" || s.kind === "installing";

/** 有可用更新、正在下载或安装:顶栏设置按钮亮红点。 */
export function hasUpdate(): boolean {
  const k = status().kind;
  return k === "available" || k === "downloading" || k === "installing";
}

function message(e: unknown) {
  return e instanceof Error ? e.message : String(e);
}

/** 用户手动检查。 */
export async function checkUpdates() {
  if (busy(status())) return;
  status.set({ kind: "checking" });
  try {
    const found = await checkForUpdate();
    status.set(found ? { kind: "available", update: found } : { kind: "upToDate" });
  } catch (e) {
    status.set({ kind: "failed", error: message(e) || t("store.update_info_failed") });
  }
}

/** 后台静默检查:失败与无更新都不改变状态。 */
async function checkSilently() {
  if (busy(status())) return;
  try {
    const found = await checkForUpdate();
    // 请求在途期间用户可能已开始手动检查/下载,落状态前复查
    if (found && !busy(status())) status.set({ kind: "available", update: found });
  } catch {
    // 静默
  }
}

/** 下载并安装可用更新,完成后重启;不能就地安装(系统目录等)时打开下载页。 */
export async function installUpdate() {
  const s = status();
  if (s.kind !== "available") return;
  if (!s.update.canInstall) {
    await openUrl(s.update.url).catch(() => {});
    return;
  }
  status.set({ kind: "downloading", pct: null });
  try {
    await downloadUpdate(({ received, total }) => {
      status.set({ kind: "downloading", pct: total ? Math.min(100, Math.round((received / total) * 100)) : null });
    });
    status.set({ kind: "installing" });
    // 成功时进程直接重启,这里只会在失败时返回
    await installAndRestart();
  } catch (e) {
    status.set({ kind: "failed", error: message(e) });
  }
}

let polling: { first: ReturnType<typeof setTimeout>; every?: ReturnType<typeof setInterval> } | null = null;

/** 启动后台轮询(打包运行才生效;重复调用无副作用)。返回停止函数。 */
export function startUpdatePolling(): () => void {
  if (!supported || polling) return stopUpdatePolling;
  const first = setTimeout(() => {
    void checkSilently();
    if (polling) polling.every = setInterval(() => void checkSilently(), POLL_INTERVAL_MS);
  }, FIRST_CHECK_MS);
  polling = { first };
  return stopUpdatePolling;
}

export function stopUpdatePolling() {
  if (!polling) return;
  clearTimeout(polling.first);
  if (polling.every) clearInterval(polling.every);
  polling = null;
}

// 与 GPUI 版一致:store 建立即开始轮询
startUpdatePolling();
