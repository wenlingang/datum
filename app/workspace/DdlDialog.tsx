// 建表语句弹窗:只读高亮 + 复制。对应 GPUI 版 workspace/ops.rs 的 open_ddl_dialog。

import { Text, View } from "nsh";
import { Dialog, Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { Btn, IconButton } from "../ui/kit";
import { FS } from "../ui/text";
import { closeDdl, copyText, currentDdl } from "./ops";

export function DdlDialog() {
  return (
    <Dialog
      open={!!currentDdl()}
      onOpenChange={(e) => {
        if (!e.open) closeDdl();
      }}
      closeOnEscape
      closeOnBackdropPress
      animation="fade"
      testID="ddl-dialog"
      backdropClass="bg-overlay"
      class="m-auto w-[640px] max-w-[90%] gap-3 rounded-(--radius-lg) border border-border bg-popover p-4 shadow-lg"
    >
      <If when={currentDdl()}>
        {(view) => (
          <>
            <View class="flex-row items-center justify-between">
              <Text class={`${FS.base} font-semibold text-popover-foreground`}>
                {t("ops.ddl_title", { name: view().table })}
              </Text>
              <IconButton icon="x" size={14} label={t("workspace.close")} onPress={closeDdl} />
            </View>
            <View class="h-[380px] overflow-hidden rounded-(--radius) border border-border">
              <Editor value={view().ddl} language="sql" readOnly softWrap class="flex-1 bg-editor" />
            </View>
            <View class="flex-row justify-end gap-2">
              <Btn
                variant="primary"
                small
                icon="copy"
                label={t("ops.copy")}
                testID="ddl-copy"
                onPress={() => void copyText(view().ddl)}
              />
            </View>
          </>
        )}
      </If>
    </Dialog>
  );
}
