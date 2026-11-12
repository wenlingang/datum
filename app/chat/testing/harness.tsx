// 测试/开发入口的外框:左侧占位、右侧 AI 面板,装上假的依赖后挂载。

import { type JSX, mount, Text, View } from "nsh";
import { dismissToast, toasts } from "../../store/toast";
import { FS } from "../../ui/text";
import { ChatPanel } from "../index";
import { type FakeOptions, installFakes } from "../fixtures";

function Toasts() {
  return (
    <View class="absolute bottom-4 left-4 w-72 gap-2" style={{ pointerEvents: "box-none" }}>
      <Each of={toasts()} key={(t) => t.id}>
        {(item) => (
          <View onPress={() => dismissToast(item().id)} class="rounded-(--radius) border border-border bg-popover px-3 py-2">
            <Text class={`${FS.sm} text-popover-foreground`}>{item().text}</Text>
          </View>
        )}
      </Each>
    </View>
  );
}

/** `left`:左侧占位区里放的内容(模拟编辑器一侧的入口)。 */
export function mountChat(options: FakeOptions = {}, before?: () => void, left?: () => JSX.Element) {
  const record = installFakes(options);
  before?.();
  mount(() => (
    <View class="flex-1 flex-row bg-background">
      <If when={!options.wide}>
        <View class="flex-1">{left?.()}</View>
      </If>
      <View class={options.wide ? "flex-1" : "w-[380px]"}>
        <ChatPanel wide={options.wide} />
      </View>
      <Toasts />
    </View>
  ));
  return record;
}
