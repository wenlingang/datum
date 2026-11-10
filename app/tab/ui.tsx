// tab/ 自用的 Datum 外观小件(颜色只用主题 token)。通用的在 ui/kit.tsx、ui/menu.tsx。

import { Text } from "nsh";
import { Button } from "nsh/base";
import { Icon } from "../ui/kit";
import { DropMenu, MItem } from "../ui/menu";
import { FS } from "../ui/text";

/** 带下拉箭头的描边选择按钮 + 菜单(筛选条的列 / 操作符)。 */
export function SelectMenu<T extends string>(props: {
  label: string;
  options: { value: T; label: string }[];
  onSelect: (value: T) => void;
  width: number;
  testID?: string;
}) {
  return (
    <DropMenu
      placement="bottom"
      align="start"
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
        {(o) => <MItem label={o().label} onSelect={() => props.onSelect(o().value)} />}
      </Each>
    </DropMenu>
  );
}
