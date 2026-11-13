// SQL 片段浮层:收藏当前编辑器里的语句(或选区),按名称 / 内容筛选,点一下插入到光标处。
// 片段不分连接(常用语句往往跨库通用),只按语言分开;插入后不自动执行,由用户自己运行。

import { List, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";
import { defaultName, deleteSnippet, saveSnippet, searchSnippets, type Snippet, type SnippetLang } from "../store/snippets";
import { toast } from "../store/toast";
import { Btn, Icon, IconButton, TextField } from "../ui/kit";
import { FS } from "../ui/text";
import { insertAtCursor, selectionText, type TabModel } from "./model";

const compress = (sql: string) => sql.split(/\s+/).filter(Boolean).join(" ");

export const snippetLang = (m: TabModel): SnippetLang => (m.dbKind === "redis" ? "redis" : "sql");

/** 要收藏的文本:有选区收藏选区,否则整个编辑器。 */
const sourceText = (m: TabModel) => (selectionText(m).trim() || m.sql()).trim();

export function SnippetPanel(props: { m: TabModel }) {
  const m = props.m;
  const lang = snippetLang(m);
  const query = signal("");
  /** 正在给当前语句起名(null 为未在收藏) */
  const naming = signal<string | null>(null);
  const found = () => searchSnippets(lang, query());

  const startSave = () => {
    const text = sourceText(m);
    if (!text) {
      toast.info(t("snippets.empty_editor"));
      return;
    }
    naming.set(defaultName(text));
  };
  const finishSave = () => {
    const name = naming();
    if (name === null) return;
    if (saveSnippet(lang, name, sourceText(m))) {
      naming.set(null);
      toast.success(t("snippets.saved"));
    }
  };
  const pick = (s: Snippet) => {
    m.snippetsOpen.set(false);
    insertAtCursor(m, s.sql);
  };

  return (
    <View class="w-[440px]">
      <View class="flex-row items-center gap-1.5 border-b border-border px-3 py-2">
        <Icon name="bookmark" size={13} />
        <Text class={`${FS.xs} flex-1 text-muted-foreground`}>{t("snippets.title")}</Text>
        <Btn
          small
          variant="ghost"
          icon="plus"
          testID="snippet-add"
          label={selectionText(m).trim() ? t("snippets.save_selection") : t("snippets.save_current")}
          onPress={startSave}
        />
      </View>
      <If when={naming() !== null}>
        <View class="flex-row items-center gap-1.5 border-b border-border px-3 py-2">
          <TextField
            class="h-7 flex-1"
            testID="snippet-name"
            autoFocus
            placeholder={t("snippets.name_placeholder")}
            value={naming() ?? ""}
            onChange={({ value }) => naming.set(value)}
            onSubmit={finishSave}
          />
          <Btn small variant="primary" testID="snippet-save" label={t("snippets.save")} onPress={finishSave} />
          <Btn small variant="ghost" label={t("snippets.cancel")} onPress={() => naming.set(null)} />
        </View>
      </If>
      <If when={searchSnippets(lang, "").length > 5}>
        <View class="border-b border-border px-3 py-2">
          <TextField
            class="h-7"
            testID="snippet-search"
            placeholder={t("snippets.search")}
            value={query()}
            onChange={({ value }) => query.set(value)}
          />
        </View>
      </If>
      <If
        when={found().length > 0}
        else={
          <Text class={`${FS.xs} px-4 py-6 text-center text-fg-faint`}>
            {query() ? t("snippets.no_match") : t("snippets.empty")}
          </Text>
        }
      >
        <List
          each={found()}
          keyExtractor={(s) => s.id}
          estimatedItemSize={48}
          style={{ height: Math.min(360, found().length * 48) }}
        >
          {(s) => (
            <View
              testID={`snippet:${s().name}`}
              onPress={() => pick(s())}
              class="flex-row items-center gap-2 border-b border-border px-3 py-1.5 hover:bg-list-hover"
            >
              <View class="min-w-0 flex-1">
                <Text class={`${FS.xs} font-bold text-foreground line-clamp-1`}>{s().name}</Text>
                <Text class={`${FS.xs} font-mono text-muted-foreground line-clamp-1`}>{compress(s().sql)}</Text>
              </View>
              <IconButton
                icon="trash"
                size={13}
                label={t("snippets.delete")}
                tooltip={t("snippets.delete")}
                onPress={() => deleteSnippet(s().id)}
              />
            </View>
          )}
        </List>
      </If>
    </View>
  );
}
