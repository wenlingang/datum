// tab/ 自用的 Datum 外观小件(颜色只用主题 token)。通用的在 ui/kit.tsx、ui/menu.tsx。

import { Text } from "nsh";
import { Toggle, ToggleGroup } from "nsh/base";
import { Icon, type IconName } from "../ui/kit";
import { FS } from "../ui/text";

export interface ViewOption<T extends string> {
  value: T;
  label: string;
  icon?: IconName;
  testID?: string;
}

/** 视图切换(树 / 文本等):一组只能按下一个的小按钮。
 *  按下态用 `aria-pressed:`,优先级高于 `hover:`,悬停不会盖掉选中底色。 */
export function ViewSwitch<T extends string>(props: { options: ViewOption<T>[]; value: T; onChange: (v: T) => void }) {
  return (
    <ToggleGroup class="flex-row items-center gap-1">
      <Each of={props.options} key={(o) => o.value}>
        {(o) => {
          const on = () => props.value === o().value;
          return (
            <Toggle
              testID={o().testID}
              pressed={on()}
              onChange={() => props.onChange(o().value)}
              accessibilityLabel={o().label}
              class="h-6 flex-row items-center justify-center gap-1.5 rounded-(--radius) px-2 hover:bg-list-hover active:bg-secondary-active aria-pressed:bg-secondary"
            >
              <If when={o().icon}>
                {(name) => <Icon name={name()} size={13} color={on() ? "secondary-foreground" : "muted-foreground"} />}
              </If>
              <Text
                class={`${FS.xs} font-medium`}
                classList={{ "text-secondary-foreground": on(), "text-foreground": !on() }}
              >
                {o().label}
              </Text>
            </Toggle>
          );
        }}
      </Each>
    </ToggleGroup>
  );
}
