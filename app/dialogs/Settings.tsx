// 设置:AI 服务商(多服务商 + 模型)/ 知识库 / 外观(语言、主题、字体、字号、粗细、行数上限)/ 关于与更新。
// 对应 GPUI 版 dialogs/settings.rs。

import { Scroll, signal, Text, View, type JSX, type Signal } from "nsh";
import { Button } from "nsh/base";
import { t } from "nsh/i18n";
import { ai, chat, dialogsNative, errorText } from "../lib/native";
import type { LanguagePreference, ProviderConfig, ThemePreference } from "../lib/types";
import {
  currentPrefs,
  FONT_FAMILIES,
  FONT_SIZES,
  FONT_WEIGHTS,
  MAX_ROWS_OPTIONS,
  normalizeMaxRows,
  updatePrefs,
} from "../store/prefs";
import { checkUpdates, installUpdate, updates } from "../store/update";
import { Btn, color, Hint, Icon, IconButton, NavRow, SectionLabel, TextField, type IconName } from "../ui/kit";
import { FS, MONO } from "../ui/text";
import {
  aiSettings,
  knowledgeDoc,
  mergeModels,
  mutateAi,
  mutateProvider,
  providerOf,
  providerSubtitle,
  reloadAiSettings,
} from "./aiSettings";
import { ActionMenu, CheckBox, DialogFrame, Dropdown, LoadingBtn, MaskedField, Toggle } from "./controls";
import type { SettingsSection } from "./state";

const SECTIONS: { id: SettingsSection; icon: IconName; key: string }[] = [
  { id: "ai", icon: "sparkles", key: "settings.section_ai" },
  { id: "knowledge", icon: "file-text", key: "settings.section_knowledge" },
  { id: "appearance", icon: "type", key: "settings.section_appearance" },
  { id: "about", icon: "refresh-cw", key: "settings.section_about" },
];

/** 表单标签列宽度 */
const LABEL_W = 76;

interface Notice {
  ok: boolean;
  text: string;
}

type Probe = { state: "running" } | { state: "ok" } | { state: "failed"; error: string };

export function SettingsDialog(props: { section: SettingsSection; open: boolean; onClose: () => void }) {
  return (
    <DialogFrame open={props.open} onClose={props.onClose} title={t("settings.title")} width={980} testID="settings">
      <SettingsView section={props.section} />
    </DialogFrame>
  );
}

function SettingsView(props: { section: SettingsSection }) {
  reloadAiSettings();
  // 初始分区只取打开时的值
  const section = signal<SettingsSection>(props.section);
  const notice = signal<Notice | null>(null);
  const current = aiSettings().current?.providerId;
  const selected = signal<string | null>(
    (current && providerOf(current) ? current : null) ?? aiSettings().providers[0]?.id ?? null,
  );

  const ctx: Ctx = { notice, selected };

  return (
    <View class="flex-row items-stretch" style={{ height: 620 }}>
      <View class="shrink-0 gap-1 border-r border-border pr-4" style={{ width: 168 }}>
        <Each of={SECTIONS} key={(s) => s.id}>
          {(s) => (
            <NavRow
              testID={`settings-section-${s().id}`}
              label={t(s().key)}
              icon={s().icon}
              selected={section() === s().id}
              onPress={() => {
                section.set(s().id);
                notice.set(null);
              }}
            />
          )}
        </Each>
      </View>
      <If
        when={section() === "ai"}
        else={
          <View class="min-w-0 flex-1 gap-3 pl-6">
            <SectionLabel>{t(SECTIONS.find((s) => s.id === section())?.key ?? "")}</SectionLabel>
            <Scroll class="flex-1" contentClass="pb-2">
              <If when={section() === "knowledge"}>
                <KnowledgeSection ctx={ctx} />
              </If>
              <If when={section() === "appearance"}>
                <AppearanceSection />
              </If>
              <If when={section() === "about"}>
                <AboutSection />
              </If>
            </Scroll>
          </View>
        }
      >
        <View class="min-w-0 flex-1 pl-6">
          <AiSection ctx={ctx} />
        </View>
      </If>
    </View>
  );
}

interface Ctx {
  notice: Signal<Notice | null>;
  selected: Signal<string | null>;
}

function NoticeLine(props: { ctx: Ctx }) {
  return (
    <If when={props.ctx.notice()}>
      {(n) => (
        <Text testID="settings-notice" class={`${FS.sm} ${n().ok ? "text-muted-foreground" : "text-negative"}`}>
          {n().text}
        </Text>
      )}
    </If>
  );
}

/** 标签 + 控件的一行。 */
function Row(props: { label: string; children: JSX.Element }) {
  return (
    <View class="flex-row items-center gap-3">
      <Text class={`${FS.sm} shrink-0 text-muted-foreground`} style={{ width: LABEL_W }}>
        {props.label}
      </Text>
      <View class="min-w-0 flex-1">{props.children}</View>
    </View>
  );
}

/** 带边框的分组卡片:标题 + 内容。 */
function Card(props: { title: string; right?: JSX.Element; children: JSX.Element; class?: string }) {
  return (
    <View class={`rounded-(--radius-lg) border border-border bg-panel p-4 ${props.class ?? "gap-3.5"}`}>
      <View class="flex-row items-center gap-2">
        <Text class={`${FS.sm} font-semibold text-foreground`}>{props.title}</Text>
        {props.right}
      </View>
      {props.children}
    </View>
  );
}

// ---------------------------------------------------------------- AI 服务商

/** 协议展示名(随 core 语言;读偏好语言以便切换语言后刷新)。 */
function protocolOptions() {
  void currentPrefs().language;
  try {
    return dialogsNative.aiProtocols();
  } catch {
    return [
      { id: "openai", label: "OpenAI" },
      { id: "anthropic", label: "Anthropic" },
      { id: "gemini", label: "Gemini" },
    ];
  }
}

function protocolLabel(id: string) {
  return protocolOptions().find((p) => p.id === id)?.label ?? id;
}

// 服务商头像的品牌色是数据色(各家品牌),不随主题变化;OpenAI 兼容用主色。
const BRAND: Record<string, string> = { anthropic: "#d97757", gemini: "#3b82f6" };

function Avatar(props: { provider: ProviderConfig; size: number }) {
  const tone = () => BRAND[props.provider.protocol] ?? color("primary");
  const initial = () => props.provider.name.trim().charAt(0).toUpperCase() || "?";
  return (
    <View
      class="shrink-0 items-center justify-center"
      style={{
        width: props.size,
        height: props.size,
        borderRadius: props.size * 0.3,
        backgroundColor: `${tone().slice(0, 7)}29`,
      }}
    >
      <Text class="font-bold" style={{ color: tone(), fontSize: props.size * 0.45 }}>
        {initial()}
      </Text>
    </View>
  );
}

const nameOf = (p: ProviderConfig) => (p.name.trim() === "" ? t("settings.unnamed") : p.name);

function AiSection(props: { ctx: Ctx }) {
  const { notice, selected } = props.ctx;

  function select(id: string | null) {
    selected.set(id);
    notice.set(null);
  }

  function addProvider(preset: { name: string; protocol: string; baseUrl: string } | null) {
    const id = crypto.randomUUID();
    const provider: ProviderConfig = {
      id,
      name: preset?.name ?? t("settings.custom_provider_name"),
      protocol: preset?.protocol ?? "openai",
      baseUrl: preset?.baseUrl ?? "",
      apiKey: "",
      enabled: true,
      models: [],
    };
    const err = mutateAi((s) => ({ ...s, providers: [...s.providers, provider] }));
    select(id);
    if (err) notice.set({ ok: false, text: err });
  }

  const presets = () => {
    void currentPrefs().language;
    try {
      return chat.presets().map((p) => ({ name: p.displayName, protocol: p.protocol, baseUrl: p.baseUrl }));
    } catch {
      return [];
    }
  };

  return (
    <View class="flex-1 flex-row items-start">
      <View class="h-full shrink-0 border-r border-border pr-4" style={{ width: 220 }}>
        <View class="pb-2.5">
          <SectionLabel>{t("settings.providers")}</SectionLabel>
        </View>
        <Scroll class="min-h-0 flex-1" contentClass="gap-1">
          <Each of={aiSettings().providers} key={(p) => p.id}>
            {(p) => {
              const isSelected = () => selected() === p().id;
              const inUse = () => aiSettings().current?.providerId === p().id;
              return (
                <Button
                  testID={`provider-${p().id}`}
                  accessibilityLabel={nameOf(p())}
                  selected={isSelected()}
                  onPress={() => select(p().id)}
                  class="h-12 flex-row items-center gap-2.5 rounded-(--radius-lg) px-2.5 hover:bg-list-hover selected:bg-raised"
                  classList={{ "opacity-50": !p().enabled }}
                >
                  <Avatar provider={p()} size={28} />
                  <View class="min-w-0 flex-1 gap-0.5">
                    <Text
                      class={`${FS.sm} text-foreground line-clamp-1`}
                      classList={{ "font-semibold": isSelected() }}
                    >
                      {nameOf(p())}
                    </Text>
                    <Text class={`${FS.xs} text-fg-faint line-clamp-1`}>
                      {providerSubtitle(p(), protocolLabel(p().protocol))}
                    </Text>
                  </View>
                  <If when={inUse()}>
                    <View class="shrink-0 rounded-full bg-ai" style={{ width: 7, height: 7 }} />
                  </If>
                </Button>
              );
            }}
          </Each>
        </Scroll>
        <View class="pt-3">
          <ActionMenu
            testID="provider-add"
            actions={[
              ...presets().map((p) => ({ label: p.name, onSelect: () => addProvider(p) })),
              { label: t("settings.custom_openai"), onSelect: () => addProvider(null), separatorBefore: true },
            ]}
            trigger={
              <View class="h-7 flex-row items-center justify-center gap-1.5 rounded-(--radius) border border-input hover:bg-list-hover">
                <Icon name="plus" size={13} color="foreground" />
                <Text class={`${FS.xs} font-medium text-foreground`}>{t("settings.add_provider")}</Text>
              </View>
            }
          />
        </View>
      </View>
      <Scroll class="h-full min-w-0 flex-1" contentClass="pl-6 pr-1 pb-4">
        <If
          when={providerOf(selected())}
          else={
            <View class="items-center justify-center gap-2 pt-48">
              <Icon name="sparkles" size={28} color="ai" />
              <Text class={`${FS.base} text-foreground`}>{t("settings.no_providers")}</Text>
              <Text class={`${FS.sm} text-center text-fg-faint`}>{t("settings.no_providers_hint")}</Text>
            </View>
          }
        >
          {/* 按服务商 id 重建:切换服务商时清掉搜索词与手输模型名 */}
          <Each of={selected() ? [selected() as string] : []} key={(id) => id}>
            {(id) => <ProviderDetail id={id()} ctx={props.ctx} onDeleted={select} />}
          </Each>
        </If>
      </Scroll>
    </View>
  );
}

function endpointHint(protocol: string) {
  if (protocol === "anthropic") return t("settings.endpoint_anthropic");
  if (protocol === "gemini") return t("settings.endpoint_gemini");
  return t("settings.endpoint_openai");
}

function ProviderDetail(props: { id: string; ctx: Ctx; onDeleted: (next: string | null) => void }) {
  const { notice } = props.ctx;
  // 删除后到本组件卸载之间仍可能读到:用最后一次的值兜底
  let last = providerOf(props.id) as ProviderConfig;
  const p = () => {
    const found = providerOf(props.id);
    if (found) last = found;
    return last;
  };
  const newModel = signal("");
  const filter = signal("");
  const fetching = signal(false);
  const probes = signal<Record<string, Probe>>({});
  const probeKey = (model: string) => `${p().id}\u0000${model}`;

  const change = (f: (x: ProviderConfig) => ProviderConfig) => {
    const err = mutateProvider(p().id, f);
    if (err) notice.set({ ok: false, text: err });
  };

  function deleteProvider() {
    const id = p().id;
    const err = mutateAi((s) => ({ ...s, providers: s.providers.filter((x) => x.id !== id) }));
    props.onDeleted(aiSettings().providers[0]?.id ?? null);
    if (err) notice.set({ ok: false, text: err });
  }

  function addManualModel() {
    const model = newModel().trim();
    if (!model) return;
    change((x) => (x.models.some((m) => m.id === model) ? x : { ...x, models: [...x.models, { id: model, enabled: true }] }));
    newModel.set("");
  }

  async function fetchModels() {
    const provider = p();
    fetching.set(true);
    notice.set(null);
    try {
      const ids = await ai.listModels(provider);
      let added = 0;
      const err = mutateProvider(provider.id, (x) => {
        const merged = mergeModels(x, ids);
        added = merged.added;
        return merged.provider;
      });
      notice.set(
        err
          ? { ok: false, text: err }
          : { ok: true, text: added > 0 ? t("settings.models_added", { count: added }) : t("settings.models_up_to_date") },
      );
    } catch (e) {
      notice.set({ ok: false, text: errorText(e) });
    } finally {
      fetching.set(false);
    }
  }

  async function testModel(model: string) {
    const key = probeKey(model);
    probes.update((m) => ({ ...m, [key]: { state: "running" } }));
    let next: Probe;
    try {
      await ai.testConfig(p(), model);
      next = { state: "ok" };
    } catch (e) {
      next = { state: "failed", error: errorText(e) };
    }
    probes.update((m) => ({ ...m, [key]: next }));
  }

  const enabledCount = () => p().models.filter((m) => m.enabled).length;
  const shown = () => {
    const f = filter().trim().toLowerCase();
    return f ? p().models.filter((m) => m.id.toLowerCase().includes(f)) : p().models;
  };

  return (
    <View class="gap-4">
      <View class="flex-row items-center gap-3 pb-1">
        <Avatar provider={p()} size={40} />
        <View class="min-w-0 flex-1 gap-1">
          <Text class={`${FS.lg} font-semibold text-foreground`}>{nameOf(p())}</Text>
          <Text class={`${FS.xs} text-fg-faint line-clamp-1`}>
            {p().baseUrl.trim() === "" ? protocolLabel(p().protocol) : `${protocolLabel(p().protocol)} · ${p().baseUrl.trim()}`}
          </Text>
        </View>
        <Toggle
          testID="provider-enabled"
          checked={p().enabled}
          label={t("settings.provider_enabled")}
          onChange={(v) => change((x) => ({ ...x, enabled: v }))}
        />
        <IconButton icon="trash" tooltip={t("settings.delete_provider")} onPress={deleteProvider} />
      </View>

      <NoticeLine ctx={props.ctx} />

      <Card title={t("settings.connection_card")}>
        <Row label={t("settings.name")}>
          <TextField
            testID="provider-name"
            value={p().name}
            placeholder={t("settings.provider_name_placeholder")}
            onChange={({ value }) => change((x) => ({ ...x, name: value }))}
          />
        </Row>
        <Row label={t("settings.protocol")}>
          <Dropdown
            testID="provider-protocol"
            style={{ width: 220 }}
            items={protocolOptions().map((o) => ({ value: o.id, title: o.label }))}
            value={p().protocol}
            label={protocolLabel(p().protocol)}
            accessibilityLabel={t("settings.protocol")}
            onChange={(v) => change((x) => ({ ...x, protocol: v }))}
          />
        </Row>
        <View class="gap-1.5">
          <Row label={t("settings.api_url")}>
            <TextField
              testID="provider-base-url"
              value={p().baseUrl}
              placeholder="https://api.openai.com/v1"
              onChange={({ value }) => change((x) => ({ ...x, baseUrl: value }))}
            />
          </Row>
          <Text class={`${FS.xs} text-fg-faint`} style={{ paddingLeft: LABEL_W + 12 }}>
            {endpointHint(p().protocol)}
          </Text>
        </View>
        <Row label="API Key">
          <MaskedField
            testID="provider-api-key"
            value={p().apiKey}
            placeholder="sk-…"
            onChange={({ value }) => change((x) => ({ ...x, apiKey: value }))}
          />
        </Row>
      </Card>

      <Card
        title={t("settings.models")}
        class="gap-3"
        right={
          <>
            <View class="rounded-[4px] bg-raised px-1.5">
              <Text class={`${FS.xs} text-muted-foreground`}>
                {t("settings.models_enabled_count", { enabled: enabledCount(), total: p().models.length })}
              </Text>
            </View>
            <View class="flex-1" />
            <LoadingBtn
              testID="models-fetch"
              small
              icon="refresh-cw"
              label={fetching() ? t("settings.fetching") : t("settings.fetch_models")}
              loading={fetching()}
              tooltip={t("settings.fetch_models_tip")}
              onPress={fetchModels}
            />
          </>
        }
      >
        <View class="flex-row gap-2">
          <View class="flex-1 justify-center">
            <TextField
              testID="model-filter"
              value={filter()}
              placeholder={t("settings.model_search_placeholder")}
              onChange={({ value }) => filter.set(value)}
              class="pl-8"
            />
            <View class="pointer-events-none absolute left-2.5">
              <Icon name="search" size={14} color="fg-faint" />
            </View>
          </View>
          <View class="flex-1 justify-center">
            <TextField
              testID="model-add"
              value={newModel()}
              placeholder={t("settings.model_add_placeholder")}
              submitBehavior="submit"
              onChange={({ value }) => newModel.set(value)}
              onSubmit={addManualModel}
              class="pl-8"
            />
            <View class="pointer-events-none absolute left-2.5">
              <Icon name="plus" size={14} color="fg-faint" />
            </View>
          </View>
        </View>
        <If
          when={p().models.length > 0}
          else={
            <View class="items-center gap-1 py-6">
              <Text class={`${FS.sm} text-fg-faint`}>{t("settings.no_models")}</Text>
              <Text class={`${FS.sm} text-fg-faint`}>{t("settings.no_models_hint")}</Text>
            </View>
          }
        >
          <If when={enabledCount() === 0}>
            <Hint>{t("settings.models_check_hint")}</Hint>
          </If>
          <View class="overflow-hidden rounded-(--radius) border border-border bg-background">
            <Each of={shown()} key={(m) => m.id}>
              {(m, index) => {
                const probe = () => probes()[probeKey(m().id)];
                return (
                  <View
                    class="group h-10 flex-row items-center gap-3 px-3 hover:bg-list-hover"
                    classList={{ "border-t border-border": index() > 0 }}
                  >
                    <CheckBox
                      testID={`model-on-${m().id}`}
                      label={m().id}
                      checked={m().enabled}
                      onChange={(v) =>
                        change((x) => ({ ...x, models: x.models.map((e) => (e.id === m().id ? { ...e, enabled: v } : e)) }))
                      }
                    />
                    <Text
                      class={`${FS.sm} ${MONO} min-w-0 flex-1 line-clamp-1`}
                      classList={{ "text-foreground": m().enabled, "text-muted-foreground": !m().enabled }}
                    >
                      {m().id}
                    </Text>
                    <If when={probe()?.state === "ok"}>
                      <View class="flex-row items-center gap-1">
                        <Icon name="circle-check" size={13} color="env-local" />
                        <Text class={`${FS.xs} text-env-local`}>{t("settings.model_available")}</Text>
                      </View>
                    </If>
                    <If when={probe()?.state === "failed" ? (probe() as { error: string }).error : null}>
                      {(err) => (
                        <View class="flex-row items-center gap-1" style={{ maxWidth: 220 }}>
                          <Icon name="circle-x" size={13} color="negative" class="shrink-0" />
                          <Text class={`${FS.xs} text-negative line-clamp-1`}>{err()}</Text>
                        </View>
                      )}
                    </If>
                    {/* 操作按钮悬停才出现,测试中保持可见 */}
                    <View
                      class="flex-row items-center gap-1"
                      classList={{
                        "opacity-0 group-hover:opacity-100": probe()?.state !== "running",
                      }}
                    >
                      <LoadingBtn
                        testID={`model-test-${m().id}`}
                        variant="ghost"
                        xsmall
                        label={t("settings.model_test")}
                        loading={probe()?.state === "running"}
                        onPress={() => void testModel(m().id)}
                      />
                      <IconButton
                        icon="trash"
                        size={13}
                        tooltip={t("settings.model_remove")}
                        onPress={() => change((x) => ({ ...x, models: x.models.filter((e) => e.id !== m().id) }))}
                      />
                    </View>
                  </View>
                );
              }}
            </Each>
            <If when={shown().length === 0}>
              <Text class={`${FS.sm} py-4 text-center text-fg-faint`}>{t("settings.no_matching_models")}</Text>
            </If>
          </View>
        </If>
      </Card>
    </View>
  );
}

// ---------------------------------------------------------------- 知识库

function KnowledgeSection(props: { ctx: Ctx }) {
  const { notice } = props.ctx;
  const doc = signal(knowledgeDoc());
  const custom = () => aiSettings().knowledgePath !== "";

  /** 选择/恢复默认立即落盘:选文件本身就是一次完整、明确的操作。 */
  function save(name: string, path: string) {
    const err = mutateAi((s) => ({ ...s, knowledgeName: name, knowledgePath: path }));
    doc.set(knowledgeDoc());
    notice.set(
      err
        ? { ok: false, text: err }
        : { ok: true, text: name === "" ? t("settings.knowledge_reset_done") : t("settings.knowledge_enabled", { name }) },
    );
  }

  async function pick() {
    try {
      const path = await dialogsNative.pickFile(t("settings.knowledge_pick_prompt"), null, []);
      if (!path) return;
      const name = path.split(/[\\/]/).pop() ?? path;
      save(name, path);
    } catch (e) {
      notice.set({ ok: false, text: errorText(e) });
    }
  }

  return (
    <View class="gap-3">
      <Hint>{t("settings.knowledge_hint")}</Hint>
      <Row label={t("settings.document")}>
        <View class="flex-row items-center gap-1.5">
          <Icon name="file-text" size={14} />
          <Text testID="knowledge-name" class={`${FS.sm} min-w-0 flex-1 text-foreground line-clamp-1`}>
            {doc().name}
          </Text>
          <If
            when={custom()}
            else={<Text class={`${FS.xs} text-fg-faint`}>{t("settings.builtin_default")}</Text>}
          >
            <IconButton icon="x" size={13} tooltip={t("settings.restore_default")} onPress={() => save("", "")} />
          </If>
          <Btn
            testID="knowledge-pick"
            variant="outline"
            small
            label={custom() ? t("settings.change_file") : t("settings.choose_file")}
            onPress={() => void pick()}
          />
        </View>
      </Row>
      <Text
        class={`${FS.xs} ${doc().warning ? "text-negative" : "text-muted-foreground"}`}
        style={{ paddingLeft: LABEL_W + 12 }}
      >
        {doc().warning ?? t("settings.knowledge_stats", { count: [...doc().content].length })}
      </Text>
      <NoticeLine ctx={props.ctx} />
    </View>
  );
}

// ---------------------------------------------------------------- 外观

const LANGUAGES: LanguagePreference[] = ["system", "zhCn", "zhTw", "en"];
const THEMES: { id: ThemePreference; icon: IconName; key: string }[] = [
  { id: "light", icon: "sun", key: "prefs.theme_light" },
  { id: "dark", icon: "moon", key: "prefs.theme_dark" },
  { id: "system", icon: "monitor", key: "prefs.theme_system" },
];

function languageLabel(l: LanguagePreference) {
  switch (l) {
    case "system":
      return t("prefs.language_system");
    case "zhCn":
      return "简体中文";
    case "zhTw":
      return "繁體中文";
    case "en":
      return "English";
  }
}

/** 字体族的展示名;不在候选里的返回 null。 */
export function fontFamilyLabel(family: string): string | null {
  switch (family) {
    case ".SystemUIFont":
      return t("prefs.font_system");
    case "PingFang SC":
      return t("prefs.font_pingfang");
    case "Helvetica Neue":
      return "Helvetica Neue";
    case "Microsoft YaHei":
      return t("prefs.font_yahei");
    case "SF Mono":
    case "Menlo":
    case "JetBrains Mono":
      return t("prefs.font_mono", { name: family });
    default:
      return null;
  }
}

const SIZE_KEYS: Record<number, string> = {
  12: "prefs.size_small",
  13: "prefs.size_standard",
  14: "prefs.size_medium",
  15: "prefs.size_large",
  16: "prefs.size_xlarge",
};
const WEIGHT_KEYS: Record<number, string> = {
  400: "prefs.weight_regular",
  500: "prefs.weight_medium",
  600: "prefs.weight_semibold",
  700: "prefs.weight_bold",
};

const WEIGHT_CLASS: Record<number, string> = {
  400: "font-normal",
  500: "font-medium",
  600: "font-semibold",
  700: "font-bold",
};

const sizeLabel = (px: number) =>
  SIZE_KEYS[px] ? t("settings.font_size_option", { label: t(SIZE_KEYS[px]), px }) : `${px}px`;
const weightLabel = (w: number) => t(WEIGHT_KEYS[w] ?? "prefs.weight_regular");
const rowsLabel = (n: number) => t("toolbar.n_rows", { n: n.toLocaleString() });

function AppearanceSection() {
  const prefs = () => currentPrefs();
  const customRows = signal("");
  const dropdown = { small: true, style: { width: 260 } };
  return (
    <View class="gap-3.5">
      <Row label={t("prefs.language")}>
        <Dropdown
          {...dropdown}
          testID="pref-language"
          accessibilityLabel={t("prefs.language")}
          items={LANGUAGES.map((l) => ({ value: l, title: languageLabel(l) }))}
          value={prefs().language}
          label={languageLabel(prefs().language)}
          onChange={(v) => updatePrefs({ language: v as LanguagePreference })}
        />
      </Row>
      <Row label={t("settings.theme")}>
        <Dropdown
          {...dropdown}
          testID="pref-theme"
          accessibilityLabel={t("settings.theme")}
          items={THEMES.map((x) => ({ value: x.id, title: t(x.key), icon: x.icon }))}
          value={prefs().theme}
          label={t(THEMES.find((x) => x.id === prefs().theme)?.key ?? "prefs.theme_dark")}
          onChange={(v) => updatePrefs({ theme: v as ThemePreference })}
        />
      </Row>
      <Row label={t("settings.font")}>
        <Dropdown
          {...dropdown}
          testID="pref-font"
          accessibilityLabel={t("settings.font")}
          items={FONT_FAMILIES.map((f) => ({ value: f, title: fontFamilyLabel(f) ?? f }))}
          value={prefs().fontFamily}
          label={fontFamilyLabel(prefs().fontFamily) ?? t("settings.font_custom")}
          onChange={(v) => updatePrefs({ fontFamily: v })}
        />
      </Row>
      <Row label={t("settings.font_size")}>
        <Dropdown
          {...dropdown}
          testID="pref-font-size"
          accessibilityLabel={t("settings.font_size")}
          items={FONT_SIZES.map((s) => ({ value: String(s), title: sizeLabel(s) }))}
          value={String(prefs().fontSize)}
          label={sizeLabel(prefs().fontSize)}
          onChange={(v) => updatePrefs({ fontSize: Number(v) })}
        />
      </Row>
      <Row label={t("settings.font_weight")}>
        <Dropdown
          {...dropdown}
          testID="pref-font-weight"
          accessibilityLabel={t("settings.font_weight")}
          items={FONT_WEIGHTS.map((w) => ({ value: String(w), title: weightLabel(w) }))}
          value={String(prefs().fontWeight)}
          label={weightLabel(prefs().fontWeight)}
          onChange={(v) => updatePrefs({ fontWeight: Number(v) })}
        />
      </Row>
      <Row label={t("toolbar.limit_title")}>
        <View class="flex-row items-center gap-2">
          <Dropdown
            {...dropdown}
            testID="pref-max-rows"
          accessibilityLabel={t("toolbar.limit_title")}
            items={MAX_ROWS_OPTIONS.map((n) => ({ value: String(n), title: rowsLabel(n) }))}
            value={String(prefs().maxRows)}
            label={rowsLabel(prefs().maxRows)}
            tooltip={t("toolbar.limit_tooltip")}
            onChange={(v) => updatePrefs({ maxRows: Number(v) })}
          />
          <TextField
            testID="pref-max-rows-custom"
            class="h-7"
            style={{ width: 120 }}
            keyboardType="number-pad"
            value={customRows()}
            placeholder={t("toolbar.limit_custom")}
            submitBehavior="submit"
            onChange={({ value }) => customRows.set(value)}
            onSubmit={({ text }) => {
              const n = normalizeMaxRows(text);
              if (n != null) {
                updatePrefs({ maxRows: n });
                customRows.set("");
              }
            }}
          />
        </View>
      </Row>
      <View class="gap-2 rounded-(--radius-lg) bg-raised px-3 py-2.5">
        <SectionLabel>{t("settings.preview")}</SectionLabel>
        <Text class={`${FS.sm} text-foreground ${WEIGHT_CLASS[prefs().fontWeight] ?? "font-normal"}`}>
          {t("settings.preview_text")}
        </Text>
      </View>
    </View>
  );
}

// ---------------------------------------------------------------- 关于与更新

function AboutSection() {
  const s = () => updates.status();
  const check = (label: string) => (
    <Btn testID="check-update" variant="outline" small label={label} onPress={() => void checkUpdates()} />
  );
  return (
    <View class="gap-3">
      <View class="flex-row items-center gap-1">
        <Text class={`${FS.sm} text-muted-foreground`}>{t("settings.current_version")}</Text>
        <Text testID="app-version" class={`${FS.sm} ${MONO} text-foreground`}>
          {updates.version()}
        </Text>
      </View>
      <View class="flex-row items-center gap-3" style={{ minHeight: 30 }}>
        <If when={updates.supported()} else={<Hint>{t("settings.dev_build")}</Hint>}>
          <If when={s().kind === "idle"}>{check(t("settings.check_updates"))}</If>
          <If when={s().kind === "checking"}>
            <Hint>{t("settings.checking")}</Hint>
          </If>
          <If when={s().kind === "upToDate"}>
            <Hint>{t("settings.up_to_date")}</Hint>
            {check(t("settings.check_updates"))}
          </If>
          <If when={(() => { const x = s(); return x.kind === "available" ? x.update : null; })()}>
            {(u) => (
              <>
                <Text class={`${FS.sm} font-semibold text-foreground`}>{t("settings.new_version", { version: u().version })}</Text>
                <Btn variant="primary" small label={t("settings.update_now")} onPress={() => void installUpdate()} />
              </>
            )}
          </If>
          <If when={(() => { const x = s(); return x.kind === "downloading" ? { pct: x.pct } : null; })()}>
            {(d) => (
              <Hint>
                {d().pct == null ? t("settings.downloading") : t("settings.downloading_pct", { pct: d().pct })}
              </Hint>
            )}
          </If>
          <If when={s().kind === "installing"}>
            <Hint>{t("settings.installing")}</Hint>
          </If>
          <If when={(() => { const x = s(); return x.kind === "failed" ? x.error : null; })()}>
            {(e) => (
              <>
                <Text class={`${FS.sm} text-negative`}>{t("settings.update_failed", { error: e() })}</Text>
                {check(t("settings.retry"))}
              </>
            )}
          </If>
        </If>
      </View>
      <If when={(() => { const x = s(); return updates.supported() && x.kind === "available" ? x.update.notes : null; })()}>
        {(notes) => <Text class={`${FS.sm} text-muted-foreground`}>{notes()}</Text>}
      </If>
    </View>
  );
}
