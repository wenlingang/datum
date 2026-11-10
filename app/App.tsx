// Datum 入口:应用偏好、挂载工作区。页面逻辑在 workspace/、tab/、dialogs/、chat/ 里。

import { mount, Text, View } from "nsh";
import { reloadConnections } from "./store/conns";
import { apply as applyPrefs } from "./store/prefs";
import { dismissToast, toasts } from "./store/toast";
import { Icon } from "./ui/kit";
import { FS } from "./ui/text";
import { Workspace } from "./workspace/Workspace";

applyPrefs();
reloadConnections();

const TOAST_ICON = { success: "circle-check", error: "circle-x", info: "info" } as const;
const TOAST_COLOR = { success: "success", error: "destructive", info: "info" } as const;

function Toasts() {
  return (
    <View class="absolute bottom-10 right-4 w-80 gap-2" style={{ pointerEvents: "box-none" }}>
      <Each of={toasts()} key={(t) => t.id}>
        {(item) => (
          <View
            onPress={() => dismissToast(item().id)}
            class="flex-row items-start gap-2 rounded-(--radius-lg) border border-border bg-popover px-3 py-2.5 shadow-lg"
          >
            <Icon name={TOAST_ICON[item().kind]} size={15} color={TOAST_COLOR[item().kind]} />
            <Text class={`${FS.sm} flex-1 text-popover-foreground`}>{item().text}</Text>
          </View>
        )}
      </Each>
    </View>
  );
}

function App() {
  return (
    <View class={`flex-1 bg-background ${FS.sm}`}>
      <Workspace />
      <Toasts />
    </View>
  );
}

mount(App);
