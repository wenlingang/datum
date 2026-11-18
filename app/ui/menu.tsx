// 弹出菜单(下拉 / 右键)的 Datum 外观:在 nsh/base 的 Menu、ContextMenu、MenuItem 之上套样式。
// 对应 GPUI 版 gpui-component 的 PopupMenu。菜单项在每次打开时才构建,可在里面读当前数据。

import { Text, View, type JSX } from "nsh";
import { ContextMenu, Menu, MenuItem, MenuSeparator, Submenu, type Align, type Placement } from "nsh/base";
import { Icon } from "./kit";
import { FS } from "./text";

const POPUP = "min-w-44 rounded-(--radius-lg) border border-border bg-popover p-1 shadow-lg";

/** 下拉菜单:点 `trigger` 打开。 */
export function DropMenu(props: {
  trigger: JSX.Element;
  children?: JSX.Element;
  placement?: Placement;
  align?: Align;
  testID?: string;
  /** 打开 / 关闭(打开时再去加载菜单内容) */
  onOpenChange?: (open: boolean) => void;
  /** 弹层额外的类(如限制高度) */
  popupClass?: string;
}) {
  return (
    <Menu
      trigger={props.trigger}
      onOpenChange={(e) => props.onOpenChange?.(e.open)}
      placement={props.placement}
      align={props.align}
      offset={4}
      testID={props.testID}
      popupClass={`${POPUP} ${props.popupClass ?? ""}`}
    >
      {props.children}
    </Menu>
  );
}

/** 右键菜单:在 `trigger` 上点右键打开。 */
export function CtxMenu(props: { trigger: JSX.Element; children?: JSX.Element; disabled?: boolean; class?: string }) {
  return (
    <ContextMenu trigger={props.trigger} disabled={props.disabled} class={props.class} popupClass={POPUP}>
      {props.children}
    </ContextMenu>
  );
}

/** 菜单项。`danger` 为破坏性操作(红字)。
 *  选中后的动作推迟到菜单收起之后执行:动作可能卸载菜单所在的视图(关闭 tab、删除连接),
 *  菜单还在收尾时卸载它的宿主会让 nsh-base 的菜单读到已释放的信号。 */
export function MItem(props: { label: string; onSelect?: () => void; disabled?: boolean; danger?: boolean }) {
  return (
    <MenuItem
      onSelect={() => setTimeout(() => props.onSelect?.(), 0)}
      disabled={props.disabled}
      accessibilityLabel={props.label}
      class="h-7 flex-row items-center rounded-(--radius) px-2 hover:bg-accent focus:bg-accent active:bg-secondary-active disabled:opacity-40"
    >
      <Text class={`${FS.sm} ${props.danger ? "text-destructive" : "text-popover-foreground"}`}>{props.label}</Text>
    </MenuItem>
  );
}

/** 可勾选的菜单项:左侧对勾表示开启(MenuItem 的 checked + indicator,读屏为 menuitemcheckbox)。 */
export function MCheck(props: { label: string; checked: boolean; onSelect: () => void; disabled?: boolean }) {
  return (
    <MenuItem
      checked={props.checked}
      onSelect={() => setTimeout(() => props.onSelect(), 0)}
      disabled={props.disabled}
      accessibilityLabel={props.label}
      class="h-7 flex-row items-center gap-1.5 rounded-(--radius) px-2 hover:bg-accent focus:bg-accent active:bg-secondary-active disabled:opacity-40"
      indicatorClass="size-3.5 items-center justify-center opacity-0 checked:opacity-100"
      indicator={<Icon name="check" size={13} color="popover-foreground" />}
    >
      <Text class={`${FS.sm} text-popover-foreground`}>{props.label}</Text>
    </MenuItem>
  );
}

/** 二级菜单:触发项外观同 MItem、右侧带箭头;指针移上去或按 → 展开,子项是 `children`。 */
export function MSub(props: { label: string; children?: JSX.Element; disabled?: boolean; testID?: string }) {
  return (
    // Submenu 自己就是这一行条目(悬停 / 键盘聚焦 / 展开的样式写在它上面);trigger 只放内容,
    // 不能再套 MenuItem——按下 MenuItem 会当作「选中」把整个菜单关掉
    <Submenu
      disabled={props.disabled}
      offset={4}
      testID={props.testID}
      class="h-7 rounded-(--radius) px-2 hover:bg-accent focus:bg-accent aria-expanded:bg-accent disabled:opacity-40"
      trigger={
        <View class="h-full flex-row items-center gap-2">
          <Text class={`${FS.sm} flex-1 text-popover-foreground`}>{props.label}</Text>
          <Icon name="chevron-right" size={13} color="muted-foreground" />
        </View>
      }
    >
      {props.children}
    </Submenu>
  );
}

/** 菜单分隔线。 */
export function MSep() {
  return <MenuSeparator class="mx-1 my-1 h-px bg-border" />;
}
