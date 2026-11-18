// Datum 的样式组件层:在 nsh/base 的行为之上套 Datum 的外观(颜色只用主题 token)。
// 页面只用这里的组件;缺什么在这里补,不要在页面里拼 View 冒充按钮/菜单。

import { Image, Text, token as nshToken, View, type JSX } from "nsh";
import { Button, Input, Toggle, Tooltip, type InputProps } from "nsh/base";
import { t } from "nsh/i18n";
import type { DbKind, Env } from "../lib/types";
import { ICONS, type IconName } from "./icons";
import { FS } from "./text";

export type { IconName };

/** 主题 token 的颜色值(读取处随主题/明暗变化)。 */
export function color(name: string): string {
  return nshToken(name as Parameters<typeof nshToken>[0]) as string;
}

/** 应用自带的 Lucide 图标;`color` 为主题 token 名(默认随文字色 muted-foreground)。
 *  `spin` 让图标一直转(加载中);`loader-circle` 默认就转。 */
export function Icon(props: { name: IconName; size?: number; color?: string; class?: string; spin?: boolean }) {
  const size = () => props.size ?? 16;
  const spin = () => props.spin ?? props.name === "loader-circle";
  return (
    <Image
      src={ICONS[props.name]}
      class={`${spin() ? "animate-spin" : ""} ${props.class ?? ""}`}
      style={{ width: size(), height: size(), tintColor: color(props.color ?? "muted-foreground") }}
    />
  );
}

const DB_ICON: Record<DbKind, IconName> = {
  pg: "db-pg",
  mysql: "db-mysql",
  redis: "db-redis",
  sqlite: "db-sqlite",
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

/** 图标按钮(工具条、顶栏)。`active` 为按下/开启态:只有未按下时挂悬停底色,免得悬停盖掉按下的底色。
 *  `small`:固定 24×24、图标 13,和 h-6 的控件(小号 Btn、ToggleBtn)同排或塞进列表行。 */
export function IconButton(props: {
  icon: IconName;
  onPress?: () => void;
  tooltip?: string;
  active?: boolean;
  disabled?: boolean;
  small?: boolean;
  size?: number;
  color?: string;
  class?: string;
  label?: string;
  testID?: string;
}) {
  return (
    <Button
      onPress={props.onPress}
      disabled={props.disabled}
      testID={props.testID}
      accessibilityLabel={props.label ?? props.tooltip ?? props.icon}
      tooltip={props.tooltip ? tip(props.tooltip) : undefined}
      class={`flex-row items-center justify-center rounded-(--radius) ${props.small ? "size-6 shrink-0" : "p-1.5"} active:bg-secondary-active disabled:opacity-40 ${props.class ?? ""}`}
      classList={{ "bg-secondary": !!props.active, "hover:bg-list-hover": !props.active }}
    >
      <Icon name={props.icon} size={props.size ?? (props.small ? 13 : 15)} color={props.color ?? (props.active ? "foreground" : "muted-foreground")} />
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

/** 文字按钮,可带前置图标与快捷键提示(`loader-circle` 图标会转,见 Icon)。
 *  尺寸两档:默认 h-8、`small` h-6。`loading`:前置图标换成转圈并禁用(测试连接、获取模型、开始导入)。 */
export function Btn(props: {
  label: string;
  onPress?: () => void;
  variant?: ButtonVariant;
  icon?: IconName;
  kbd?: string;
  small?: boolean;
  loading?: boolean;
  disabled?: boolean;
  tooltip?: string;
  class?: string;
  testID?: string;
}) {
  const v = () => VARIANT[props.variant ?? "secondary"];
  return (
    <Button
      onPress={props.onPress}
      disabled={props.disabled || props.loading}
      testID={props.testID}
      tooltip={props.tooltip ? tip(props.tooltip) : undefined}
      class={`flex-row items-center justify-center gap-1.5 rounded-(--radius) disabled:opacity-40 ${props.small ? "h-6 px-2" : "h-8 px-3"} ${v().box} ${props.class ?? ""}`}
    >
      <If when={props.loading ? ("loader-circle" as IconName) : props.icon}>
        {(name) => <Icon name={name()} size={props.small ? 13 : 15} color={v().icon} />}
      </If>
      <Text class={`${props.small ? FS.xs : FS.sm} font-medium ${v().text}`}>{props.label}</Text>
      <If when={props.kbd}>
        <Text class={`${FS.xs} ${v().text} opacity-60`}>{props.kbd}</Text>
      </If>
    </Button>
  );
}

/** 开关按钮:外观同小号 Btn(按下 secondary、未按下 outline),可带前置图标。
 *  按下样式用 `aria-pressed:`(优先级高于 hover:),悬停盖不掉。 */
export function ToggleBtn(props: {
  label: string;
  pressed: boolean;
  onPress: () => void;
  tooltip?: string;
  testID?: string;
  icon?: IconName;
}) {
  return (
    <Toggle
      testID={props.testID}
      pressed={props.pressed}
      onChange={() => props.onPress()}
      accessibilityLabel={props.label}
      tooltip={props.tooltip ? tip(props.tooltip) : undefined}
      class="group h-6 shrink-0 flex-row items-center justify-center gap-1.5 rounded-(--radius) border border-input px-2 hover:bg-list-hover active:bg-secondary-active disabled:opacity-40 aria-pressed:border-transparent aria-pressed:bg-secondary"
    >
      <If when={props.icon}>
        {(name) => <Icon name={name()} size={13} color={props.pressed ? "secondary-foreground" : "foreground"} />}
      </If>
      <Text class={`${FS.xs} font-medium text-foreground group-aria-pressed:text-secondary-foreground`}>{props.label}</Text>
    </Toggle>
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

/** 空状态。默认整页 / 整块级:居中的 32 号图标(fg-faint)+ 标题(FS.sm 中粗)+ 说明(Hint),
 *  `children` 放操作按钮。`compact`:列表里的小空态,只有一行居中小字(无图标)。
 *  `class` 替换默认的 `flex-1 p-6`(嵌在别的布局里时传 "")。`testID` 挂在标题文字上。 */
export function EmptyState(props: {
  title: string;
  icon?: IconName;
  /** 图标颜色 token(默认 fg-faint) */
  iconColor?: string;
  hint?: string;
  compact?: boolean;
  class?: string;
  testID?: string;
  children?: JSX.Element;
}) {
  return (
    <If
      when={!props.compact}
      else={
        <Text testID={props.testID} class={`${FS.xs} px-4 py-6 text-center text-fg-faint ${props.class ?? ""}`}>
          {props.title}
        </Text>
      }
    >
      <View class={`items-center justify-center gap-2 ${props.class ?? "flex-1 p-6"}`}>
        <If when={props.icon}>{(n) => <Icon name={n()} size={32} color={props.iconColor ?? "fg-faint"} />}</If>
        <Text testID={props.testID} class={`${FS.sm} text-center font-medium text-foreground`}>
          {props.title}
        </Text>
        <If when={props.hint}>{(h) => <Hint class="max-w-[460px] text-center">{h()}</Hint>}</If>
        {props.children}
      </View>
    </If>
  );
}

/** 加载中提示:转圈(loader-circle 14)+ 文字(FS.sm muted)。 */
export function Loading(props: { label: string; class?: string; testID?: string }) {
  return (
    <View testID={props.testID} class={`flex-row items-center gap-2 ${props.class ?? ""}`}>
      <Icon name="loader-circle" size={14} />
      <Text class={`${FS.sm} text-muted-foreground`}>{props.label}</Text>
    </View>
  );
}

/** 小号加粗分节标题(设置页、详情栏)。 */
export function SectionLabel(props: { children: JSX.Element; class?: string; testID?: string }) {
  return (
    <Text testID={props.testID} class={`${FS.xs} font-bold text-fg-faint ${props.class ?? ""}`}>
      {props.children}
    </Text>
  );
}

/** 快捷键提示(⌘↵ 等)。 */
export function Kbd(props: { children: string }) {
  return <Text class={`${FS.xs} text-fg-faint`}>{props.children}</Text>;
}

/** 列表/导航行:悬停高亮,选中态靠文字色与底色区分(字重不变)。 */
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
      // hover: 优先级高于 selected:,只给未选中项加悬停底色,免得悬停盖掉选中底色
      class={`h-[30px] flex-row items-center gap-2 rounded-(--radius) px-2.5 ${props.selected ? "bg-raised" : "hover:bg-list-hover"}`}
    >
      <If when={props.icon}>{(n) => <Icon name={n()} size={15} color={props.selected ? "foreground" : "muted-foreground"} />}</If>
      <Text
        class={`${FS.sm} flex-1 line-clamp-1 font-medium`}
        classList={{ "text-foreground": !!props.selected, "text-muted-foreground": !props.selected }}
      >
        {props.label}
      </Text>
      {props.trailing}
    </Button>
  );
}

/** 去掉若干键的 props 视图:其余键用 getter 转读原 props,保持响应式(不能用解构,解构会把值读死)。 */
function omitProps<T extends object, K extends keyof T>(props: T, keys: readonly K[]): Omit<T, K> {
  const out = {} as Omit<T, K>;
  for (const k of Object.keys(props) as (keyof T)[]) {
    if ((keys as readonly (keyof T)[]).includes(k)) continue;
    Object.defineProperty(out, k, { enumerable: true, get: () => props[k] });
  }
  return out;
}

/** 单行输入框(Datum 外观)。`mono` 等宽字体、`invalid` 红框由这里转成 class,其余属性透传给 nsh/base 的 Input。 */
export function TextField(props: InputProps & { mono?: boolean; invalid?: boolean }) {
  return (
    <Input
      {...omitProps(props, ["mono", "invalid"] as const)}
      class={`h-8 rounded-(--radius) border bg-background px-2.5 ${FS.sm} text-foreground placeholder:text-fg-faint focus:border-ring ${props.mono ? "font-mono" : ""} ${props.invalid ? "border-destructive" : "border-input"} ${props.class ?? ""}`}
    />
  );
}

/** 搜索 / 筛选框:左侧放大镜(13 号、fg-faint)+ 输入框。默认 h-7,`small` 为 h-6(紧凑工具条);
 *  `clearable` 有内容时右侧出一个清除按钮。宽度由 `class` 给(外层容器)。 */
export function SearchField(props: {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  small?: boolean;
  clearable?: boolean;
  class?: string;
  testID?: string;
}) {
  return (
    <View class={`relative justify-center ${props.class ?? ""}`}>
      <TextField
        testID={props.testID}
        value={props.value}
        placeholder={props.placeholder}
        onChange={({ value }) => props.onChange(value)}
        class={`${props.small ? "h-6" : "h-7"} pl-7 ${props.clearable ? "pr-7" : ""}`}
      />
      <View class="pointer-events-none absolute left-2.5">
        <Icon name="search" size={13} color="fg-faint" />
      </View>
      <If when={props.clearable && props.value}>
        <View class="absolute right-0.5">
          <IconButton
            small
            icon="x"
            label={t("search_field.clear")}
            testID={props.testID ? `${props.testID}-clear` : undefined}
            onPress={() => props.onChange("")}
          />
        </View>
      </If>
    </View>
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
