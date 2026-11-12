// 应用命令:按钮、应用菜单、快捷键派发同一个函数,行为只实现一次。
// 对应 GPUI 版 actions.rs(键位)、main.rs(应用菜单)与 workspace/mod.rs 的 on_action。

import { effect, keymap, platform } from "nsh";
import { currentWindow, isAvailable, Menu, type MenuItem, type WindowRef } from "nsh/desktop";
import { t } from "nsh/i18n";
import { openSettings } from "../dialogs";
import { supportsSql } from "../lib/types";
import { conns, loadTables } from "../store/conns";
import { toast } from "../store/toast";
import { closeTab, toggleAi, toggleAiFullscreen, toggleDetail, toggleSidebar, workspace } from "../store/workspace";
import { newQueryTab, openErd, runTabCommand } from "../tab";

let mainWindow: WindowRef | null = null;

/** 新建查询 tab(⌘T)。 */
export function newQuery() {
  newQueryTab();
}

/** ⌘W:当前连接有 tab 则关它;全局无任何 tab 才关窗口(macOS 只隐藏)。 */
export function closeActiveTab() {
  const tab = workspace.activeTab();
  if (tab) {
    closeTab(tab.id);
  } else if (workspace.tabs().length === 0) {
    hideOrCloseWindow();
  }
}

function hideOrCloseWindow() {
  if (!mainWindow) return;
  if (platform.os === "macos") mainWindow.hide();
  else mainWindow.close();
}

/** ⌘R:刷新当前 tab,并重新加载当前连接的对象列表(已加载过的才刷)。 */
export function refresh() {
  if (workspace.activeTab()) runTabCommand("refresh");
  const id = conns.activeId();
  if (id && conns.tables(id)) void loadTables(id);
}

/** 打开当前连接的关系图(Redis / Mongo 没有关系图)。 */
export function openErdForActive() {
  const conn = conns.active();
  if (!conn || !supportsSql(conn.kind)) {
    toast.error(t("workspace.select_conn_first"));
    return;
  }
  openErd(conn.id);
}

export { toggleAi, toggleAiFullscreen, toggleDetail, toggleSidebar };

/** 应用菜单(切换语言后随 t() 自动重建)。菜单项的 shortcut 即全局快捷键。 */
function applicationMenu(): MenuItem[] {
  return [
    {
      label: "Datum",
      submenu: [
        { label: t("menu.settings"), shortcut: "mod+,", onSelect: () => openSettings() },
        { type: "separator" },
        { label: t("menu.hide"), role: "hide" },
        { label: t("menu.quit"), role: "quit" },
      ],
    },
    {
      label: t("menu.file"),
      submenu: [
        { label: t("menu.new_query"), shortcut: "mod+t", onSelect: newQuery },
        { label: t("menu.close_tab"), shortcut: "mod+w", onSelect: closeActiveTab },
      ],
    },
    // 编辑:输入框与编辑器的剪切 / 复制 / 粘贴等由系统角色提供(GPUI 版由控件自己处理)
    {
      label: t("menu.edit"),
      submenu: [
        { role: "undo" },
        { role: "redo" },
        { type: "separator" },
        { role: "cut" },
        { role: "copy" },
        { role: "paste" },
        { role: "selectAll" },
      ],
    },
    {
      label: t("menu.view"),
      submenu: [
        { label: t("menu.toggle_sidebar"), shortcut: "mod+b", onSelect: toggleSidebar },
        { label: t("menu.toggle_detail"), onSelect: toggleDetail },
        { label: t("menu.toggle_ai"), onSelect: toggleAi },
        { label: t("menu.ai_workbench"), shortcut: "mod+j", onSelect: toggleAiFullscreen },
        { label: t("menu.erd"), onSelect: openErdForActive },
        { type: "separator" },
        { label: t("menu.refresh"), shortcut: "mod+r", onSelect: refresh },
      ],
    },
  ];
}

/**
 * 在工作区组件里调用一次:装应用菜单、绑定其余快捷键、macOS 上关窗口只隐藏。
 * 快捷键随组件卸载而解除。键位与 GPUI 版 actions.rs 一致;编辑器内的 ⌘↵ / ⌘⇧A 由 tab/ 自己绑定。
 */
export function installAppCommands() {
  effect(() => {
    Menu.setApplicationMenu(applicationMenu());
  });
  keymap.bind({
    "mod+y": () => runTabCommand("history"),
    "mod+e": () => runTabCommand("explain"),
    "mod+f": () => runTabCommand("filter"),
    "mod+s": () => runTabCommand("commit"),
    "mod+shift+f": () => runTabCommand("format"),
  });
  if (!isAvailable) return;
  try {
    mainWindow = currentWindow();
  } catch {
    mainWindow = null;
  }
  // macOS 惯例:点关闭只隐藏窗口,留在 Dock;点 Dock 图标唤回,工作区状态不丢
  if (mainWindow && platform.os === "macos") {
    const win = mainWindow;
    win.set({ closeOnPress: false });
    win.on("closePress", () => win.hide());
  }
}
