// 界面偏好:主题、语言、字体、字号、行数上限。落盘在 Rust 侧(ui_prefs.json),
// 这里持有唯一的内存镜像,修改一律走 updatePrefs。
//
// 字号:GPUI 版以「正文字号」为基准(text-sm = 字号),整套尺度随之缩放。这里用主题 token
// 表达:--fs-xs / --fs-sm / --fs-base / --fs-lg,组件用 ui/text.ts 里的 FS.* 类名引用它们。

import { setColorScheme, setTheme, signal } from "nsh";
import { setLocale } from "nsh/i18n";
import baseTheme from "../theme.json";
import { prefsStore } from "../lib/native";
import type { LanguagePreference, Prefs, ThemePreference } from "../lib/types";

const prefs = signal<Prefs>(prefsStore.load());

/** 当前偏好(读取处被追踪)。 */
export const currentPrefs = () => prefs();

/** 修改偏好并立即落盘、应用。 */
export function updatePrefs(patch: Partial<Prefs>) {
  prefs.update((p) => ({ ...p, ...patch }));
  prefsStore.save(prefs());
  apply();
}

/** 单次查询返回的最大行数。 */
export const maxRows = () => prefs().maxRows;

export const MAX_ROWS_OPTIONS = [1, 100, 500, 1000, 5000, 10_000, 100_000];
export const MAX_ROWS_CEILING = 1_000_000;
export const FONT_FAMILIES = [
  ".SystemUIFont",
  "PingFang SC",
  "Helvetica Neue",
  "Microsoft YaHei",
  "SF Mono",
  "Menlo",
  "JetBrains Mono",
];
export const FONT_SIZES = [12, 13, 14, 15, 16];
export const FONT_WEIGHTS = [400, 500, 600, 700];

/** 规范化自定义行数上限:正整数且不超过上限,否则 null。 */
export function normalizeMaxRows(v: string): number | null {
  const n = Number(v.trim());
  return Number.isInteger(n) && n >= 1 && n <= MAX_ROWS_CEILING ? n : null;
}

function localeOf(pref: LanguagePreference): string | null {
  switch (pref) {
    case "zhCn":
      return "zh-CN";
    case "zhTw":
      return "zh-TW";
    case "en":
      return "en";
    default:
      return null;
  }
}

/** 系统语言 → 支持的 locale:繁体 → zh-TW,其他中文 → zh-CN,其余英文。 */
export function mapLocale(tag: string): "en" | "zh-CN" | "zh-TW" {
  const t = tag.toLowerCase().replace(/_/g, "-");
  if (!t.startsWith("zh")) return "en";
  if (t.includes("hant") || t.includes("-tw") || t.includes("-hk") || t.includes("-mo")) return "zh-TW";
  return "zh-CN";
}

function schemeOf(pref: ThemePreference): "light" | "dark" | "system" {
  return pref;
}

function applyTheme(p: Prefs) {
  const size = p.fontSize;
  const family = p.fontFamily === ".SystemUIFont" ? null : p.fontFamily;
  const theme = structuredCloneTheme();
  const vars = theme.cssVars.theme as Record<string, string>;
  vars["fs-2xs"] = `${round((size * 0.625) / 0.875)}px`;
  vars["fs-xs"] = `${round((size * 0.75) / 0.875)}px`;
  vars["fs-sm"] = `${size}px`;
  vars["fs-base"] = `${round(size / 0.875)}px`;
  vars["fs-lg"] = `${round((size * 1.125) / 0.875)}px`;
  vars["fs-xl"] = `${round((size * 1.25) / 0.875)}px`;
  if (family) vars["font-sans"] = family;
  setTheme(theme);
}

function round(n: number) {
  return Math.round(n * 10) / 10;
}

function structuredCloneTheme(): typeof baseTheme {
  return JSON.parse(JSON.stringify(baseTheme));
}

/** 按当前偏好应用主题模式、字体与语言。启动时调用一次,之后每次修改偏好再调用。 */
export function apply() {
  const p = prefs();
  setColorScheme(schemeOf(p.theme));
  applyTheme(p);
  const tag = localeOf(p.language);
  setLocale(tag);
  // core 的报错文案跟随界面语言
  prefsStore.setCoreLocale(mapLocale(tag ?? systemLocaleTag()));
}

function systemLocaleTag(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().locale;
  } catch {
    return "en";
  }
}
