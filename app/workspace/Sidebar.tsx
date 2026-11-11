// 左侧栏:连接图标竖轨 + 表栏(连接头 / 库切换 / 搜索 / 表列表)。对应 GPUI 版 workspace/sidebar.rs。

import { effect, List, Scroll, signal, Text, View } from "nsh";
import { Button } from "nsh/base";
import { t } from "nsh/i18n";
import { openConnectionForm, openImport } from "../dialogs";
import { errorText } from "../lib/native";
import { type Connection, type DbKind, supportsSql } from "../lib/types";
import { activate, conns, loadDatabases, loadTables } from "../store/conns";
import { toast } from "../store/toast";
import { workspace } from "../store/workspace";
import { openTable } from "../tab";
import { MiddleEllipsis } from "../ui/ellipsis";
import { color, EnvBadge, envLabel, envToken, Icon, IconButton, type IconName, TextField, tip } from "../ui/kit";
import { CtxMenu, DropMenu, MItem, MSep } from "../ui/menu";
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
  exportTableLabel,
  pickDatabase,
  showDdl,
  showTableDetail,
} from "./ops";

const DB_ICON: Record<DbKind, IconName> = {
  pg: "db-pg",
  mysql: "db-mysql",
  redis: "db-redis",
  sqlite: "db-sqlite",
  mongo: "db-mongo",
};

const OBJECT_ICON: Record<DbKind, IconName> = {
  pg: "table",
  mysql: "table",
  sqlite: "table",
  redis: "key-round",
  mongo: "inbox",
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
      <If when={supportsSql(c.kind)}>
        <MItem label={t("sidebar.import_sql")} onSelect={() => openImport(c.id)} />
        <MItem label={t("sidebar.export_database")} onSelect={() => void exportDatabase(c.id)} />
      </If>
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
      <MSep />
      {EXPORT_FORMATS.map((f) => (
        <MItem label={exportTableLabel(f)} onSelect={() => void exportTable(connId, table, f)} />
      ))}
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

/** 连接竖轨:每个连接一个图标(右下角环境色点),底部「+」新建连接。 */
function Rail() {
  return (
    <Scroll class="w-12 shrink-0 bg-rail" contentClass="items-center gap-1.5 py-2" testID="conn-rail">
      <Each of={conns.list()} key={(c) => c.id}>
        {(c) => {
          const active = () => conns.activeId() === c().id;
          return (
            <CtxMenu trigger={
              <Button
                onPress={() => selectConn(c().id)}
                selected={active()}
                testID={`rail-${c().id}`}
                accessibilityLabel={c().name}
                tooltip={tip(() => `${c().name} · ${envLabel(c().env).toLowerCase()}`)}
                class="relative size-8 shrink-0 items-center justify-center rounded-lg"
                classList={{ "bg-primary": active(), "border border-hairline hover:bg-raised": !active() }}
              >
                <Icon name={DB_ICON[c().kind]} size={16} color={active() ? "primary-foreground" : "muted-foreground"} />
                <View
                  class="absolute -bottom-px -right-px size-[9px] rounded-full border-2 border-rail"
                  style={{ backgroundColor: color(envToken(c().env)) }}
                />
              </Button>
            }>
              <ConnMenuItems conn={c()} />
            </CtxMenu>
          );
        }}
      </Each>
      <If when={conns.list().length > 0}>
        <View class="my-1 h-px w-5 shrink-0 bg-hairline" />
      </If>
      <IconButton
        icon="plus"
        size={16}
        tooltip={t("sidebar.new_connection")}
        label={t("sidebar.new_connection")}
        onPress={() => openConnectionForm()}
        class="p-2"
      />
    </Scroll>
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
        <If when={supportsSql(conn().kind)}>
          <IconButton icon="network" tooltip={t("workspace.relationship_diagram")} onPress={openErdForActive} />
        </If>
        <DropMenu
          testID="conn-actions"
          trigger={<IconButton icon="ellipsis" size={13} tooltip={t("sidebar.conn_actions")} class="p-1" />}
        >
          <ConnMenuItems conn={conn()} />
        </DropMenu>
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
        <View class="relative justify-center">
          <TextField
            value={props.search}
            onChange={({ value }) => props.onSearch(value)}
            placeholder={t("sidebar.search_tables")}
            testID="table-search"
            class="h-7 pl-7"
          />
          <View class="pointer-events-none absolute left-2.5">
            <Icon name="search" size={13} color="fg-faint" />
          </View>
          <If when={props.search}>
            <View class="absolute right-1">
              <IconButton icon="x" size={12} class="p-1" label="clear" onPress={() => props.onSearch("")} />
            </View>
          </If>
        </View>
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
                  class="relative h-[30px] flex-row items-center gap-2 rounded pl-3 pr-2.5"
                  classList={{ "bg-sidebar-accent": active(), "hover:bg-list-hover": !active() }}
                >
                  <If when={active()}>
                    <View class="absolute bottom-[5px] left-0 top-[5px] w-[3px] rounded-r-[3px] bg-primary" />
                  </If>
                  <View style={{ opacity: active() ? 1 : 0.72 }}>
                    <Icon name={OBJECT_ICON[conn().kind]} size={13} color={active() ? "data" : "primary"} />
                  </View>
                  <MiddleEllipsis
                    text={name()}
                    class={`${FS.sm} ${active() ? "font-bold text-foreground" : "font-medium text-fg-soft"}`}
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
    <View class="max-h-[260px] rounded-lg border border-border bg-panel shadow-md">
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
                      class="h-7 flex-row items-center gap-2 rounded-[5px] px-2 hover:bg-list-hover"
                      classList={{ "pl-7": !current() }}
                    >
                      <If when={current()}>
                        <Icon name="check" size={12} color="data" />
                      </If>
                      <MiddleEllipsis
                        text={displayDb(props.conn.kind, name())}
                        class={`${FS.xs} ${MONO} ${current() ? "font-bold text-foreground" : "font-normal text-fg-soft"}`}
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
