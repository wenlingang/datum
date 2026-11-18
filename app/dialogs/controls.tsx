// 对话框共用的样式控件:外框、字段标签、分段选择、下拉选择、掩码输入、复选框、开关、操作菜单。
// 都套在 nsh/base 的行为组件上,颜色只用主题 token。

import { EditorKeys } from "datum/native";
import { batch, Overlay, Scroll, signal, Text, View, type JSX, type ViewRef } from "nsh";
import { Button, Checkbox, Dialog, Input, Select, Switch, Toggle, ToggleGroup, type InputProps } from "nsh/base";
import { t } from "nsh/i18n";
import { color as tokenColor, Icon, IconButton, SectionLabel, TextField, tip, Btn, type IconName, type ButtonVariant } from "../ui/kit";
import { DropMenu, MItem, MSep } from "../ui/menu";
import { FS } from "../ui/text";
import { ErrorText } from "../ui/error";

/** 对话框外框:标题(可带 16 号图标)+ 右上角关闭 + 内容;Esc 关闭。`width` 与 GPUI 版一致(点)。
 *  应用里的对话框都用它(或下面的 ConfirmFrame),不要自己拼 Dialog 外观。 */
export function DialogFrame(props: {
  open: boolean;
  onClose: () => void;
  title: string;
  width: number;
  /** 标题前的图标(16 号) */
  icon?: IconName;
  /** 图标颜色 token(默认 foreground) */
  iconColor?: string;
  /** 点遮罩是否关闭(默认 true) */
  closeOnBackdrop?: boolean;
  /** 读屏角色:确认类用 alertdialog */
  role?: "dialog" | "alertdialog";
  testID?: string;
  children: JSX.Element;
}) {
  return (
    <Dialog
      open={props.open}
      testID={props.testID}
      role={props.role}
      accessibilityLabel={props.title}
      animation="fade"
      closeOnBackdropPress={props.closeOnBackdrop ?? true}
      onOpenChange={({ open }) => {
        if (!open) props.onClose();
      }}
      backdropClass="bg-overlay"
      class="m-auto gap-4 rounded-(--radius-lg) border border-border bg-popover px-6 pb-6 pt-5 shadow-2xl"
      style={{ width: props.width, maxWidth: "94%", maxHeight: "94%" }}
    >
      <View class="flex-row items-center gap-2">
        <If when={props.icon}>{(n) => <Icon name={n()} size={16} color={props.iconColor ?? "foreground"} />}</If>
        <Text class={`${FS.lg} min-w-0 flex-1 font-semibold text-popover-foreground line-clamp-2`}>{props.title}</Text>
        <IconButton
          icon="x"
          label={t("import.close")}
          testID={props.testID ? `${props.testID}-dismiss` : undefined}
          onPress={props.onClose}
          class="-mr-2"
        />
      </View>
      {props.children}
    </Dialog>
  );
}

/** 确认框:DialogFrame 的外观 + 底部「取消 / 确认」。内容放说明、只读预览、勾选确认等。
 *  nsh/base 的 AlertDialog 是系统原生弹窗,只放得下标题和一句话,只有这种纯文字确认才用它。 */
export function ConfirmFrame(props: {
  open: boolean;
  onClose: () => void;
  title: string;
  /** 默认 560 */
  width?: number;
  icon?: IconName;
  iconColor?: string;
  testID?: string;
  cancelLabel: string;
  cancelTestID?: string;
  confirmLabel: string;
  confirmTestID?: string;
  /** 默认 primary;破坏性操作用 danger */
  confirmVariant?: ButtonVariant;
  confirmIcon?: IconName;
  confirmDisabled?: boolean;
  onConfirm: () => void;
  children?: JSX.Element;
}) {
  return (
    <DialogFrame
      role="alertdialog"
      open={props.open}
      onClose={props.onClose}
      title={props.title}
      width={props.width ?? 560}
      icon={props.icon}
      iconColor={props.iconColor}
      testID={props.testID}
    >
      <View class="min-h-0 shrink gap-3">{props.children}</View>
      <View class="flex-row justify-end gap-2">
        <Btn variant="outline" label={props.cancelLabel} onPress={props.onClose} testID={props.cancelTestID} />
        <Btn
          variant={props.confirmVariant ?? "primary"}
          icon={props.confirmIcon}
          label={props.confirmLabel}
          disabled={props.confirmDisabled}
          onPress={props.onConfirm}
          testID={props.confirmTestID}
        />
      </View>
    </DialogFrame>
  );
}

/** 字段:小号加粗标签 + 控件。 */
export function Field(props: { label: string; children: JSX.Element; class?: string; style?: { width?: number } }) {
  return (
    <View class={`min-w-0 gap-2 ${props.class ?? "flex-1"}`} style={props.style}>
      <SectionLabel>{props.label}</SectionLabel>
      {props.children}
    </View>
  );
}

export interface SegmentOption<T extends string> {
  value: T;
  label: string;
  /** 选中时文字与圆点的颜色 token;不给则用前景色且不画圆点 */
  color?: string;
}

/** 胶囊分段选择(如环境):ToggleGroup 里一组单选的 Toggle。 */
export function Segmented<T extends string>(props: {
  options: SegmentOption<T>[];
  value: T;
  onChange: (v: T) => void;
  testID?: string;
}) {
  return (
    <ToggleGroup class="flex-row gap-1.5 rounded-full bg-raised p-1" testID={props.testID}>
      <Each of={props.options} key={(o) => o.value}>
        {(o) => {
          const active = () => props.value === o().value;
          const tone = () => (active() ? (o().color ?? "foreground") : "fg-faint");
          return (
            <Toggle
              pressed={active()}
              // 单选:再点已选中的项(请求 pressed:false)不取消
              onChange={(e) => {
                if (e.pressed) props.onChange(o().value);
              }}
              accessibilityLabel={o().label}
              // aria-pressed: 优先级高于 hover:,悬停不会盖掉选中底色
              class="h-8 flex-1 flex-row items-center justify-center gap-2 rounded-full hover:bg-list-hover aria-pressed:bg-background aria-pressed:shadow-sm"
            >
              <If when={o().color}>
                <View class="rounded-full" style={{ width: 7, height: 7, backgroundColor: tokenColor(tone()) }} />
              </If>
              <Text class={`${FS.xs} font-bold`} style={{ color: tokenColor(tone()) }}>
                {o().label}
              </Text>
            </Toggle>
          );
        }}
      </Each>
    </ToggleGroup>
  );
}

export interface DropdownItem {
  value: string;
  title: string;
  icon?: IconName;
  /** 品牌图标等自带颜色的图(替代 icon) */
  leading?: () => JSX.Element;
}

/** 描边下拉选择:触发器显示当前项,弹出列表(当前项打勾)。 */
export function Dropdown(props: {
  items: DropdownItem[];
  value: string | null;
  onChange: (value: string) => void;
  /** 触发器上的文字 */
  label: string;
  /** 触发器前置内容(图标) */
  leading?: JSX.Element;
  placeholder?: string;
  small?: boolean;
  disabled?: boolean;
  tooltip?: string;
  accessibilityLabel?: string;
  testID?: string;
  class?: string;
  style?: { width?: number };
}) {
  const open = signal(false);
  return (
    <Select
      items={props.items.map((i) => ({ value: i.value, title: i.title }))}
      value={props.value}
      open={open()}
      onOpenChange={(e) => open.set(e.open)}
      onChange={({ value }) => {
        if (value != null) props.onChange(value);
      }}
      disabled={props.disabled}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
      testID={props.testID}
      offset={4}
      margin={8}
      class={props.class}
      style={props.style}
      popupClass="min-w-40 rounded-(--radius-lg) border border-border bg-popover p-1 shadow-lg"
      listClass="max-h-80"
      itemClass="group h-7 flex-row items-center gap-2 rounded-(--radius) px-2 hover:bg-accent focus:bg-accent disabled:opacity-40"
      renderItem={(entry) => {
        const item = props.items[entry.index];
        return (
          <View class="flex-1 flex-row items-center gap-2">
            {item?.leading ? item.leading() : item?.icon ? <Icon name={item.icon} size={14} /> : null}
            <Text class={`${FS.sm} flex-1 text-popover-foreground line-clamp-1`}>{entry.title}</Text>
            <Icon name="check" size={13} color="foreground" class="opacity-0 group-selected:opacity-100" />
          </View>
        );
      }}
      trigger={
        <View
          tooltip={props.tooltip ? tip(props.tooltip) : undefined}
          class={`flex-row items-center gap-2 rounded-(--radius) border border-input bg-background hover:bg-list-hover ${props.small ? "h-7 px-2.5" : "h-8 px-3"}`}
          classList={{ "bg-secondary-active": open(), "opacity-40": !!props.disabled }}
        >
          {props.leading}
          <Text
            class={`${props.small ? FS.xs : FS.sm} flex-1 line-clamp-1`}
            classList={{
              "text-foreground": !!props.label,
              "text-fg-faint": !props.label,
            }}
          >
            {props.label || props.placeholder || ""}
          </Text>
          <Icon name="chevron-down" size={14} />
        </View>
      }
    />
  );
}

/** 带「显示/隐藏」切换的掩码输入框(密码、API Key)。 */
export function MaskedField(props: InputProps & { testID?: string }) {
  const shown = signal(false);
  return (
    <View class="relative justify-center">
      <TextField {...props} masked={!shown()} class="pr-8" />
      <IconButton
        icon={shown() ? "eye-off" : "eye"}
        label={shown() ? "hide" : "show"}
        onPress={() => shown.update((v) => !v)}
        size={14}
        class="absolute right-1"
      />
    </View>
  );
}

/** 复选框(模型勾选)。 */
export function CheckBox(props: { checked: boolean; onChange: (v: boolean) => void; label: string; testID?: string }) {
  return (
    <Checkbox
      checked={props.checked}
      onChange={({ checked }) => props.onChange(checked)}
      accessibilityLabel={props.label}
      testID={props.testID}
      hitSlop={6}
      class="group"
      indicatorClass="h-4 w-4 items-center justify-center rounded-(--radius-sm) border border-input bg-background checked:border-primary checked:bg-primary"
      indicator={<Icon name="check" size={12} color="primary-foreground" class="opacity-0 group-checked:opacity-100" />}
    />
  );
}

/** 开关(服务商启用)。 */
export function SwitchField(props: { checked: boolean; onChange: (v: boolean) => void; label: string; testID?: string }) {
  return (
    <Switch
      checked={props.checked}
      onChange={({ checked }) => props.onChange(checked)}
      accessibilityLabel={props.label}
      testID={props.testID}
      class="flex-row items-center gap-2"
      trackClass="h-5 w-9 justify-center rounded-full bg-switch-off px-0.5 checked:bg-primary"
      thumbClass="h-4 w-4 rounded-full bg-background shadow-sm checked:translate-x-4"
    >
      <Text class={`${FS.sm} text-foreground`}>{props.label}</Text>
    </Switch>
  );
}

export interface MenuAction {
  label: string;
  onSelect: () => void;
  separatorBefore?: boolean;
}

/** 操作菜单:按钮触发,列出若干动作(添加服务商)。菜单项用 ui/menu 的 MItem(动作推迟一拍)。 */
export function ActionMenu(props: { trigger: JSX.Element; actions: MenuAction[]; testID?: string }) {
  return (
    <DropMenu trigger={props.trigger} testID={props.testID} placement="top">
      <Each of={props.actions}>
        {(a) => (
          <>
            <If when={a().separatorBefore}>
              <MSep />
            </If>
            <MItem label={a().label} onSelect={a().onSelect} />
          </>
        )}
      </Each>
    </DropMenu>
  );
}

/** 带候选的输入框(可搜索的下拉输入):聚焦或点击时展开候选,打开时先列出全部,
 *  开始输入后按输入过滤;↑/↓ 移动高亮,回车或点击选中,Esc 收起;仍可输入列表外的值。
 *  候选由调用方在 `onOpen` 里加载,`state` 告诉它加载中 / 失败 / 已就绪。 */
export function SuggestField(props: {
  value: string;
  onChange: (value: string) => void;
  /** 展开时调用:由调用方按需(参数变了才)重新加载候选 */
  onOpen?: () => void;
  items: string[];
  state: "idle" | "loading" | "ok" | "error";
  error?: string | null;
  loadingText: string;
  emptyText: string;
  placeholder?: string;
  testID?: string;
  /** 右侧展开按钮的无障碍名称 */
  toggleLabel?: string;
}) {
  const open = signal(false);
  const focused = signal(false);
  /** 输入框的宽度:候选列表与它等宽 */
  const width = signal(240);
  let anchor: ViewRef | undefined;
  /** null:展开后还没输入,列出全部;否则按它过滤 */
  const filter = signal<string | null>(null);
  const highlight = signal(0);
  const shown = () => {
    const f = filter()?.trim().toLowerCase();
    return f ? props.items.filter((x) => x.toLowerCase().includes(f)) : props.items;
  };
  const show = () => {
    if (open()) return;
    batch(() => {
      filter.set(null);
      highlight.set(Math.max(0, props.items.indexOf(props.value)));
      open.set(true);
    });
    props.onOpen?.();
  };
  /** 选中 / Esc 收起后的一小段时间里,光标移动不算「点进输入框」 */
  let quietUntil = 0;
  const close = () => {
    quietUntil = Date.now() + 250;
    open.set(false);
  };
  const choose = (v: string) => {
    props.onChange(v);
    close();
  };
  const onKey = (k: string) => {
    if (k === "escape") return close();
    if (!open()) return show();
    const n = shown().length;
    if (n === 0) return;
    highlight.update((i) => (k === "down" ? (i + 1) % n : (i - 1 + n) % n));
  };
  return (
    <View class="relative" ref={anchor} onLayout={({ bounds }) => width.set(bounds.size.width)}>
      <EditorKeys keys={["down", "up", "escape"]} active={focused()} onKey={({ keystroke }) => onKey(keystroke)} />
      <Input
        testID={props.testID}
        value={props.value}
        placeholder={props.placeholder}
        submitBehavior="submit"
        onFocus={() => {
          focused.set(true);
          show();
        }}
        onBlur={() => {
          focused.set(false);
          // 点候选时输入框先失焦:稍等,让点击先选中
          setTimeout(() => {
            if (!focused()) open.set(false);
          }, 150);
        }}
        // 已聚焦时再点输入框:光标移动 → 重新展开
        onCaretMove={() => {
          if (focused() && !open() && Date.now() > quietUntil) show();
        }}
        onChange={({ value }) => {
          batch(() => {
            props.onChange(value);
            filter.set(value);
            highlight.set(0);
            open.set(true);
          });
        }}
        onSubmit={() => {
          const pick = open() ? shown()[highlight()] : undefined;
          if (pick !== undefined) choose(pick);
          else close();
        }}
        class={`h-8 rounded-(--radius) border border-input bg-background pl-2.5 pr-8 ${FS.sm} text-foreground placeholder:text-fg-faint focus:border-ring`}
      />
      <View class="absolute bottom-0 right-1 top-0 justify-center">
        <Button
          testID={props.testID ? `${props.testID}-toggle` : undefined}
          accessibilityLabel={props.toggleLabel}
          onPress={() => (open() ? close() : show())}
          class="rounded-(--radius) p-1 hover:bg-list-hover"
        >
          <Icon name="chevron-down" size={14} />
        </Button>
      </View>
      <If when={open()}>
        <Overlay
          anchor={anchor}
          placement="bottom"
          align="start"
          offset={4}
          margin={8}
          onPressOutside={close}
          onCancel={close}
          testID={props.testID ? `${props.testID}-list` : undefined}
          class="rounded-(--radius-lg) border border-border bg-popover p-1 shadow-lg"
          style={{ width: width() }}
        >
          <Scroll style={{ maxHeight: 240 }}>
            <If when={props.state === "loading"}>
              <Text class={`${FS.sm} px-2 py-1.5 text-muted-foreground`}>{props.loadingText}</Text>
            </If>
            <If when={props.state === "error"}>
              <ErrorText size="xs" class="px-2 py-1.5" text={props.error ?? ""} />
            </If>
            <If when={props.state === "ok" && shown().length === 0}>
              <Text class={`${FS.sm} px-2 py-1.5 text-muted-foreground`}>{props.emptyText}</Text>
            </If>
            <Each of={shown()}>
              {(name, i) => (
                <View
                  onPress={() => choose(name())}
                  class="h-7 flex-row items-center rounded-(--radius) px-2 hover:bg-accent"
                  classList={{ "bg-accent": i() === highlight() }}
                >
                  <Text class={`${FS.sm} flex-1 text-popover-foreground line-clamp-1`}>{name()}</Text>
                  <If when={name() === props.value}>
                    <Icon name="check" size={13} color="data" />
                  </If>
                </View>
              )}
            </Each>
          </Scroll>
        </Overlay>
      </If>
    </View>
  );
}
