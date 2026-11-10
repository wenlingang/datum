// Datum 的样式组件层:在 nsh/base 的行为之上套 Datum 的外观(颜色只用主题 token)。
// 页面只用这里的组件;缺什么在这里补,不要在页面里拼 View 冒充按钮/菜单。

import { Image, Text, token as nshToken, View, type JSX } from "nsh";
import { Button, Input, Tooltip, type InputProps } from "nsh/base";
import type { DbKind, Env } from "../lib/types";
import { ICONS, type IconName } from "./icons";
import { FS } from "./text";

export type { IconName };

/** 主题 token 的颜色值(读取处随主题/明暗变化)。 */
export function color(name: string): string {
  return nshToken(name as Parameters<typeof nshToken>[0]) as string;
}

/** 应用自带的 Lucide 图标;`color` 为主题 token 名(默认随文字色 muted-foreground)。 */
export function Icon(props: { name: IconName; size?: number; color?: string; class?: string }) {
  const size = () => props.size ?? 16;
  return (
    <Image
      src={ICONS[props.name]}
      class={props.class}
      style={{ width: size(), height: size(), tintColor: color(props.color ?? "muted-foreground") }}
    />
  );
}

const DB_ICON: Record<DbKind, IconName> = {
  pg: "db-pg",
  mysql: "db-mysql",
  redis: "db-redis",
  sqlite: "db-sqlite",
  mongo: "db-mongo",
};

/** 数据库品牌图标(保留原色,不着色)。 */
export function DbIcon(props: { kind: DbKind; size?: number; class?: string }) {
  const size = () => props.size ?? 16;
  return <Image src={ICONS[DB_ICON[props.kind]]} class={props.class} style={{ width: size(), height: size() }} />;
}

/** 悬停提示。用法:`<View tooltip={tip("…")}>`。 */
export function tip(text: string | (() => string)) {
  return (
    <Tooltip class="rounded-(--radius) bg-popover border border-border px-2 py-1" offset={6}>
      <Text class={`${FS.xs} text-popover-foreground`}>{typeof text === "function" ? text() : text}</Text>
    </Tooltip>
  );
}

/** 图标按钮(工具条、顶栏)。`active` 为按下/开启态。 */
export function IconButton(props: {
  icon: IconName;
  onPress?: () => void;
  tooltip?: string;
  active?: boolean;
  disabled?: boolean;
  size?: number;
  color?: string;
  class?: string;
  label?: string;
}) {
  return (
    <Button
      onPress={props.onPress}
      disabled={props.disabled}
      accessibilityLabel={props.label ?? props.tooltip ?? props.icon}
      tooltip={props.tooltip ? tip(props.tooltip) : undefined}
      class={`flex-row items-center justify-center rounded-(--radius) p-1.5 hover:bg-list-hover active:bg-secondary-active disabled:opacity-40 ${props.class ?? ""}`}
      classList={{ "bg-secondary": !!props.active }}
    >
      <Icon name={props.icon} size={props.size ?? 15} color={props.color ?? (props.active ? "foreground" : "muted-foreground")} />
    </Button>
  );
}

export type ButtonVariant = "primary" | "secondary" | "ghost" | "outline" | "danger" | "ai";

const VARIANT: Record<ButtonVariant, { box: string; text: string; icon: string }> = {
  primary: {
    box: "bg-primary hover:bg-primary-hover active:bg-primary-active",
    text: "text-primary-foreground",
    icon: "primary-foreground",
  },
  secondary: {
    box: "bg-secondary hover:bg-secondary-hover active:bg-secondary-active",
    text: "text-secondary-foreground",
    icon: "secondary-foreground",
  },
  ghost: { box: "hover:bg-list-hover active:bg-secondary-active", text: "text-foreground", icon: "muted-foreground" },
  outline: {
    box: "border border-input hover:bg-list-hover active:bg-secondary-active",
    text: "text-foreground",
    icon: "foreground",
  },
  danger: {
    box: "bg-destructive hover:bg-destructive-hover active:opacity-90",
    text: "text-destructive-foreground",
    icon: "destructive-foreground",
  },
  ai: { box: "bg-ai hover:opacity-90 active:opacity-80", text: "text-on-ai", icon: "on-ai" },
};

/** 文字按钮,可带前置图标与快捷键提示。 */
export function Btn(props: {
  label: string;
  onPress?: () => void;
  variant?: ButtonVariant;
  icon?: IconName;
  kbd?: string;
  small?: boolean;
  disabled?: boolean;
  tooltip?: string;
  class?: string;
  testID?: string;
}) {
  const v = () => VARIANT[props.variant ?? "secondary"];
  return (
    <Button
      onPress={props.onPress}
      disabled={props.disabled}
      testID={props.testID}
      tooltip={props.tooltip ? tip(props.tooltip) : undefined}
      class={`flex-row items-center justify-center gap-1.5 rounded-(--radius) disabled:opacity-40 ${props.small ? "h-6 px-2" : "h-8 px-3"} ${v().box} ${props.class ?? ""}`}
    >
      <If when={props.icon}>{(name) => <Icon name={name()} size={props.small ? 13 : 15} color={v().icon} />}</If>
      <Text class={`${props.small ? FS.xs : FS.sm} font-medium ${v().text}`}>{props.label}</Text>
      <If when={props.kbd}>
        <Text class={`${FS.xs} ${v().text} opacity-60`}>{props.kbd}</Text>
      </If>
    </Button>
  );
}

const ENV_COLOR: Record<Env, string> = { local: "env-local", staging: "env-staging", prod: "env-prod" };

export const envToken = (env: Env) => ENV_COLOR[env];
export const envLabel = (env: Env) => env.toUpperCase();

/** 环境徽章:描边胶囊 + 大写环境名。 */
export function EnvBadge(props: { env: Env }) {
  return (
    <View
      class="shrink-0 rounded-full border px-1.5"
      style={{ borderColor: color(envToken(props.env)), paddingTop: 1, paddingBottom: 1 }}
    >
      <Text class={`${FS["2xs"]} font-bold`} style={{ color: color(envToken(props.env)) }}>
        {envLabel(props.env)}
      </Text>
    </View>
  );
}

/** 状态小圆点;`color` 为主题 token 名。 */
export function Dot(props: { color: string; size?: number }) {
  const s = () => props.size ?? 6;
  return <View class="shrink-0 rounded-full" style={{ width: s(), height: s(), backgroundColor: color(props.color) }} />;
}

/** 竖分隔线。 */
export function VSep() {
  return <View class="h-4 w-px shrink-0 bg-hairline" />;
}

/** 段落式提示文本(空态、说明)。 */
export function Hint(props: { children: JSX.Element; class?: string }) {
  return <Text class={`${FS.sm} text-muted-foreground ${props.class ?? ""}`}>{props.children}</Text>;
}

/** 小号加粗分节标题(设置页、详情栏)。 */
export function SectionLabel(props: { children: JSX.Element; class?: string }) {
  return <Text class={`${FS.xs} font-bold text-fg-faint ${props.class ?? ""}`}>{props.children}</Text>;
}

/** 快捷键提示(⌘↵ 等)。 */
export function Kbd(props: { children: string }) {
  return <Text class={`${FS.xs} text-fg-faint`}>{props.children}</Text>;
}

/** 列表/导航行:悬停高亮,选中态加粗加底色。 */
export function NavRow(props: {
  label: string;
  icon?: IconName;
  selected?: boolean;
  onPress?: () => void;
  trailing?: JSX.Element;
  testID?: string;
}) {
  return (
    <Button
      onPress={props.onPress}
      testID={props.testID}
      selected={props.selected}
      class="h-[30px] flex-row items-center gap-2 rounded-(--radius) px-2.5 hover:bg-list-hover selected:bg-raised"
    >
      <If when={props.icon}>{(n) => <Icon name={n()} size={15} color={props.selected ? "foreground" : "muted-foreground"} />}</If>
      <Text
        class={`${FS.sm} flex-1 line-clamp-1`}
        classList={{ "font-bold text-foreground": !!props.selected, "text-muted-foreground": !props.selected }}
      >
        {props.label}
      </Text>
      {props.trailing}
    </Button>
  );
}

/** 单行输入框(Datum 外观)。其余属性透传给 nsh/base 的 Input。 */
export function TextField(props: InputProps & { mono?: boolean; invalid?: boolean }) {
  return (
    <Input
      {...props}
      class={`h-8 rounded-(--radius) border bg-background px-2.5 ${FS.sm} text-foreground placeholder:text-fg-faint focus:border-ring ${props.mono ? "font-mono" : ""} ${props.invalid ? "border-destructive" : "border-input"} ${props.class ?? ""}`}
    />
  );
}

/** 品牌洗色:靛 → 紫 → 青 渐隐,铺在顶栏/状态栏底下(容器需 relative)。 */
export function BrandWash(props: { mirror?: boolean }) {
  return (
    <View
      class={`pointer-events-none absolute inset-0 ${props.mirror ? "bg-linear-to-l" : "bg-linear-to-r"} from-primary/10 via-ai/5 via-40% to-data/0 to-80%`}
    />
  );
}

/** 霓虹渐变发丝线:压在顶栏下缘(`top` 为 false)或状态栏上缘。 */
export function BrandLine(props: { mirror?: boolean; top?: boolean }) {
  return (
    <View
      class={`pointer-events-none absolute left-0 right-0 h-px ${props.top ? "top-0" : "bottom-0"} ${props.mirror ? "bg-linear-to-l" : "bg-linear-to-r"} from-primary/80 via-ai/70 via-40% to-data/0 to-90%`}
    />
  );
}
