// 报错文本:可以鼠标选中(⌘C / 右键复制),旁边一个复制按钮一键复制全文。
// 所有给用户看的错误都用它,方便贴给别人或交给 AI。

import { Clipboard, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";
import { IconButton } from "./kit";
import { FS, MONO } from "./text";

/** 复制按钮:点了把文本放进剪贴板,图标短暂变成对勾。 */
export function CopyButton(props: { text: string; size?: number; testID?: string }) {
  const done = signal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  async function copy() {
    await Clipboard.writeText(props.text);
    done.set(true);
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => done.set(false), 1500);
  }
  return (
    <View testID={props.testID}>
      <IconButton
        icon={done() ? "check" : "copy"}
        size={props.size ?? 13}
        color={done() ? "success" : "muted-foreground"}
        tooltip={done() ? t("error_text.copied") : t("error_text.copy")}
        class="p-1"
        onPress={() => void copy()}
      />
    </View>
  );
}

/** 一段报错:可选中 + 复制按钮。`mono` 用等宽字体(数据库原文),`size` 为字号档。 */
export function ErrorText(props: {
  text: string;
  mono?: boolean;
  size?: "xs" | "sm";
  class?: string;
  /** 文字颜色(主题 class),默认 text-negative */
  tone?: string;
  testID?: string;
}) {
  return (
    <View selectable class={`flex-row items-start gap-1 ${props.class ?? ""}`}>
      <Text
        testID={props.testID}
        class={`${FS[props.size ?? "sm"]} ${props.mono ? MONO : ""} ${props.tone ?? "text-negative"} min-w-0 flex-1`}
      >
        {props.text}
      </Text>
      <CopyButton text={props.text} testID={props.testID ? `${props.testID}-copy` : undefined} />
    </View>
  );
}
