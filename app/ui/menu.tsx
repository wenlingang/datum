// 弹出菜单(下拉 / 右键)的 Datum 外观:在 nsh/base 的 Menu、ContextMenu、MenuItem 之上套样式。
// 对应 GPUI 版 gpui-component 的 PopupMenu。菜单项在每次打开时才构建,可在里面读当前数据。

import { Text, type JSX } from "nsh";
import { ContextMenu, Menu, MenuItem, MenuSeparator, type Align, type Placement } from "nsh/base";
import { FS } from "./text";

const POPUP = "min-w-44 rounded-(--radius-lg) border border-border bg-popover p-1 shadow-lg";

/** 下拉菜单:点 `trigger` 打开。 */
export function DropMenu(props: {
  trigger: JSX.Element;
  children?: JSX.Element;
  placement?: Placement;
  align?: Align;
  testID?: string;
}) {
  return (
    <Menu
      trigger={props.trigger}
      placement={props.placement}
      align={props.align}
      offset={4}
      testID={props.testID}
      popupClass={POPUP}
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

/** 菜单分隔线。 */
export function MSep() {
  return <MenuSeparator class="mx-1 my-1 h-px bg-border" />;
}
