// 设置页 AI 部分的状态:整份 AI 配置的镜像(落盘在 Rust 侧 ai_settings.json),
// 所见即所存:每次修改立即保存,没有单独的「保存」按钮。

import { signal } from "nsh";
import { t } from "nsh/i18n";
import { ai, errorText } from "../lib/native";
import { saveAiSettings } from "../store/ai";
import type { AiSettings, KnowledgeDoc, ProviderConfig } from "../lib/types";

const settings = signal<AiSettings>(load());
const revision = signal(0);

function load(): AiSettings {
  try {
    return ai.settings();
  } catch {
    return { providers: [], current: null, knowledgeName: "", knowledgePath: "" };
  }
}

/** 当前 AI 配置(读取处被追踪)。 */
export const aiSettings = () => settings();

/** 每保存一次加一:AI 面板等可据此重读 `ai.status()`。 */
export const aiSettingsRevision = () => revision();

/** 从磁盘重读(打开设置时)。 */
export function reloadAiSettings() {
  settings.set(load());
}

/** 改一份副本并立即落盘;返回错误文案(成功为 null)。 */
export function mutateAi(f: (s: AiSettings) => AiSettings): string | null {
  const next = f(structuredCopy(settings()));
  try {
    // 经 store/ai 保存:AI 面板的配置与状态随之刷新
    settings.set(saveAiSettings(next));
    revision.update((n) => n + 1);
    return null;
  } catch (e) {
    return errorText(e);
  }
}

/** 改某个服务商。 */
export function mutateProvider(id: string, f: (p: ProviderConfig) => ProviderConfig): string | null {
  return mutateAi((s) => ({ ...s, providers: s.providers.map((p) => (p.id === id ? f(p) : p)) }));
}

export function providerOf(id: string | null): ProviderConfig | undefined {
  return id ? settings().providers.find((p) => p.id === id) : undefined;
}

/** 服务商在列表里的副标题:协议 · 启用的模型数。 */
export function providerSubtitle(p: ProviderConfig, protocolLabel: string): string {
  const n = p.models.filter((m) => m.enabled).length;
  if (n === 0) return protocolLabel;
  if (n === 1) return t("settings.provider_models_one", { protocol: protocolLabel });
  return t("settings.provider_models_n", { protocol: protocolLabel, count: n });
}

/** 把拉取到的模型并进列表:已有的保留勾选状态,新的默认不勾选。返回新增数。 */
export function mergeModels(p: ProviderConfig, ids: string[]): { provider: ProviderConfig; added: number } {
  const have = new Set(p.models.map((m) => m.id));
  const fresh = ids.filter((id, i) => !have.has(id) && ids.indexOf(id) === i);
  return { provider: { ...p, models: [...p.models, ...fresh.map((id) => ({ id, enabled: false }))] }, added: fresh.length };
}

/** 当前知识库文档(内置默认或用户选的文件)。 */
export function knowledgeDoc(): KnowledgeDoc {
  try {
    return ai.knowledge();
  } catch (e) {
    return { name: "", content: "", warning: errorText(e) };
  }
}

function structuredCopy<T>(v: T): T {
  return JSON.parse(JSON.stringify(v)) as T;
}
