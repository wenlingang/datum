// tab/ 自用的 Datum 外观小件(颜色只用主题 token)。通用的在 ui/kit.tsx。

import { Text } from "nsh";
import { Button, Menu, MenuItem } from "nsh/base";
import { Icon } from "../ui/kit";
import { FS } from "../ui/text";

/** 下拉菜单弹层的统一外观。 */
export const MENU_POPUP = "min-w-[140px] rounded-(--radius-lg) border border-border bg-popover p-1 shadow-lg";
export const MENU_ITEM =
  "flex-row items-center rounded-(--radius) px-2 py-1.5 hover:bg-list-hover focus:bg-list-hover";

/** 带下拉箭头的描边选择按钮 + 菜单(筛选条的列 / 操作符)。 */
export function SelectMenu<T extends string>(props: {
  label: string;
  options: { value: T; label: string }[];
  onSelect: (value: T) => void;
  width: number;
  testID?: string;
}) {
  return (
    <Menu
      placement="bottom"
      align="start"
      offset={4}
      popupClass={`${MENU_POPUP} max-h-[320px]`}
      trigger={
        <Button
          testID={props.testID}
          class="h-7 flex-row items-center gap-1.5 rounded-(--radius) border border-input bg-background px-2.5 hover:bg-list-hover"
          style={{ width: props.width }}
        >
          <Text class={`${FS.sm} flex-1 text-foreground line-clamp-1`}>{props.label}</Text>
          <Icon name="chevron-down" size={13} />
        </Button>
      }
    >
      <Each of={props.options} key={(o) => o.value}>
        {(o) => (
          <MenuItem class={MENU_ITEM} onSelect={() => props.onSelect(o().value)}>
            <Text class={`${FS.sm} text-popover-foreground`}>{o().label}</Text>
          </MenuItem>
        )}
      </Each>
    </Menu>
  );
}
