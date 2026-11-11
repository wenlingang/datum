// 对话框的临时验证页(入口是 app/DialogsDev.tsx:nsh 要求入口与 nsh.json 同目录)。
// `DATUM_CONFIG_DIR=<临时目录> nsh run DialogsDev.tsx`。
// 几个按钮分别打开各对话框,底部列出已保存的连接与当前偏好,便于核对保存结果。

import { Store } from "datum/native";
import { mount, Text, View } from "nsh";
import { conns, reloadConnections } from "../../store/conns";
import { apply as applyPrefs, currentPrefs, updatePrefs } from "../../store/prefs";
import { hasUpdate } from "../../store/update";
import { Btn, DbIcon, EnvBadge } from "../../ui/kit";
import { FS } from "../../ui/text";
import { DialogHost, openConnectionForm, openImport, openSettings } from "../index";

/** 启动:应用偏好、读连接列表(与 App.tsx 相同)。 */
export function bootDev() {
  applyPrefs();
  reloadConnections();
  // 截图用:往配置目录的 dev-cmd.txt 写 conn|edit|settings|general|about|import|light|dark
  // 即打开对应对话框 / 切换明暗(每 500ms 读一次;同一行不重复执行,可加 `#n` 区分)
  const cmdFile = `${Store.configDir()}/dev-cmd.txt`;
  let last = "";
  setInterval(() => {
    let cmd = "";
    try {
      cmd = Store.readSqlFile(cmdFile).trim();
    } catch {
      return;
    }
    if (!cmd || cmd === last) return;
    last = cmd;
    runDevCommand(cmd.split("#")[0]);
  }, 500);
}

function runDevCommand(which: string) {
  const first = conns.list()[0];
  if (which === "conn") openConnectionForm();
  else if (which === "edit" && first) openConnectionForm(first);
  else if (which === "settings") openSettings("ai");
  else if (which === "general") openSettings("general");
  else if (which === "about") openSettings("about");
  else if (which === "import" && first) openImport(first.id);
  else if (which === "light" || which === "dark") updatePrefs({ theme: which });
}

export function Dev() {
  const first = () => conns.list()[0];
  return (
    <View class={`flex-1 gap-4 bg-background p-6 pt-14 ${FS.sm}`}>
      <View class="flex-row flex-wrap gap-2">
        <Btn testID="dev-new-conn" variant="primary" label="New connection" onPress={() => openConnectionForm()} />
        <Btn
          testID="dev-edit-conn"
          label="Edit first"
          disabled={!first()}
          onPress={() => openConnectionForm(first())}
        />
        <Btn testID="dev-settings" label="Settings" onPress={() => openSettings()} />
        <Btn testID="dev-settings-general" label="Settings · general" onPress={() => openSettings("general")} />
        <Btn testID="dev-settings-about" label="Settings · about" onPress={() => openSettings("about")} />
        <Btn testID="dev-import" label="Import" disabled={!first()} onPress={() => openImport(first()!.id)} />
        <Btn
          testID="dev-theme"
          label="Toggle theme"
          onPress={() => updatePrefs({ theme: currentPrefs().theme === "dark" ? "light" : "dark" })}
        />
      </View>
      <View class="gap-1">
        <Text testID="dev-active" class="text-muted-foreground">
          active: {conns.activeId() ?? "-"} · theme: {currentPrefs().theme} · maxRows: {currentPrefs().maxRows} · update
          dot: {hasUpdate() ? "on" : "off"}
        </Text>
        <Each of={conns.list()} key={(c) => c.id}>
          {(c) => (
            <View class="flex-row items-center gap-2">
              <DbIcon kind={c().kind} />
              <Text class="text-foreground">{c().name}</Text>
              <EnvBadge env={c().env} />
              <Text class="text-fg-faint">{c().kind === "sqlite" ? c().filePath : `${c().host}:${c().port}`}</Text>
            </View>
          )}
        </Each>
      </View>
      <DialogHost />
    </View>
  );
}

bootDev();
mount(Dev);
