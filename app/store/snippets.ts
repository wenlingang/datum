// 收藏的 SQL 片段:全局共享(不分连接),按语言分开——SQL 片段只出现在 SQL 编辑器里,Redis 命令只在 Redis 里。
// 落盘在配置目录的 snippets.json;写盘失败提示一次,内存里的改动仍然生效。

import { signal } from "nsh";
import { t } from "nsh/i18n";
import { errorText, store } from "../lib/native";
import { toast } from "./toast";

export type SnippetLang = "sql" | "redis";

export interface Snippet {
  id: string;
  name: string;
  sql: string;
  lang: SnippetLang;
  /** Redis 分类下的 Lua 脚本(在 Lua 脚本标签页里用;查询标签页里选中时开 Lua 标签页) */
  lua?: boolean;
  /** Unix 毫秒 */
  createdAt: number;
}

/** 片段数量上限:收藏是给常用语句的,不是第二份历史。 */
export const MAX_SNIPPETS = 500;

function load(): Snippet[] {
  try {
    const raw = store.loadSnippets() as { snippets?: unknown } | null;
    const list = Array.isArray(raw?.snippets) ? raw.snippets : [];
    return list.filter(
      (s): s is Snippet =>
        !!s && typeof s.id === "string" && typeof s.name === "string" && typeof s.sql === "string",
    ).map((s) => {
      const lang: SnippetLang = s.lang === "redis" ? "redis" : "sql";
      return { ...s, lang, lua: lang === "redis" && s.lua === true ? true : undefined, createdAt: Number(s.createdAt) || 0 };
    });
  } catch {
    return [];
  }
}

const list = signal<Snippet[]>(load());

function persist(next: Snippet[]) {
  list.set(next);
  try {
    store.saveSnippets({ version: 1, snippets: next });
  } catch (e) {
    toast.error(t("snippets.save_failed", { error: errorText(e) }));
  }
}

/** 某种语言的片段,按名称排序。 */
export const snippets = (lang: SnippetLang) =>
  list()
    .filter((s) => s.lang === lang)
    .sort((a, b) => a.name.localeCompare(b.name));

/** 按名称或内容筛选(不区分大小写,空格分隔的每个词都要命中)。`luaOnly`:只要 Lua 脚本。 */
export function searchSnippets(lang: SnippetLang, query: string, luaOnly = false): Snippet[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  return snippets(lang).filter((s) => {
    if (luaOnly && !s.lua) return false;
    const hay = `${s.name}\n${s.sql}`.toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}

/** 由 SQL 推一个默认名:第一行非注释内容压成一行,最长 40 字。 */
export function defaultName(sql: string): string {
  const line =
    sql
      .split("\n")
      .map((l) => l.trim())
      .find((l) => l && !l.startsWith("--")) ?? "";
  const flat = line.replace(/\s+/g, " ");
  return flat.length > 40 ? `${flat.slice(0, 39)}…` : flat;
}

/** 收藏一条;同语言下同名的覆盖内容。返回是否成功(空内容、超上限时为 false)。
 *  `lua`:Redis 分类下标记为 Lua 脚本。 */
export function saveSnippet(lang: SnippetLang, name: string, sql: string, lua = false): boolean {
  const body = sql.trim();
  const title = name.trim() || defaultName(body);
  if (!body || !title) return false;
  const all = list();
  const same = all.find((s) => s.lang === lang && s.name === title);
  if (same) {
    persist(all.map((s) => (s === same ? { ...s, sql: body, lua: lua || undefined } : s)));
    return true;
  }
  if (all.length >= MAX_SNIPPETS) {
    toast.error(t("snippets.too_many", { max: MAX_SNIPPETS }));
    return false;
  }
  const id = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  persist([...all, { id, name: title, sql: body, lang, ...(lua ? { lua: true } : {}), createdAt: Date.now() }]);
  return true;
}

export function deleteSnippet(id: string) {
  persist(list().filter((s) => s.id !== id));
}
