// Datum 入口:应用偏好、挂载工作区与对话框宿主。页面逻辑在 workspace/、tab/、dialogs/、chat/ 里;
// 应用菜单与快捷键由 workspace/commands.ts 在工作区挂载时安装。

import { mount, Text, View } from "nsh";
import { Toast, ToastStack } from "nsh/base";
import { t } from "nsh/i18n";
import { DialogHost } from "./dialogs";
import { persistSession, restoreSession } from "./tab/session";
import { loadConversations, persistConversations } from "./store/ai";
import { reloadConnections } from "./store/conns";
import { apply as applyPrefs } from "./store/prefs";
import { dismissNewestToast, dismissToast, setToastsExpanded, toasts, toastsExpanded } from "./store/toast";
import { CopyButton } from "./ui/error";
import { Icon, IconButton } from "./ui/kit";
import { FS } from "./ui/text";
import { Workspace } from "./workspace/Workspace";

applyPrefs();
reloadConnections();
// 恢复上次打开的标签与内容,之后有变化就保存
restoreSession();
persistSession();
// AI 对话同样跨重启保留
loadConversations();
persistConversations();

const TOAST_ICON = { success: "circle-check", error: "circle-alert", info: "info" } as const;
const TOAST_COLOR = { success: "success", error: "destructive", info: "info" } as const;

const TOAST_CLASS = "flex-row items-start gap-2 rounded-(--radius-lg) border border-border bg-popover py-2.5 shadow-lg transition-all duration-200 ease-out";

/** 右下角的提示栈:收起时叠成几层,指针停上去展开成列表并暂停计时,此时 Esc 关掉最新一条。 */
function Toasts() {
  return (
    <View class="absolute bottom-10 right-4 w-80" style={{ pointerEvents: "box-none" }}>
      {/* Esc 只在指针停在提示上时关最新一条:栈一直听 Esc 的话会抢走对话框、菜单的 Esc */}
      <ToastStack
        placement="bottomRight"
        class="w-full"
        closeOnEscape={toastsExpanded()}
        onExpandedChange={(e) => setToastsExpanded(e.expanded)}
        onCancel={dismissNewestToast}
      >
        <Each of={toasts()} key={(t) => t.id}>
          {(item) => (
            <If
              when={item().kind === "error"}
              else={
                <Toast
                  transitionStatus={item().status}
                  accessibilityLabel={item().text}
                  onDismissRequest={() => dismissToast(item().id)}
                  class={`${TOAST_CLASS} px-3`}
                  startingClass="opacity-0 translate-y-4"
                  endingClass="opacity-0 scale-95"
                  swipingClass="transition-none"
                >
                  <View onPress={() => dismissToast(item().id)} class="min-w-0 flex-1 flex-row items-start gap-2">
                    <Icon name={TOAST_ICON[item().kind]} size={15} color={TOAST_COLOR[item().kind]} />
                    <Text class={`${FS.sm} flex-1 text-popover-foreground`}>{item().text}</Text>
                  </View>
                </Toast>
              }
            >
              {/* 错误:文字可选中、可一键复制,点 × 关闭(不滑动关闭、点一下也不消失,免得选字时误关) */}
              <Toast
                testID="toast-error"
                transitionStatus={item().status}
                accessibilityLabel={item().text}
                swipeDirection={null}
                class={`${TOAST_CLASS} pl-3 pr-1.5`}
                startingClass="opacity-0 translate-y-4"
                endingClass="opacity-0 scale-95"
              >
                <Icon name={TOAST_ICON.error} size={15} color={TOAST_COLOR.error} class="mt-0.5" />
                <View selectable class="min-w-0 flex-1">
                  <Text class={`${FS.sm} text-popover-foreground`}>{item().text}</Text>
                </View>
                <CopyButton text={item().text} />
                <IconButton small icon="x" tooltip={t("error_text.close")} onPress={() => dismissToast(item().id)} />
              </Toast>
            </If>
          )}
        </Each>
      </ToastStack>
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
