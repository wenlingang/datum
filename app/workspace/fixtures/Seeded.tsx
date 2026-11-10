// 测试 / 截图用入口:聚焦第一个连接、预置两个 tab 后挂载工作区。
// 只给 workspace/*.test.tsx 用,不进正式包(App.tsx 不引用它)。
// 需要 DATUM_CONFIG_DIR 指向临时配置目录(至少一个 SQLite 连接)。

import { mount, signal, View } from "nsh";
import { DialogHost } from "../../dialogs";
import { activate, conns, loadTables, reloadConnections } from "../../store/conns";
import { apply as applyPrefs } from "../../store/prefs";
import { addTab, newTabId, setResultToolsProvider } from "../../store/workspace";
import { FS } from "../../ui/text";
import { Workspace } from "../Workspace";

applyPrefs();
reloadConnections();

const first = conns.list()[0];
if (first) {
  activate(first.id);
  void loadTables(first.id);
  addTab({ id: newTabId(), kind: "query", connId: first.id, dbKind: first.kind, table: null, title: signal("SQL Query"), state: null });
  addTab({ id: newTabId(), kind: "table", connId: first.id, dbKind: first.kind, table: "products", title: signal("products"), state: null });
  // 模拟 tab/ 提供的结果工具:表浏览 tab 有结果、已筛选 1 条
  setResultToolsProvider((tab) =>
    tab.kind === "table"
      ? {
          filterCount: 1,
          filterOpen: false,
          exportData: () => ({ table: "products", columns: ["id", "name"], rows: [["1", "Boquete"]] }),
        }
      : null,
  );
}

function Root() {
  return (
    <View class={`flex-1 bg-background ${FS.sm}`}>
      <Workspace />
      <DialogHost />
    </View>
  );
}

mount(Root);
