// Datum 入口:应用偏好、挂载工作区与对话框宿主。页面逻辑在 workspace/、tab/、dialogs/、chat/ 里;
// 应用菜单与快捷键由 workspace/commands.ts 在工作区挂载时安装。

import { mount, Text, View } from "nsh";
import { t } from "nsh/i18n";
import { DialogHost } from "./dialogs";
import { persistSession, restoreSession } from "./tab/session";
import { reloadConnections } from "./store/conns";
import { apply as applyPrefs } from "./store/prefs";
import { dismissToast, toasts } from "./store/toast";
import { CopyButton } from "./ui/error";
import { Icon, IconButton } from "./ui/kit";
import { FS } from "./ui/text";
import { Workspace } from "./workspace/Workspace";

applyPrefs();
reloadConnections();
// 恢复上次打开的标签与内容,之后有变化就保存
restoreSession();
persistSession();

const TOAST_ICON = { success: "circle-check", error: "circle-x", info: "info" } as const;
const TOAST_COLOR = { success: "success", error: "destructive", info: "info" } as const;

function Toasts() {
  return (
    <View class="absolute bottom-10 right-4 w-80 gap-2" style={{ pointerEvents: "box-none" }}>
      <Each of={toasts()} key={(t) => t.id}>
        {(item) => (
          <If
            when={item().kind === "error"}
            else={
              <View
                onPress={() => dismissToast(item().id)}
                class="flex-row items-start gap-2 rounded-(--radius-lg) border border-border bg-popover px-3 py-2.5 shadow-lg"
              >
                <Icon name={TOAST_ICON[item().kind]} size={15} color={TOAST_COLOR[item().kind]} />
                <Text class={`${FS.sm} flex-1 text-popover-foreground`}>{item().text}</Text>
              </View>
            }
          >
            {/* 错误:文字可选中、可一键复制,点 × 关闭(不再点一下就消失,免得选字时误关) */}
            <View
              testID="toast-error"
              class="flex-row items-start gap-2 rounded-(--radius-lg) border border-border bg-popover py-2.5 pl-3 pr-1.5 shadow-lg"
            >
              <Icon name={TOAST_ICON.error} size={15} color={TOAST_COLOR.error} class="mt-0.5" />
              <View selectable class="min-w-0 flex-1">
                <Text class={`${FS.sm} text-popover-foreground`}>{item().text}</Text>
              </View>
              <CopyButton text={item().text} />
              <IconButton icon="x" size={13} class="p-1" tooltip={t("error_text.close")} onPress={() => dismissToast(item().id)} />
            </View>
          </If>
        )}
      </Each>
    </View>
  );
}

function App() {
  return (
    <View class={`flex-1 bg-background ${FS.sm}`}>
      <Workspace />
      <DialogHost />
      <Toasts />
    </View>
  );
}

mount(App);
