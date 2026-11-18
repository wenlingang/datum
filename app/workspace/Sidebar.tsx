// 左侧栏:连接图标竖轨 + 表栏(连接头 / 库切换 / 搜索 / 表列表)。对应 GPUI 版 workspace/sidebar.rs。

import { effect, List, Scroll, signal, Text, View } from "nsh";
import { Button } from "nsh/base";
import { t } from "nsh/i18n";
import { openConnectionForm, openImport, openImportData, openPrivacy } from "../dialogs";
import { errorText } from "../lib/native";
import { type Connection, type DbKind, supportsSql } from "../lib/types";
import { activate, conns, loadDatabases, loadTables, refreshTables } from "../store/conns";
import { toast } from "../store/toast";
import { workspace } from "../store/workspace";
import { openDataDiff, openDiagnostics, openJournal, openRedisInsight, openRedisLive, openRedisLua, openSchemaDiff, openTable } from "../tab";
import { NewKeyDialog } from "../tab/RedisKeyEditor";
import { MiddleEllipsis } from "../ui/ellipsis";
import { color, EnvBadge, envLabel, envToken, Icon, IconButton, type IconName, SearchField, tip } from "../ui/kit";
import { CtxMenu, DropMenu, MItem, MSep, MSub } from "../ui/menu";
import { FS, MONO } from "../ui/text";
import { openErdForActive } from "./commands";
import {
  confirmDeleteConnection,
  copyText,
  dropDbByName,
  editConnWithDb,
  EXPORT_FORMATS,
  exportDatabase,
  exportDbByName,
  exportTable,
  exportFormatName,
  pickDatabase,
  showAlterTable,
  showDdl,
  showTableDetail,
  showTestData,
} from "./ops";

const DB_ICON: Record<DbKind, IconName> = {
  pg: "db-pg",
  mysql: "db-mysql",
  redis: "db-redis",
  sqlite: "db-sqlite",
};

const OBJECT_ICON: Record<DbKind, IconName> = {
  pg: "table",
  mysql: "table",
  sqlite: "table",
  redis: "key-round",
};

/** Redis 的库是编号:显示成 db0、db1…… */
export function displayDb(kind: DbKind, database: string) {
  return kind === "redis" ? `db${database}` : database;
}

/** 点竖轨图标:聚焦该连接,首次点击懒加载表列表。 */
function selectConn(id: string) {
  activate(id);
  if (!conns.tables(id) && !conns.isLoading(id)) void loadTables(id);
}

/** 连接右键 / 「…」菜单。 */
function ConnMenuItems(props: { conn: Connection }) {
  const c = props.conn;
  return (
    <>
      <MItem label={t("sidebar.edit")} onSelect={() => openConnectionForm(c)} />
      <MItem label={t("sidebar.refresh_tables")} onSelect={() => void loadTables(c.id)} />
      <MItem label={t("mask.menu")} onSelect={() => openPrivacy(c.id)} />
      <If when={supportsSql(c.kind)}>
        <MItem label={t("sidebar.import_sql")} onSelect={() => openImport(c.id)} />
        <If when={c.kind === "sqlite"}>
          <MItem label={t("sidebar.import_data")} onSelect={() => openImportData(c.id)} />
        </If>
        <MItem label={t("sidebar.export_database")} onSelect={() => void exportDatabase(c.id)} />
        <MItem label={t("sidebar.schema_diff")} onSelect={() => openSchemaDiff(c.id)} />
        <MItem label={t("sidebar.data_diff")} onSelect={() => openDataDiff(c.id)} />
      </If>
      <MItem label={t("journal.menu")} onSelect={() => openJournal(c.id)} />
      <MSep />
      <MItem label={t("sidebar.delete")} danger onSelect={() => void confirmDeleteConnection(c)} />
    </>
  );
}

/** 表 / key / 集合右键菜单。 */
function TableMenuItems(props: { connId: string; kind: DbKind; table: string }) {
  // 菜单项在每次打开时构建,这里取打开那一刻的值
  const connId = props.connId;
  const kind = props.kind;
  const table = props.table;
  if (kind === "redis") {
    return <MItem label={t("sidebar.copy_key_name")} onSelect={() => void copyText(table)} />;
  }
  return (
    <>
      <MItem label={t("sidebar.view_table_detail")} onSelect={() => showTableDetail(connId, table)} />
      <MItem label={t("workspace.copy_table_name")} onSelect={() => void copyText(table)} />
      <MItem label={t("sidebar.view_ddl")} onSelect={() => void showDdl(connId, table)} />
      <MItem label={t("sidebar.data_diff_table")} onSelect={() => openDataDiff(connId, table)} />
      <If when={kind === "sqlite"}>
        <MItem label={t("sidebar.alter_table")} onSelect={() => showAlterTable(connId, table)} />
      </If>
      <MItem label={t("testdata.menu")} onSelect={() => showTestData(connId, table)} />
      <MSep />
      <MSub label={t("sidebar.export_table")} testID="export-table-menu">
        {EXPORT_FORMATS.map((f) => (
          <MItem label={exportFormatName(f)} onSelect={() => void exportTable(connId, table, f)} />
        ))}
      </MSub>
    </>
  );
}

/** 库列表右键:编辑连接 / 导出 / DROP(Redis 库是固定编号,仅编辑)。 */
function DbMenuItems(props: { kind: DbKind; db: string }) {
  return (
    <>
      <MItem label={t("sidebar.edit_connection")} onSelect={() => editConnWithDb(props.db)} />
      <If when={supportsSql(props.kind)}>
        <MItem label={t("sidebar.export_database")} onSelect={() => exportDbByName(props.db)} />
        <MSep />
        <MItem label={t("sidebar.drop_database")} danger onSelect={() => void dropDbByName(props.db)} />
      </If>
    </>
  );
}

/** 连接竖轨:每个连接一个中性方块 + 放大的数据库标志(靠标志轮廓认类型,不按库上色),
 *  右下角环境色点;跟在后面的「+」新建连接。
 *  连接多了只滚图标列表,「+」在滚动区外面,不会被滚走。 */
function Rail() {
  return (
    <View class="w-12 shrink-0 items-center bg-rail" testID="conn-rail">
      <Scroll class="min-h-0 shrink self-stretch" contentClass="items-center gap-1.5 pt-2">
        <Each of={conns.list()} key={(c) => c.id}>
          {(c) => {
            const active = () => conns.activeId() === c().id;
            return (
              <CtxMenu trigger={
                // 未选中:中性底 + 前景色标志;当前连接:整块主色、白色标志,左边缘一条品牌渐变竖条。
                // 未选中才挂 hover(nsh 里 hover 优先级高于无前缀的底色,否则会盖掉主色)
                <View class="relative w-12 shrink-0 items-center">
                  <If when={active()}>
                    <View class="absolute left-0 top-1/2 -mt-2.5 h-5 w-[3px] rounded-r-full bg-linear-to-b from-primary to-ai" />
                  </If>
                  <Button
                    onPress={() => selectConn(c().id)}
                    selected={active()}
                    testID={`rail-${c().id}`}
                    accessibilityLabel={c().name}
                    tooltip={tip(() => `${c().name} · ${envLabel(c().env).toLowerCase()}`)}
                    class="relative size-[34px] items-center justify-center rounded-[9px]"
                    classList={{ "bg-primary": active(), "bg-raised hover:bg-list-hover": !active() }}
                  >
                    <Icon name={DB_ICON[c().kind]} size={18} color={active() ? "primary-foreground" : "foreground"} />
                    <View
                      class="pointer-events-none absolute -bottom-[3px] -right-[3px] size-[10px] rounded-full border-2 border-rail"
                      style={{ backgroundColor: color(envToken(c().env)) }}
                    />
                  </Button>
                </View>
              }>
                <ConnMenuItems conn={c()} />
              </CtxMenu>
            );
          }}
        </Each>
      </Scroll>
      <If when={conns.list().length > 0}>
        <View class="my-2.5 h-px w-5 shrink-0 bg-hairline" />
      </If>
      <View class="shrink-0 pb-2">
        <IconButton
          icon="plus"
          size={16}
          tooltip={t("sidebar.new_connection")}
          label={t("sidebar.new_connection")}
          onPress={() => openConnectionForm()}
          class="p-2"
        />
      </View>
    </View>
  );
}

/** 当前连接的表栏。 */
function TablesPane(props: { conn: Connection; search: string; onSearch: (v: string) => void; dbOpen: boolean; onDbOpen: (v: boolean) => void }) {
  const conn = () => props.conn;
  const tables = () => conns.tables(conn().id);
  const activeTable = () => {
    const tab = workspace.activeTab();
    return tab && tab.connId === conn().id ? tab.table : null;
  };
  const filtered = () => {
    const q = props.search.trim().toLowerCase();
    const list = tables() ?? [];
    return q ? list.filter((x) => x.toLowerCase().includes(q)) : list;
  };
  const dbLoading = signal(false);
  const newKeyOpen = signal(false);

  const toggleDbList = async () => {
    if (props.dbOpen) {
      props.onDbOpen(false);
      return;
    }
    props.onDbOpen(true);
    dbLoading.set(true);
    const id = conn().id;
    try {
      await loadDatabases(id);
    } catch (e) {
      if (conn().id === id) props.onDbOpen(false);
      toast.error(t("sidebar.load_dbs_failed", { error: errorText(e) }));
    } finally {
      dbLoading.set(false);
    }
  };

  return (
    <View class="min-w-0 flex-1">
      {/* 连接头:名称 / 环境 / 关系图 / … */}
      <View class="h-11 shrink-0 flex-row items-center gap-2 pl-3 pr-2">
        <MiddleEllipsis text={conn().name} class={`${FS.sm} font-bold text-foreground`} />
        <EnvBadge env={conn().env} />
        {/* 操作按钮:小号(24px)、间距 2px,整组不压缩;以前 27px + 8px 间距,Redis 有 5 个按钮时把连接名挤成「c…」 */}
        <View class="shrink-0 flex-row items-center gap-0.5">
        <If when={supportsSql(conn().kind)}>
          <IconButton small icon="network" tooltip={t("workspace.relationship_diagram")} onPress={openErdForActive} />
          <IconButton
            small
            icon="stethoscope"
            tooltip={t("workspace.diagnostics")}
            onPress={() => openDiagnostics(conn().id)}
          />
        </If>
        <If when={conn().kind === "redis"}>
          <IconButton
            small
            icon="plus"
            label={t("rkey.new_key")}
            tooltip={t("rkey.new_key")}
            onPress={() => newKeyOpen.set(true)}
          />
          <IconButton
            small
            icon="activity"
            tooltip={t("workspace.redis_insight")}
            onPress={() => openRedisInsight(conn().id)}
          />
          <IconButton
            small
            icon="radio"
            tooltip={t("workspace.redis_live")}
            onPress={() => openRedisLive(conn().id)}
          />
          <IconButton
            small
            icon="code-xml"
            tooltip={t("workspace.redis_lua")}
            onPress={() => openRedisLua(conn().id)}
          />
          <NewKeyDialog
            connId={conn().id}
            open={newKeyOpen()}
            onClose={() => newKeyOpen.set(false)}
            onCreated={(key) => {
              const id = conn().id;
              newKeyOpen.set(false);
              void refreshTables(id);
              openTable(id, key);
            }}
          />
        </If>
        <DropMenu
          testID="conn-actions"
          trigger={<IconButton small icon="ellipsis" tooltip={t("sidebar.conn_actions")} />}
        >
          <ConnMenuItems conn={conn()} />
        </DropMenu>
        </View>
      </View>

      {/* 库切换(SQLite 没有库的概念) */}
      <If when={conn().kind !== "sqlite"}>
        <View class="shrink-0 gap-1.5 px-2 pb-1.5">
          <Button
            onPress={() => void toggleDbList()}
            testID="db-switcher"
            class="h-[30px] flex-row items-center gap-2 rounded-(--radius) border bg-raised pl-2.5 pr-2"
            classList={{ "border-primary": props.dbOpen, "border-hairline": !props.dbOpen }}
          >
            <Icon name={DB_ICON[conn().kind]} size={13} color="data" />
            <MiddleEllipsis text={displayDb(conn().kind, conn().database)} class={`${FS.xs} ${MONO} font-semibold text-foreground`} />
            <If when={tables()}>
              {(list) => <Text class={`${FS["2xs"]} ${MONO} font-bold text-fg-faint`}>{list().length}</Text>}
            </If>
            <Icon name={props.dbOpen ? "chevron-up" : "chevron-down"} size={12} />
          </Button>
          <If when={props.dbOpen}>
            <DbList conn={conn()} loading={dbLoading()} onPicked={() => props.onDbOpen(false)} />
          </If>
        </View>
      </If>

      {/* 搜索 */}
      <View class="shrink-0 px-2 pb-2">
        <SearchField
          value={props.search}
          onChange={props.onSearch}
          placeholder={t("sidebar.search_tables")}
          testID="table-search"
          clearable
        />
      </View>

      {/* 表列表 */}
      <View class="min-h-0 flex-1" classList={{ "opacity-40": props.dbOpen }}>
        <If when={tables() === undefined}>
          <If
            when={conns.isLoading(conn().id)}
            else={
              <View class="flex-row items-center gap-2 px-5 py-2">
                <Text class={`${FS.xs} text-fg-faint`}>{t("sidebar.not_loaded")}</Text>
                <Button
                  onPress={() => void loadTables(conn().id)}
                  class="h-6 rounded-(--radius) px-2 hover:bg-list-hover active:bg-secondary-active"
                >
                  <Text class={`${FS.xs} text-foreground`}>{t("sidebar.retry")}</Text>
                </Button>
              </View>
            }
          >
            <Text class={`${FS.xs} px-5 py-2 text-fg-faint`}>{t("sidebar.loading")}</Text>
          </If>
        </If>
        <List
          each={filtered()}
          keyExtractor={(name) => name}
          estimatedItemSize={30}
          class="flex-1"
          contentClass="px-2 pb-1.5"
          testID="table-list"
        >
          {(name) => {
            const active = () => activeTable() === name();
            return (
              <CtxMenu trigger={
                <Button
                  onPress={() => openTable(conn().id, name())}
                  selected={active()}
                  accessibilityLabel={name()}
                  class="relative h-[30px] flex-row items-center gap-2 rounded-(--radius) pl-3 pr-2.5"
                  classList={{ "bg-sidebar-accent": active(), "hover:bg-list-hover": !active() }}
                >
                  <If when={active()}>
                    <View class="absolute bottom-[5px] left-0 top-[5px] w-[3px] rounded-r-[3px] bg-linear-to-b from-primary to-ai" />
                  </If>
                  <View style={{ opacity: active() ? 1 : 0.72 }}>
                    <Icon name={OBJECT_ICON[conn().kind]} size={13} color={active() ? "data" : "primary"} />
                  </View>
                  <MiddleEllipsis
                    text={name()}
                    class={`${FS.sm} font-medium ${active() ? "text-foreground" : "text-fg-soft"}`}
                  />
                </Button>
              }>
                <TableMenuItems connId={conn().id} kind={conn().kind} table={name()} />
              </CtxMenu>
            );
          }}
        </List>
      </View>
    </View>
  );
}

/** 展开的库列表:当前库打勾;点选切库,右键更多操作。 */
function DbList(props: { conn: Connection; loading: boolean; onPicked: () => void }) {
  const dbs = () => conns.databases(props.conn.id);
  return (
    <View class="max-h-[260px] rounded-(--radius-lg) border border-border bg-panel shadow-md">
      <Scroll class="shrink" contentClass="p-1" testID="db-list">
        <If
          when={dbs()}
          else={
            <View class="h-7 justify-center pl-7">
              <Text class={`${FS.xs} text-fg-faint`}>{t("sidebar.loading")}</Text>
            </View>
          }
        >
          {(list) => (
            <Each of={list()}>
              {(name) => {
                const current = () => name() === props.conn.database;
                return (
                  <CtxMenu trigger={
                    <Button
                      onPress={() => {
                        props.onPicked();
                        pickDatabase(name());
                      }}
                      accessibilityLabel={displayDb(props.conn.kind, name())}
                      class="h-7 flex-row items-center gap-2 rounded-(--radius) px-2 hover:bg-list-hover"
                      classList={{ "pl-7": !current() }}
                    >
                      <If when={current()}>
                        <Icon name="check" size={12} color="data" />
                      </If>
                      <MiddleEllipsis
                        text={displayDb(props.conn.kind, name())}
                        class={`${FS.xs} ${MONO} font-medium ${current() ? "text-foreground" : "text-fg-soft"}`}
                      />
                    </Button>
                  }>
                    <DbMenuItems kind={props.conn.kind} db={name()} />
                  </CtxMenu>
                );
              }}
            </Each>
          )}
        </If>
        <View class="mx-1.5 my-1 h-px bg-border" />
        <View class="min-h-[26px] justify-center py-1 pl-7 pr-2">
          <Text class={`${FS.xs} text-fg-faint`}>{t("sidebar.db_switch_hint")}</Text>
        </View>
      </Scroll>
    </View>
  );
}

/** 左侧栏。 */
export function Sidebar() {
  // 搜索与库列表按连接隔离:切换连接时清空 / 收起
  const search = signal("");
  const dbOpen = signal(false);
  effect(() => {
    conns.activeId();
    search.set("");
    dbOpen.set(false);
  });
  return (
    <View class="flex-1 flex-row bg-sidebar">
      <Rail />
      <View class="w-px bg-border" />
      <If
        when={conns.active()}
        else={
          <View class="flex-1 items-center justify-center px-4">
            <Text class={`${FS.xs} text-center text-fg-faint`}>
              {conns.list().length === 0 ? t("sidebar.empty_no_conns") : t("sidebar.empty_pick_conn")}
            </Text>
          </View>
        }
      >
        {(conn) => (
          <TablesPane
            conn={conn()}
            search={search()}
            onSearch={(v) => search.set(v)}
            dbOpen={dbOpen()}
            onDbOpen={(v) => dbOpen.set(v)}
          />
        )}
      </If>
    </View>
  );
}
