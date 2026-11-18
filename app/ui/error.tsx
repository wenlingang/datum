// 报错文本:可以鼠标选中(⌘C / 右键复制),旁边一个复制按钮一键复制全文。
// 所有给用户看的错误都用它(块级用 ErrorBox,行内一句用 ErrorText),方便贴给别人或交给 AI。

import { Clipboard, Scroll, signal, Text, View, type JSX } from "nsh";
import { t } from "nsh/i18n";
import { Icon, IconButton } from "./kit";
import { FS, MONO } from "./text";

/** 复制按钮:点了把文本放进剪贴板,图标短暂变成对勾。 */
export function CopyButton(props: { text: string; testID?: string }) {
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
        small
        icon={done() ? "check" : "copy"}
        color={done() ? "success" : "muted-foreground"}
        tooltip={done() ? t("error_text.copied") : t("error_text.copy")}
        onPress={() => void copy()}
      />
    </View>
  );
}

/** 报错框:淡红边框(/30)与底色(/5)、左侧 circle-alert、文字可选中、右侧复制按钮,超出 `maxHeight`
 *  (默认 132)滚动。块级错误(测试连接失败、AI 回复出错、改表失败)用它;行内一句话的报错用 ErrorText。
 *  `title` 为加粗的首行,`children` 接在文字下面(如违规行列表),`action` 放在复制按钮左边(如重试)。 */
export function ErrorBox(props: {
  text: string;
  title?: string;
  mono?: boolean;
  maxHeight?: number;
  class?: string;
  testID?: string;
  action?: JSX.Element;
  children?: JSX.Element;
}) {
  return (
    <View
      testID={props.testID}
      class={`flex-row items-start gap-2 rounded-(--radius) border border-destructive/30 bg-destructive/5 py-1.5 pl-3 pr-1 ${props.class ?? ""}`}
    >
      <Icon name="circle-alert" size={14} color="negative" class="mt-1 shrink-0" />
      <Scroll class="min-w-0 flex-1 py-0.5" style={{ maxHeight: props.maxHeight ?? 132 }}>
        <View selectable class="gap-1">
          <If when={props.title}>{(h) => <Text class={`${FS.xs} font-bold text-negative`}>{h()}</Text>}</If>
          <If when={props.text}>
            <Text class={`${FS.xs} ${props.mono ? MONO : ""} text-negative`}>{props.text}</Text>
          </If>
          {props.children}
        </View>
      </Scroll>
      {props.action}
      <CopyButton text={props.title ? `${props.title}\n${props.text}` : props.text} />
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
