// AI 助手面板:按连接分会话、绑定连接条、模型切换、清空、引用选中 SQL、流式回复(含 AI 自己查数据的
// 工具步骤)、停止 / 复制 / 重新生成、回到最新、未配置时引导去设置。
// 布局参照 nsh gallery 的对话示例:回复上方是模型名,用户消息是描边气泡,输入框随内容长高;
// 消息与输入框放在最宽 820 的居中一栏里,面板变宽(全屏)时也好读。

import { Clipboard, computed, effect, type ListRef, List, signal, Text, untracked, View } from "nsh";
import { Button, Input, TextView } from "nsh/base";
import { t } from "nsh/i18n";
import { chat as chatNative } from "../lib/native";
import type { DbKind } from "../lib/types";
import { type AiMessage, aiStore, setPendingRef, stopStreaming, type ToolStep } from "../store/ai";
import { toast } from "../store/toast";
import { toggleAiFullscreen } from "../store/workspace";
import { Btn, Hint, Icon, IconButton, tip } from "../ui/kit";
import { FS } from "../ui/text";
import {
  askFix,
  canSend,
  convoKey,
  newConversation,
  openConversation,
  quoteSelection,
  regenerate,
  removeConversation,
  sendText,
  target,
} from "./actions";
import { cardKey } from "./cards";
import { chatDeps } from "./deps";
import { ModelPicker } from "./ModelPicker";
import { SqlCard, SqlConfirmDialog } from "./SqlCard";
import { ResultTable, ShownNote, ToolCard } from "./ToolCard";
import { ErrorText } from "../ui/error";
import { DropMenu, MItem, MSep } from "../ui/menu";

/** 消息与输入框所在的居中一栏(面板窄时就是全宽)。 */
const COLUMN = "w-full max-w-[820px] self-center";
/** 输入框随内容长高的上下限。 */
const MIN_INPUT = 20;
const MAX_INPUT = 160;
/** 离底部多近算「在底部」(跟随新内容)。 */
const FOLLOW_THRESHOLD = 48;

/** 每秒跳一次的时钟:「思考中… N 秒」用;只在有回复进行时走。 */
const now = signal(Date.now());
let ticker: ReturnType<typeof setInterval> | undefined;
effect(() => {
  const busy = aiStore.streamingKey() !== null;
  if (busy && !ticker) {
    now.set(Date.now());
    ticker = setInterval(() => now.set(Date.now()), 1000);
  } else if (!busy && ticker) {
    clearInterval(ticker);
    ticker = undefined;
  }
});

/** 历史对话的时间:刚刚 / N 分钟前 / N 小时前 / 日期。 */
function whenText(ms: number): string {
  if (!ms) return "";
  const mins = Math.floor((Date.now() - ms) / 60000);
  if (mins < 1) return t("chat.history_now");
  if (mins < 60) return t("chat.history_minutes", { n: mins });
  if (mins < 24 * 60) return t("chat.history_hours", { n: Math.floor(mins / 60) });
  return new Date(ms).toLocaleDateString();
}

/** 当前连接的历史对话(有内容的),最近的在前。 */
function historyList() {
  const c = chatDeps.headConn();
  return c ? aiStore.history(c.id) : [];
}

/** 侧边面板顶栏的「历史」菜单。 */
function HistoryMenu() {
  return (
    <DropMenu
      testID="ai-history"
      placement="bottom"
      align="end"
      popupClass="max-h-[360px] min-w-64"
      trigger={<IconButton icon="rotate-ccw-clock" size={14} tooltip={t("chat.history")} class="p-1" />}
    >
      <Each of={historyList()} key={(h) => h.key}>
        {(h) => (
          <MItem
            label={`${h().key === convoKey() ? "● " : ""}${h().title || t("chat.untitled")} · ${whenText(h().updatedAt)}`}
            onSelect={() => openConversation(h().key)}
          />
        )}
      </Each>
      <MSep />
      <MItem label={t("chat.new_chat")} onSelect={newConversation} />
    </DropMenu>
  );
}

/** 对话工作台左侧:当前连接的历史对话。 */
function HistoryRail() {
  return (
    <View testID="ai-history-rail" class="w-[240px] shrink-0 border-r border-border">
      <View class="h-10 shrink-0 flex-row items-center px-3">
        <Text class={`${FS.xs} flex-1 font-bold text-muted-foreground`}>{t("chat.history")}</Text>
        <IconButton icon="plus" size={14} tooltip={t("chat.new_chat")} class="p-1" onPress={newConversation} />
      </View>
      <If when={historyList().length > 0} else={<Hint class={`${FS.xs} px-3`}>{t("chat.history_empty")}</Hint>}>
        <List each={historyList()} keyExtractor={(h) => h.key} estimatedItemSize={48} class="flex-1" contentClass="px-1.5 pb-2">
          {(h) => (
            <View
              testID={`ai-history-item:${h().title}`}
              role="button"
              accessibilityLabel={h().title || t("chat.untitled")}
              onPress={() => openConversation(h().key)}
              class="flex-row items-start gap-1 rounded-(--radius) py-1.5 pl-2.5 pr-1 hover:bg-list-hover"
              classList={{ "bg-raised": h().key === convoKey() }}
            >
              <View class="min-w-0 flex-1 gap-0.5">
                <Text
                  class={`${FS.sm} line-clamp-2`}
                  classList={{ "font-bold text-foreground": h().key === convoKey(), "text-foreground": h().key !== convoKey() }}
                >
                  {h().title || t("chat.untitled")}
                </Text>
                <Text class={`${FS.xs} text-fg-faint`}>{whenText(h().updatedAt)}</Text>
              </View>
              <IconButton icon="x" size={12} tooltip={t("chat.delete_convo")} class="p-1" onPress={() => removeConversation(h().key)} />
            </View>
          )}
        </List>
      </If>
    </View>
  );
}

function Header(props: { label: string | null; hasMessages: boolean; wide: boolean }) {
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
          icon="plus"
          label={t("chat.new_chat")}
          tooltip={t("chat.new_chat_tooltip")}
          testID="ai-new"
          onPress={newConversation}
        />
      </If>
      <If when={!props.wide && historyList().length > 0}>
        <HistoryMenu />
      </If>
      <Btn
        small
        variant="ghost"
        icon={props.wide ? "panel-left" : "maximize"}
        label={props.wide ? t("chat.exit_workbench") : t("chat.workbench")}
        tooltip={t("chat.workbench_tooltip")}
        testID="ai-fullscreen"
        onPress={toggleAiFullscreen}
      />
    </View>
  );
}

/** 空会话:标题、说明与几条可直接发送的建议。 */
function EmptyState(props: { canAsk: boolean }) {
  const suggestions = () => [
    { icon: "list-tree" as const, title: t("chat.suggest_tables"), prompt: t("chat.suggest_tables_prompt") },
    { icon: "table" as const, title: t("chat.suggest_biggest"), prompt: t("chat.suggest_biggest_prompt") },
    { icon: "scan-search" as const, title: t("chat.suggest_quality"), prompt: t("chat.suggest_quality_prompt") },
    { icon: "wand-sparkles" as const, title: t("chat.suggest_create"), prompt: t("chat.suggest_create_prompt") },
  ];
  return (
    <View class={`${COLUMN} flex-1 justify-center gap-5 px-4 py-6`}>
      <View class="items-center gap-2">
        <View class="h-11 w-11 items-center justify-center rounded-[12px] border border-border bg-raised">
          <Icon name="sparkles" size={20} color="ai" />
        </View>
        <Text class={`${FS.base} font-bold text-foreground`}>{t("chat.empty_title")}</Text>
        <Text class={`${FS.sm} text-center text-muted-foreground`}>{t("chat.empty_sub")}</Text>
      </View>
      <View class="flex-row flex-wrap gap-2">
        <Each of={suggestions()}>
          {(s, i) => (
            <View
              testID={`ai-suggestion-${i()}`}
              role="button"
              accessibilityLabel={s().title}
              onPress={() => props.canAsk && sendText(s().prompt)}
              class="min-w-[150px] flex-1 basis-[45%] gap-1.5 rounded-(--radius) border border-border bg-background p-3 hover:bg-list-hover"
              classList={{ "opacity-50": !props.canAsk }}
            >
              <Icon name={s().icon} size={15} color="ai" />
              <Text class={`${FS.sm} font-bold text-foreground`}>{s().title}</Text>
              <Text class={`${FS.xs} text-muted-foreground line-clamp-2`}>{s().prompt}</Text>
            </View>
          )}
        </Each>
      </View>
      <Text class={`${FS.xs} text-center text-fg-faint`}>{t("chat.empty_confirm")}</Text>
    </View>
  );
}

/** 一段回复文字:切成文本(Markdown)与 SQL 卡片。`base` 让同一条消息里各段的卡片 key 不重复。 */
function ReplyText(props: { msgId: number; text: string; base: number; streaming: boolean; kind: DbKind; target: string }) {
  const segments = computed(() =>
    chatNative.segments(props.text).map((s, i) => ({ ...s, key: `${props.msgId}:${props.base + i}:${s.kind}`, index: props.base + i })),
  );
  return (
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
              cardKey={cardKey(props.msgId, seg().index)}
              sql={seg().text}
              streaming={props.streaming}
              kind={props.kind}
              target={props.target}
            />
          </If>
        )}
      </Each>
    </View>
  );
}

/** 回复里可复制的文字(文字段拼接,不含工具步骤)。 */
function replyText(m: AiMessage): string {
  return m.parts ? m.parts.flatMap((p) => (p.kind === "text" ? [p.text] : [])).join("\n") : m.content;
}

/** 回复下方的一行操作:复制、重新生成(只在最后一条回复上)。 */
function Actions(props: { msg: AiMessage; last: boolean }) {
  const copied = signal(false);
  const copy = async () => {
    await Clipboard.writeText(replyText(props.msg));
    copied.set(true);
    toast.success(t("chat.copied"));
    setTimeout(() => copied.set(false), 1500);
  };
  return (
    <View class="-ml-1 flex-row items-center gap-0.5">
      <IconButton
        icon={copied() ? "check" : "copy"}
        size={13}
        tooltip={t("chat.copy")}
        label={t("chat.copy")}
        onPress={() => void copy()}
        class="p-1"
      />
      <If when={props.last}>
        <IconButton icon="refresh-cw" size={13} tooltip={t("chat.regenerate")} label={t("chat.regenerate")} onPress={regenerate} class="p-1" />
      </If>
    </View>
  );
}

/** 一条消息。列表单元格会复用到别的消息上:所有读取都在绑定里进行。 */
function MessageRow(props: {
  msg: AiMessage;
  streaming: boolean;
  last: boolean;
  kind: DbKind;
  target: string;
  wide: boolean;
  onOpenQuery: (sql: string) => void;
}) {
  const assistant = () => props.msg.role === "assistant" && !props.msg.hidden;
  const plain = () => assistant() && !props.msg.parts && !!props.msg.content;
  // 工具模式:文字段与工具步骤按顺序;每段文字的卡片序号从 段序号 × 1000 起,互不冲突
  const parts = () => (assistant() ? (props.msg.parts ?? []) : []);
  const empty = () => !props.msg.content && !(props.msg.parts?.length ?? 0);
  const waited = () => Math.max(0, Math.floor((now() - (props.msg.startedAt ?? now())) / 1000));
  return (
    <View class={`${COLUMN} px-3 pb-4`}>
      <If when={props.msg.hidden}>
        <View class="gap-1.5">
          <Text class={`${FS.xs} px-1 text-fg-faint`}>{props.msg.content}</Text>
          <If when={props.msg.table}>
            {(tb) => (
              <View testID="ai-result" class="gap-1.5 rounded-(--radius) border border-border bg-background px-3 py-2">
                <ResultTable columns={(tb().columns ?? []) as string[]} rows={tb().rows ?? []} wide={props.wide} />
                <ShownNote columns={tb().columns?.length ?? 0} rows={tb().rows?.length ?? 0} wide={props.wide} />
              </View>
            )}
          </If>
        </View>
      </If>
      <If when={!props.msg.hidden && props.msg.role === "user"}>
        <View class="flex-row justify-end">
          <View class="max-w-[85%] rounded-[14px] rounded-br-[5px] border border-border bg-raised px-3 py-2">
            <Text class={`${FS.sm} select-text text-foreground`}>{props.msg.content}</Text>
          </View>
        </View>
      </If>
      <If when={assistant()}>
        <View class="gap-2">
          <View class="h-5 flex-row items-center gap-1.5">
            <View class="h-5 w-5 items-center justify-center rounded-full bg-ai/15">
              <Icon name="sparkles" size={11} color="ai" />
            </View>
            <Text class={`${FS.xs} font-bold text-muted-foreground`}>{props.msg.model ?? t("chat.title")}</Text>
          </View>
          <If when={empty() && props.streaming}>
            <View testID="ai-thinking" class="flex-row items-center gap-2">
              <Icon name="loader-circle" size={12} color="ai" />
              <Text class={`${FS.xs} text-muted-foreground`}>{t("chat.thinking_for", { n: waited() })}</Text>
            </View>
          </If>
          <If when={plain()}>
            <ReplyText msgId={props.msg.id} text={props.msg.content} base={0} streaming={props.streaming} kind={props.kind} target={props.target} />
          </If>
          <If when={parts().length > 0}>
            <View class="gap-2">
              <Each of={parts().map((p, i) => ({ p, i }))} key={(x) => (x.p.kind === "tool" ? `t:${x.p.id}` : `x:${x.i}`)}>
                {(x) => (
                  <If
                    when={x().p.kind === "tool"}
                    else={
                      <ReplyText
                        msgId={props.msg.id}
                        text={(x().p as { text: string }).text}
                        base={x().i * 1000}
                        streaming={props.streaming && x().i === parts().length - 1}
                        kind={props.kind}
                        target={props.target}
                      />
                    }
                  >
                    <ToolCard
                      testID={`ai-tool-${x().i}`}
                      step={x().p as ToolStep}
                      wide={props.wide}
                      onOpenQuery={props.onOpenQuery}
                    />
                  </If>
                )}
              </Each>
            </View>
          </If>
          <If when={props.msg.error}>
            {(e) => (
              <View testID="ai-error" class="flex-row items-start gap-2 rounded-(--radius) border border-destructive/30 bg-destructive/5 px-3 py-2">
                <Icon name="circle-alert" size={13} color="negative" class="mt-0.5" />
                <ErrorText size="xs" class="min-w-0 flex-1" text={e()} />
                <If when={props.last}>
                  <Btn small variant="outline" icon="refresh-cw" label={t("chat.retry")} onPress={regenerate} testID="ai-retry" />
                </If>
              </View>
            )}
          </If>
          <If when={props.msg.stopped}>
            <Text testID="ai-stopped" class={`${FS.xs} text-fg-faint`}>{t("chat.stopped")}</Text>
          </If>
          <If when={!props.streaming && !props.msg.error && !empty()}>
            <Actions msg={props.msg} last={props.last} />
          </If>
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

/** 圆形发送按钮(AI 强调色);回复进行中换成停止按钮。 */
function SendButton(props: { disabled: boolean; onPress: () => void; streaming: boolean }) {
  return (
    <>
      <If when={!props.streaming}>
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
      </If>
      <If when={props.streaming}>
        <Button
          onPress={stopStreaming}
          testID="ai-stop"
          accessibilityLabel={t("chat.stop")}
          tooltip={tip(t("chat.stop"))}
          class="size-7 shrink-0 items-center justify-center rounded-full bg-foreground hover:opacity-85 active:opacity-70"
        >
          <View class="h-2.5 w-2.5 rounded-[2px] bg-background" />
        </Button>
      </If>
    </>
  );
}

/** `wide`:对话工作台(铺满主区域)。 */
export function ChatPanel(props: { wide?: boolean } = {}) {
  const wide = () => !!props.wide;
  const draft = signal("");
  const inputHeight = signal(MIN_INPUT);
  /** 是否停在底部:是则新内容自动跟随;用户往上翻就显示「回到最新」 */
  const following = signal(true);
  let list: ListRef | undefined;
  let dragging = false;

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
  const lastAssistant = () => {
    const ms = messages();
    for (let i = ms.length - 1; i >= 0; i--) if (ms[i].role === "assistant" && !ms[i].hidden) return ms[i].id;
    return -1;
  };
  const placeholder = () => {
    if (!target()) return t("chat.placeholder_no_conn");
    if (!ready()) return t("chat.placeholder_not_ready");
    if (busyElsewhere()) return t("chat.placeholder_busy");
    return t("chat.placeholder_ask");
  };

  const send = () => {
    if (sendText(draft())) {
      draft.set("");
      following.set(true);
    }
  };
  const jumpToLatest = () => {
    following.set(true);
    list?.scrollToEnd({ animated: true });
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
    <View class="flex-1 flex-row bg-panel" classList={{ "border-l border-ai/30": !wide() }} testID={wide() ? "ai-panel-wide" : "ai-panel"}>
      <If when={wide()}>
        <HistoryRail />
      </If>
      <View class="min-w-0 flex-1">
      <Header label={bound()?.label ?? null} hasMessages={messages().length > 0} wide={wide()} />
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
      <View class="min-h-0 flex-1">
        <If when={messages().length > 0} else={<EmptyState canAsk={canSend()} />}>
          <List
            ref={list}
            testID="ai-messages"
            each={messages()}
            keyExtractor={(m) => String(m.id)}
            estimatedItemSize={80}
            maintainVisibleContentPosition={{ autoscrollToBottomThreshold: FOLLOW_THRESHOLD }}
            scrollEventThrottle={100}
            onScrollDragStart={() => (dragging = true)}
            onScroll={(e) => {
              if (dragging) following.set(e.offset.y + e.size.height >= e.contentSize.height - FOLLOW_THRESHOLD);
            }}
            onScrollEnd={() => (dragging = false)}
            onMomentumEnd={() => (dragging = false)}
            class="flex-1"
            contentClass="pt-3"
            footer={() => (
              <If when={showFix()}>
                <View class={`${COLUMN} flex-row px-3 pb-3`}>
                  <Btn small variant="outline" label={t("chat.fix")} onPress={askFix} testID="ai-fix" />
                </View>
              </If>
            )}
          >
            {(m, i) => (
              <MessageRow
                msg={m()}
                streaming={streaming() && i() === messages().length - 1}
                last={m().id === lastAssistant()}
                kind={cardTarget()?.kind ?? "pg"}
                target={cardTarget()?.label ?? ""}
                wide={wide()}
                onOpenQuery={(sql) => {
                  const to = cardTarget();
                  if (to) chatDeps.openQuery(sql, to.connId);
                }}
              />
            )}
          </List>
        </If>
        <If when={!following() && messages().length > 0}>
          <View class="absolute bottom-3 right-4">
            <Button
              testID="ai-jump"
              onPress={jumpToLatest}
              accessibilityLabel={t("chat.jump_latest")}
              tooltip={tip(t("chat.jump_latest"))}
              class="size-8 items-center justify-center rounded-full border border-border bg-popover shadow-md hover:bg-list-hover"
            >
              <Icon name="chevron-down" size={15} color="foreground" />
              <If when={streaming()}>
                <View class="absolute right-0 top-0 h-2.5 w-2.5 rounded-full border-2 border-popover bg-ai" />
              </If>
            </Button>
          </View>
        </If>
      </View>
      <Composer
        draft={draft()}
        onDraft={(v) => draft.set(v)}
        inputHeight={inputHeight()}
        onInputHeight={(h) => inputHeight.set(h)}
        placeholder={placeholder()}
        inputDisabled={!canSend() && !streaming()}
        sendDisabled={!canSend() || !hasInput()}
        streaming={streaming()}
        onSend={send}
      />
      </View>
      <SqlConfirmDialog />
    </View>
  );
}

/** 输入区:圆角框里是引用、随内容长高的输入框,下方一行放引用选中、模型与发送 / 停止。 */
function Composer(props: {
  draft: string;
  onDraft: (v: string) => void;
  inputHeight: number;
  onInputHeight: (h: number) => void;
  placeholder: string;
  inputDisabled: boolean;
  sendDisabled: boolean;
  streaming: boolean;
  onSend: () => void;
}) {
  return (
    <View class={`${COLUMN} shrink-0 px-3 pb-2.5 pt-1`}>
      <View class="gap-1.5 rounded-[14px] border border-input bg-background p-1.5 focus-within:border-ring">
        <If when={aiStore.pendingRef()}>{(sql) => <PendingRef sql={sql()} />}</If>
        <Input
          testID="ai-input"
          multiline
          submitBehavior="submit"
          value={props.draft}
          onChange={({ value }) => props.onDraft(value)}
          onSubmit={() => {
            if (!props.streaming) props.onSend();
          }}
          onContentSizeChange={({ size }) => props.onInputHeight(Math.min(MAX_INPUT, Math.max(MIN_INPUT, size.height)))}
          placeholder={props.placeholder}
          disabled={props.inputDisabled}
          class={`px-2 py-1.5 ${FS.sm} text-foreground placeholder:text-fg-faint disabled:opacity-60`}
          style={{ height: props.inputHeight + 12 }}
        />
        <View class="flex-row items-center gap-1">
          <IconButton
            icon="quote"
            size={14}
            tooltip={t("chat.quote_selection_tooltip")}
            label={t("chat.quote_selection")}
            onPress={quoteSelection}
            class="p-1.5"
          />
          <View class="min-w-0 shrink">
            <ModelPicker />
          </View>
          {/* 提示文字占剩余空间、可被挤没,发送按钮始终留在行尾 */}
          <Text class="min-w-0 flex-1 text-right text-[10.5px] text-fg-faint line-clamp-1">{t("chat.input_hint")}</Text>
          <SendButton disabled={props.sendDisabled} onPress={props.onSend} streaming={props.streaming} />
        </View>
      </View>
    </View>
  );
}
