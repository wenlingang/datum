// 右侧 AI 助手面板:按连接分会话、绑定连接条、模型切换、清空、输入框高度可调并记住、
// 引用选中 SQL、流式回复、未配置时引导去设置。对应 GPUI 版 chat/mod.rs 的界面部分。

import { computed, effect, type ListRef, List, signal, Text, untracked, View } from "nsh";
import { Button, Input, ResizablePanel, ResizablePanelGroup, TextView } from "nsh/base";
import { t } from "nsh/i18n";
import { chat as chatNative } from "../lib/native";
import type { DbKind } from "../lib/types";
import { type AiMessage, aiStore, setPendingRef } from "../store/ai";
import { currentPrefs, updatePrefs } from "../store/prefs";
import { Btn, Hint, Icon, tip } from "../ui/kit";
import { FS } from "../ui/text";
import { askFix, canSend, clearConversation, convoKey, quoteSelection, sendText, target } from "./actions";
import { cardKey } from "./cards";
import { chatDeps } from "./deps";
import { ModelPicker } from "./ModelPicker";
import { SqlCard, SqlConfirmDialog } from "./SqlCard";

/** 输入框高度(记住在偏好里)的上下限。 */
const MIN_INPUT = 44;
const MAX_INPUT = 400;
/** 输入区除输入框之外的高度(引用条、按钮行、内边距)的初值;实际值按布局测得。 */
const CHROME_GUESS = 81;

function Header(props: { label: string | null; hasMessages: boolean }) {
  return (
    <View class="h-10 shrink-0 flex-row items-center gap-2 border-b border-border px-3">
      <Icon name="sparkles" size={15} color="ai" />
      <Text class={`${FS.sm} shrink-0 font-bold text-foreground`}>{t("chat.title")}</Text>
      <If when={props.label}>
        {(label) => <Text class={`${FS.xs} min-w-0 shrink font-mono text-muted-foreground line-clamp-1`}>{label()}</Text>}
      </If>
      <View class="flex-1" />
      <If when={props.hasMessages}>
        <Btn
          small
          variant="ghost"
          icon="eraser"
          label={t("chat.clear")}
          tooltip={t("chat.clear_tooltip")}
          testID="ai-clear"
          onPress={clearConversation}
        />
      </If>
    </View>
  );
}

function EmptyIntro() {
  return (
    <View class="flex-1 items-center justify-center gap-1 px-4">
      <Text class={`${FS.sm} text-center text-fg-faint`}>{t("chat.empty_intro")}</Text>
      <Text class={`${FS.sm} text-center text-fg-faint`}>{t("chat.empty_example_create")}</Text>
      <Text class={`${FS.sm} text-center text-fg-faint`}>{t("chat.empty_example_seed")}</Text>
      <Text class={`${FS.sm} text-center text-fg-faint`}>{t("chat.empty_confirm")}</Text>
    </View>
  );
}

/** 一条消息。列表单元格会复用到别的消息上:所有读取都在绑定里进行。 */
function MessageRow(props: { msg: AiMessage; streaming: boolean; kind: DbKind; target: string }) {
  const segments = computed(() =>
    props.msg.role === "assistant" && !props.msg.hidden
      ? chatNative.segments(props.msg.content).map((s, i) => ({ ...s, key: `${props.msg.id}:${i}:${s.kind}`, index: i }))
      : [],
  );
  return (
    <View class="pb-3">
      <If when={props.msg.hidden}>
        <Text class={`${FS.xs} px-1 text-fg-faint`}>{props.msg.content}</Text>
      </If>
      <If when={!props.msg.hidden && props.msg.role === "user"}>
        <View class="flex-row justify-end">
          <View class="max-w-[88%] rounded-[12px] rounded-br-[4px] bg-primary px-3 py-2">
            <Text class={`${FS.sm} select-text text-primary-foreground`}>
              {props.msg.content}
            </Text>
          </View>
        </View>
      </If>
      <If when={!props.msg.hidden && props.msg.role === "assistant" && !props.msg.content && props.streaming}>
        <View class="flex-row items-center gap-2 px-1">
          <Icon name="loader-circle" size={12} color="ai" />
          <Text class={`${FS.xs} text-muted-foreground`}>{t("chat.thinking")}</Text>
        </View>
      </If>
      <If when={segments().length > 0}>
        <View class="gap-2">
          <Each of={segments()} key={(s) => s.key}>
            {(seg) => (
              <If
                when={seg().kind === "sql"}
                else={
                  <TextView
                    markdown={seg().text}
                    options={{ streaming: props.streaming }}
                    selectable
                    class={`${FS.sm} leading-5 text-foreground`}
                  />
                }
              >
                <SqlCard
                  cardKey={cardKey(props.msg.id, seg().index)}
                  sql={seg().text}
                  streaming={props.streaming}
                  kind={props.kind}
                  target={props.target}
                />
              </If>
            )}
          </Each>
        </View>
      </If>
    </View>
  );
}

function PendingRef(props: { sql: string }) {
  return (
    <View class="flex-row items-start gap-1.5 rounded-[8px] border border-border bg-raised px-2 py-1.5">
      <Text class={`${FS.xs} shrink-0 text-fg-faint`}>{t("chat.quote")}</Text>
      <View class="max-h-[60px] flex-1 overflow-hidden">
        <Text class={`${FS.xs} font-mono text-muted-foreground line-clamp-4`}>{props.sql}</Text>
      </View>
      <Button
        onPress={() => setPendingRef(null)}
        accessibilityLabel={t("chat.remove_quote")}
        tooltip={tip(t("chat.remove_quote"))}
        testID="ai-remove-quote"
        class="rounded-(--radius) p-0.5 hover:bg-list-hover active:bg-secondary-active"
      >
        <Icon name="x" size={12} />
      </Button>
    </View>
  );
}

/** 圆形发送按钮(AI 强调色)。 */
function SendButton(props: { disabled: boolean; onPress: () => void }) {
  return (
    <Button
      onPress={props.onPress}
      disabled={props.disabled}
      testID="ai-send"
      accessibilityLabel={t("chat.send_tooltip")}
      tooltip={tip(t("chat.send_tooltip"))}
      class="size-7 shrink-0 items-center justify-center rounded-full bg-ai hover:opacity-85 active:opacity-70 disabled:opacity-40"
    >
      <Icon name="arrow-up" size={15} color="on-ai" />
    </Button>
  );
}

export function ChatPanel() {
  const draft = signal("");
  const chrome = signal(CHROME_GUESS);
  let list: ListRef | undefined;

  const key = () => convoKey();
  const convo = () => aiStore.convo(key());
  const messages = () => convo()?.messages ?? [];
  const bound = () => convo()?.bound ?? null;
  const streaming = () => aiStore.streamingKey() !== null && aiStore.streamingKey() === key();
  const busyElsewhere = () => aiStore.streamingKey() !== null && aiStore.streamingKey() !== key();
  const status = () => aiStore.status();
  const ready = () => aiStore.ready();
  const mismatch = () => {
    const b = bound();
    const head = chatDeps.headConn();
    return !!b && !!head && b.connId !== head.id;
  };
  const cardTarget = () => bound() ?? target();
  const hasInput = () => draft().trim().length > 0 || aiStore.pendingRef() !== null;
  const showFix = () => {
    const last = messages()[messages().length - 1];
    return !streaming() && !!last && last.hidden && last.content.includes("✗");
  };
  const placeholder = () => {
    if (!target()) return t("chat.placeholder_no_conn");
    if (!ready()) return t("chat.placeholder_not_ready");
    if (busyElsewhere()) return t("chat.placeholder_busy");
    return t("chat.placeholder_ask");
  };
  const inputHeight = () => Math.min(MAX_INPUT, Math.max(MIN_INPUT, currentPrefs().aiInputHeight || MIN_INPUT));

  const send = () => {
    if (sendText(draft())) draft.set("");
  };

  // 切换连接时丢弃未发送的引用(首次运行不算切换)
  let lastKey: string | null | undefined;
  effect(() => {
    const k = key();
    if (lastKey !== undefined && k !== lastKey && untracked(() => aiStore.pendingRef()) !== null) setPendingRef(null);
    lastKey = k;
  });

  // 新消息到来时滚到底部(流式中的增长由列表的 maintainVisibleContentPosition 跟随)
  effect(() => {
    if (messages().length > 0) queueMicrotask(() => list?.scrollToEnd({ animated: false }));
  });

  return (
    <View class="flex-1 border-l border-ai/30 bg-panel" testID="ai-panel">
      <Header label={bound()?.label ?? null} hasMessages={messages().length > 0} />
      <If when={status() !== null && !ready()}>
        <View class="gap-2 border-b border-border px-3 py-2.5">
          <Hint>{status()?.error || t("chat.unavailable")}</Hint>
          <View class="flex-row">
            <Btn small variant="outline" label={t("chat.open_settings")} onPress={chatDeps.openSettings} testID="ai-open-settings" />
          </View>
        </View>
      </If>
      <If when={mismatch()}>
        <View class="border-b border-border px-3 py-2">
          <Text class={`${FS.xs} text-muted-foreground`}>{t("chat.bound_notice", { label: bound()?.label ?? "" })}</Text>
        </View>
      </If>
      <ResizablePanelGroup
        axis="vertical"
        class="flex-1"
        handleClass="bg-transparent"
        handleAccessibilityLabel={t("chat.title")}
      >
        <ResizablePanel minSize={120}>
          <If when={messages().length > 0} else={<EmptyIntro />}>
            <List
              ref={list}
              testID="ai-messages"
              each={messages()}
              keyExtractor={(m) => String(m.id)}
              estimatedItemSize={64}
              maintainVisibleContentPosition={{ autoscrollToBottomThreshold: 48 }}
              class="flex-1"
              contentClass="px-3 pt-3"
              footer={() => (
                <If when={showFix()}>
                  <View class="flex-row px-3 pb-3">
                    <Btn small variant="outline" label={t("chat.fix")} onPress={askFix} testID="ai-fix" />
                  </View>
                </If>
              )}
            >
              {(m, i) => (
                <MessageRow
                  msg={m()}
                  streaming={streaming() && i() === messages().length - 1}
                  kind={cardTarget()?.kind ?? "pg"}
                  target={cardTarget()?.label ?? ""}
                />
              )}
            </List>
          </If>
        </ResizablePanel>
        <ResizablePanel
          fixed
          size={inputHeight() + chrome()}
          minSize={MIN_INPUT + chrome()}
          maxSize={MAX_INPUT + chrome()}
          onResize={({ size }) =>
            updatePrefs({ aiInputHeight: Math.round(Math.min(MAX_INPUT, Math.max(MIN_INPUT, size - chrome()))) })
          }
        >
          <Composer
            draft={draft()}
            onDraft={(v) => draft.set(v)}
            placeholder={placeholder()}
            inputDisabled={!canSend() && !streaming()}
            sendDisabled={!canSend() || !hasInput()}
            onSend={send}
            onChrome={(h) => {
              if (Math.abs(h - untracked(chrome)) > 0.5) chrome.set(h);
            }}
          />
        </ResizablePanel>
      </ResizablePanelGroup>
      <SqlConfirmDialog />
    </View>
  );
}

function Composer(props: {
  draft: string;
  onDraft: (v: string) => void;
  placeholder: string;
  inputDisabled: boolean;
  sendDisabled: boolean;
  onSend: () => void;
  /** 报告输入框之外的高度,面板据此换算记住的输入框高度 */
  onChrome: (height: number) => void;
}) {
  // 输入框之外各部分的实测高度;它们都不收缩,输入行取剩余空间
  const quoteRow = signal(24);
  const modelRow = signal(24);
  const pendingBox = signal(0);
  effect(() => {
    const pending = aiStore.pendingRef() !== null ? pendingBox() + 6 : 0;
    // 上下内边距 20 + 顶边框 1 + 两处间距 12
    props.onChrome(33 + quoteRow() + modelRow() + pending);
  });
  return (
    <View class="flex-1 gap-1.5 border-t border-border p-2.5">
      <If when={aiStore.pendingRef()}>
        {(sql) => (
          <View class="shrink-0" onLayout={({ bounds }) => pendingBox.set(bounds.size.height)}>
            <PendingRef sql={sql()} />
          </View>
        )}
      </If>
      <View class="shrink-0 flex-row" onLayout={({ bounds }) => quoteRow.set(bounds.size.height)}>
        <Btn
          small
          variant="ghost"
          icon="quote"
          label={t("chat.quote_selection")}
          tooltip={t("chat.quote_selection_tooltip")}
          onPress={quoteSelection}
          testID="ai-quote-selection"
        />
      </View>
      <View class="min-h-[44px] flex-1 flex-row items-end gap-2">
        <Input
          testID="ai-input"
          multiline
          submitBehavior="submit"
          value={props.draft}
          onChange={({ value }) => props.onDraft(value)}
          onSubmit={props.onSend}
          placeholder={props.placeholder}
          disabled={props.inputDisabled}
          class={`h-full min-h-[44px] flex-1 rounded-(--radius) border border-input bg-background px-2.5 py-2 ${FS.sm} text-foreground placeholder:text-fg-faint focus:border-ring disabled:opacity-60`}
        />
        <SendButton disabled={props.sendDisabled} onPress={props.onSend} />
      </View>
      <View class="shrink-0 flex-row items-center gap-2" onLayout={({ bounds }) => modelRow.set(bounds.size.height)}>
        <ModelPicker />
        <View class="flex-1" />
        <Text class="text-[10.5px] text-fg-faint line-clamp-1">{t("chat.input_hint")}</Text>
      </View>
    </View>
  );
}
