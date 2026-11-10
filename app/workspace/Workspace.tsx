// 主窗口工作区(占位,待移植 GPUI 版 workspace/)。

import { Text, View } from "nsh";
import { conns } from "../store/conns";
import { DbIcon, EnvBadge } from "../ui/kit";
import { FS } from "../ui/text";

export function Workspace() {
  return (
    <View class="flex-1 gap-1 p-4">
      <Each of={conns.list()} key={(c) => c.id}>
        {(c) => (
          <View class="flex-row items-center gap-2">
            <DbIcon kind={c().kind} />
            <Text class={`${FS.sm} text-foreground`}>{c().name}</Text>
            <EnvBadge env={c().env} />
          </View>
        )}
      </Each>
    </View>
  );
}
