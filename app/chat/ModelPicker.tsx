// 输入框下方的模型切换:按服务商分组列出设置里勾选启用的模型,选中即写回全局配置。

import { Text, View } from "nsh";
import { t } from "nsh/i18n";
import type { ProviderConfig } from "../lib/types";
import { aiStore, setCurrentModel } from "../store/ai";
import { Icon, SectionLabel, tip } from "../ui/kit";
import { DropMenu, MCheck, MItem, MSep } from "../ui/menu";
import { FS } from "../ui/text";
import { chatDeps } from "./deps";

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
    <DropMenu
      placement="top"
      popupClass="min-w-[240px] max-h-[400px]"
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
              <MSep />
            </If>
            <SectionLabel class="px-2 pb-0.5 pt-1.5">{p().name}</SectionLabel>
            <Each of={enabledModels(p())}>
              {(m) => (
                <MCheck
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
        <MSep />
      </If>
      <MItem label={t("chat.manage_providers")} onSelect={chatDeps.openSettings} />
    </DropMenu>
  );
}
