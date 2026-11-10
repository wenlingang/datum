// 输入框下方的模型切换:按服务商分组列出设置里勾选启用的模型,选中即写回全局配置。

import { Text, View } from "nsh";
import { Menu, MenuItem, MenuSeparator } from "nsh/base";
import { t } from "nsh/i18n";
import type { ProviderConfig } from "../lib/types";
import { aiStore, setCurrentModel } from "../store/ai";
import { Icon, type IconName, tip } from "../ui/kit";
import { FS } from "../ui/text";
import { chatDeps } from "./deps";

const ITEM = "h-[26px] flex-row items-center gap-2 rounded-(--radius) px-2 hover:bg-accent focus:bg-accent active:bg-secondary-active";

function Item(props: { label: string; checked?: boolean; icon?: IconName; onSelect: () => void }) {
  return (
    <MenuItem
      onSelect={props.onSelect}
      checked={props.checked}
      role={props.checked === undefined ? undefined : "menuitemradio"}
      class={ITEM}
    >
      <View class="w-3.5 items-center">
        <If when={props.checked}>
          <Icon name="check" size={13} color="foreground" />
        </If>
        <If when={props.icon}>{(n) => <Icon name={n()} size={13} />}</If>
      </View>
      <Text class={`${FS.sm} flex-1 text-popover-foreground line-clamp-1`}>{props.label}</Text>
    </MenuItem>
  );
}

const enabledModels = (p: ProviderConfig) => p.models.filter((m) => m.enabled).map((m) => m.id);

export function ModelPicker() {
  const current = () => aiStore.settings()?.current ?? null;
  const groups = () => (aiStore.settings()?.providers ?? []).filter((p) => p.enabled && enabledModels(p).length > 0);
  const label = () => current()?.model || t("chat.select_model");
  const tooltip = () => {
    const p = aiStore.settings()?.providers.find((x) => x.id === current()?.providerId);
    return p ? `${p.name} · ${t("chat.switch_model")}` : t("chat.switch_model");
  };
  return (
    <Menu
      offset={4}
      placement="top"
      popupClass="min-w-[240px] max-h-[400px] rounded-(--radius) border border-border bg-popover p-1 shadow-lg"
      trigger={
        <View
          role="button"
          testID="ai-model"
          accessibilityLabel={t("chat.switch_model")}
          tooltip={tip(tooltip)}
          class="h-6 flex-row items-center gap-1 rounded-(--radius) px-1.5 hover:bg-list-hover active:bg-secondary-active"
        >
          <Icon name="cpu" size={13} />
          <Text class={`${FS.xs} text-foreground line-clamp-1`}>{label()}</Text>
          <Icon name="chevron-down" size={12} color="fg-faint" />
        </View>
      }
    >
      <Each of={groups()} key={(p) => p.id}>
        {(p, i) => (
          <>
            <If when={i() > 0}>
              <MenuSeparator class="mx-1 my-1 h-px bg-border" />
            </If>
            <Text class={`${FS.xs} px-2 pb-0.5 pt-1.5 font-semibold text-fg-faint`}>{p().name}</Text>
            <Each of={enabledModels(p())}>
              {(m) => (
                <Item
                  label={m()}
                  checked={current()?.providerId === p().id && current()?.model === m()}
                  onSelect={() => setCurrentModel(p().id, m())}
                />
              )}
            </Each>
          </>
        )}
      </Each>
      <If when={groups().length > 0}>
        <MenuSeparator class="mx-1 my-1 h-px bg-border" />
      </If>
      <Item label={t("chat.manage_providers")} icon="settings" onSelect={chatDeps.openSettings} />
    </Menu>
  );
}
