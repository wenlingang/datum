// 建表语句弹窗:只读高亮 + 复制。对应 GPUI 版 workspace/ops.rs 的 open_ddl_dialog。

import { View } from "nsh";
import { Editor } from "nsh/base";
import { t } from "nsh/i18n";
import { closeDdl, copyText, currentDdl } from "./ops";
import { DialogFrame } from "../dialogs/controls";
import { Btn } from "../ui/kit";

export function DdlDialog() {
  return (
    <DialogFrame
      open={!!currentDdl()}
      onClose={closeDdl}
      title={currentDdl() ? t("ops.ddl_title", { name: currentDdl()!.table }) : ""}
      width={640}
      testID="ddl-dialog"
    >
      <If when={currentDdl()}>
        {(view) => (
          <>
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
    </DialogFrame>
  );
}
