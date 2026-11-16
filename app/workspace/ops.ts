// 工作区的命令实现:导出、切库 / 删库、建表语句、删除连接、复制。
// 对应 GPUI 版 workspace/ops.rs。按钮、菜单、快捷键都调这里的函数,行为只实现一次。

import { Clipboard, signal } from "nsh";
import { AlertDialog } from "nsh/base";
import { t } from "nsh/i18n";
import { openConnectionForm } from "../dialogs";
import { db, errorText, type ExportFormat, journal, shell, sql } from "../lib/native";
import { type Connection, type DbKind, supportsSql } from "../lib/types";
import {
  activate,
  conns,
  deleteConnection,
  forgetDatabase,
  loadTables,
  siblings,
  switchDatabase,
} from "../store/conns";
import { privacy } from "../store/privacy";
import { toast } from "../store/toast";
import { closeConnTabs, setDetailOpen, workspace } from "../store/workspace";
import { openTable } from "../tab";

/** 导出格式,菜单顺序与 GPUI 版一致。 */
export const EXPORT_FORMATS: ExportFormat[] = ["csv", "xlsx", "json", "sql", "markdown"];

const SHORT: Record<ExportFormat, string> = { csv: "CSV", xlsx: "Excel", json: "JSON", sql: "SQL", markdown: "Markdown" };
const EXT: Record<ExportFormat, string> = { csv: "csv", xlsx: "xlsx", json: "json", sql: "sql", markdown: "md" };

/** 结果栏「导出」菜单项文案。 */
export const exportMenuLabel = (f: ExportFormat) => t("ops.export_as", { format: SHORT[f] });
/** 表右键「导出整表」菜单项文案。 */
export const exportTableLabel = (f: ExportFormat) => t("ops.export_table_as", { format: SHORT[f] });

/** 弹出保存对话框写入文本;成功 / 失败都给提示,取消不提示。 */
export async function saveText(fileName: string, text: string, success: string) {
  try {
    const path = await shell.saveText(fileName, text);
    if (path) toast.success(success);
  } catch (e) {
    toast.error(t("ops.write_failed", { error: errorText(e) }));
  }
}

/** 复制文本到剪贴板并提示。 */
export async function copyText(text: string) {
  try {
    await Clipboard.writeText(text);
    toast.success(t("workspace.copied"));
  } catch (e) {
    toast.error(errorText(e));
  }
}

/** 按格式渲染并弹出保存对话框;Excel 是二进制,由原生层直接写文件。 */
export async function saveRows(
  format: ExportFormat,
  table: string,
  columns: string[],
  rows: (string | null)[][],
  kind: DbKind,
) {
  const name = fileName(table, EXT[format]);
  const success = t("ops.exported", { format: SHORT[format] });
  if (format !== "xlsx") {
    await saveText(name, shell.renderExport(format, table, columns, rows, kind), success);
    return;
  }
  try {
    const path = await shell.saveXlsx(name, table, columns, rows);
    if (path) toast.success(success);
  } catch (e) {
    toast.error(t("ops.write_failed", { error: errorText(e) }));
  }
}

const fileName = (base: string, ext: string) => `${base}_${shell.fileTimestamp()}.${ext}`;

// ------------------------------------------------------------ 导出

/** 导出时遮罩敏感列(结果栏导出菜单里的勾选项,默认勾选;演示模式下强制)。 */
const exportMask = signal(true);
export const exportMasked = () => exportMask() || privacy.demo();
export const toggleExportMask = () => exportMask.update((v) => !v);

/** 导出当前结果(已应用筛选后的行);有敏感列且勾选了「遮罩敏感列」时导出遮罩后的值。 */
export async function exportFiltered(format: ExportFormat) {
  const tab = workspace.activeTab();
  const data = workspace.resultTools()?.exportData();
  if (!tab || !data || data.rows.length === 0) return;
  const rows = data.maskedRows && exportMasked() ? data.maskedRows() : data.rows;
  await saveRows(format, data.table, data.columns, rows, tab.dbKind);
}

/** 整表 / 整库导出是长任务,防重入。 */
let exportingDb = false;

/** 表右键「导出整表」:全部行(不受行数上限)。先选文件,再由原生层逐行写入,数据不经过界面。 */
export async function exportTable(connId: string, table: string, format: ExportFormat) {
  const conn = conns.get(connId);
  if (!conn || !supportsSql(conn.kind)) return;
  if (exportingDb) {
    toast.info(t("ops.export_busy"));
    return;
  }
  exportingDb = true;
  try {
    const done = await shell.exportTable(conn.kind, connId, table, format, fileName(table, EXT[format]));
    if (!done) return;
    if (done.rows === 0) toast.info(t("ops.table_empty"));
    else toast.success(t("ops.exported_rows", { format: SHORT[format], count: done.rows }));
  } catch (e) {
    toast.error(errorText(e));
  } finally {
    exportingDb = false;
  }
}

/** 导出整个数据库:逐表查询全部行,合成一个 SQL 文件。`cleanup`:完成后删除的临时克隆连接。 */
export async function exportDatabase(connId: string, cleanup?: string) {
  if (exportingDb) {
    toast.info(t("ops.export_busy"));
    return;
  }
  const conn = conns.get(connId);
  if (!conn) return;
  exportingDb = true;
  try {
    // 侧栏没展开过这个连接时表名还没加载,这里补一次
    let tables = conns.tables(connId) ?? [];
    if (tables.length === 0) {
      await db.connect(conn.kind, connId);
      tables = await db.listObjects(conn.kind, connId);
    }
    if (tables.length === 0) {
      toast.info(t("ops.no_tables"));
      return;
    }
    const n = tables.length;
    const path = await shell.exportDatabase(conn.kind, connId, tables, `${conn.name}_${shell.fileTimestamp()}.sql`);
    if (path) toast.success(t("ops.exported_db", { count: n }));
  } catch (e) {
    toast.error(errorText(e));
  } finally {
    exportingDb = false;
    if (cleanup) {
      try {
        deleteConnection(cleanup);
      } catch {
        // 临时连接删不掉只是多一条记录,不打扰用户
      }
    }
  }
}

// ------------------------------------------------------------ 库切换

/** 选库:同实例已有该库的连接则切换过去,否则克隆一个(密码一并复制)再切换。 */
export function pickDatabase(database: string) {
  const active = conns.active();
  if (!active || database === active.database) return;
  try {
    const next = switchDatabase(active.id, database);
    activate(next.id);
    void loadTables(next.id);
  } catch (e) {
    toast.error(t("ops.switch_db_failed", { error: errorText(e) }));
  }
}

/** 库列表右键「编辑连接」:打开连接表单,库名预填为右键的库。 */
export function editConnWithDb(database: string) {
  const active = conns.active();
  if (active) openConnectionForm(active, { database });
}

/** 导出右键的库:当前库直接导;其他库先拿到(复用或克隆的)连接,不切换侧栏焦点。
 *  若连接是为导出新克隆的,导出完成后删掉。 */
export function exportDbByName(database: string) {
  const active = conns.active();
  if (!active) return;
  if (database === active.database) {
    void exportDatabase(active.id);
    return;
  }
  const existed = siblings(active, database).length > 0;
  try {
    const next = switchDatabase(active.id, database);
    void exportDatabase(next.id, existed ? undefined : next.id);
  } catch (e) {
    toast.error(t("ops.prepare_export_failed", { error: errorText(e) }));
  }
}

/** DROP 右键的库:当前库拒绝;其余先确认,确认后断开指向目标库的残留连接再执行。 */
export async function dropDbByName(database: string) {
  const active = conns.active();
  if (!active) return;
  if (database === active.database) {
    toast.error(t("ops.cannot_drop_current"));
    return;
  }
  if (!supportsSql(active.kind)) return;
  const statement = `DROP DATABASE ${sql.ident(database, active.kind)}`;
  const { action } = await AlertDialog.show({
    title: t("risk.title"),
    description: `${t("risk.target_conn", { target: active.name, count: 1 })}\n\n${statement}\n\n${t("risk.irreversible")}`,
    actions: [
      { id: "cancel", label: t("risk.cancel"), role: "cancel" },
      { id: "confirm", label: t("risk.confirm"), role: "destructive" },
    ],
    closeOnEscape: true,
  });
  if (action !== "confirm") return;
  const stale = siblings(active, database);
  try {
    // 活跃会话会让 PG 拒绝 DROP:先强制断开
    for (const s of stale) await db.disconnect(s.kind, s.id).catch(() => {});
    await db.query(active.kind, active.id, statement, null);
  } catch (e) {
    toast.error(errorText(e));
    return;
  }
  for (const s of stale) {
    try {
      deleteConnection(s.id);
      closeConnTabs(s.id);
    } catch {
      // 残留连接删不掉不影响 DROP 的结果
    }
  }
  forgetDatabase(active.id, database);
  toast.success(t("ops.dropped_db", { name: database }));
}

// ------------------------------------------------------------ 表

/** 右键表「查看表详情」:聚焦(或新开)该表 tab 后展开右栏。 */
export function showTableDetail(connId: string, table: string) {
  if (openTable(connId, table)) setDetailOpen(true);
}

export interface DdlView {
  table: string;
  ddl: string;
}

const ddlView = signal<DdlView | null>(null);

/** 正在显示的建表语句(DdlDialog 读取)。 */
export const currentDdl = () => ddlView();
export const closeDdl = () => ddlView.set(null);

/** 查看建表语句。 */
export async function showDdl(connId: string, table: string) {
  const conn = conns.get(connId);
  if (!conn) return;
  try {
    const ddl = await db.createTableDdl(conn.kind, connId, table);
    ddlView.set({ table, ddl });
  } catch (e) {
    toast.error(errorText(e));
  }
}

// ------------------------------------------------------------ 连接

/** 删除连接:确认后删除并关闭它的全部 tab。 */
export async function confirmDeleteConnection(conn: Connection) {
  // 有写操作记录时问一句要不要一起删(默认保留:删掉的连接上做过什么仍可追溯)
  const hasJournal = await journal
    .list(conn.id, { limit: 1 })
    .then((l) => l.length > 0)
    .catch(() => false);
  const base = conn.env !== "local" ? t("ops.delete_conn_desc_keychain") : t("ops.delete_conn_desc");
  const { action } = await AlertDialog.show({
    title: t("ops.delete_conn_title", { name: conn.name }),
    description: hasJournal ? `${base}\n\n${t("journal.delete_conn_journal_note")}` : base,
    actions: hasJournal
      ? [
          { id: "cancel", label: t("ops.cancel"), role: "cancel" },
          { id: "delete", label: t("journal.delete_conn_keep") },
          { id: "delete-all", label: t("journal.delete_conn_with_journal"), role: "destructive" },
        ]
      : [
          { id: "cancel", label: t("ops.cancel"), role: "cancel" },
          { id: "delete", label: t("ops.delete"), role: "destructive" },
        ],
    closeOnEscape: true,
  });
  if (action !== "delete" && action !== "delete-all") return;
  try {
    deleteConnection(conn.id);
    closeConnTabs(conn.id);
    if (action === "delete-all") await journal.clear(conn.id);
  } catch (e) {
    toast.error(errorText(e));
  }
}

/** 正在修改结构的 SQLite 表(AlterTableDialog 读取)。 */
const alterView = signal<{ connId: string; table: string } | null>(null);
export const alterTarget = () => alterView();
export const closeAlterTable = () => alterView.set(null);

/** 右键 SQLite 表「修改表结构」。 */
export function showAlterTable(connId: string, table: string) {
  const conn = conns.get(connId);
  if (!conn || conn.kind !== "sqlite") return;
  alterView.set({ connId, table });
}
