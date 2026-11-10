// 连接:列表、当前聚焦连接、每个连接懒加载的对象列表 / 列信息 / 库列表缓存。
// 对应 GPUI 版 store.rs 的 ConnectionStore。

import { batch, signal } from "nsh";
import { t } from "nsh/i18n";
import { db, errorText, store } from "../lib/native";
import { type ColumnsByTable, type Connection, supportsSql } from "../lib/types";
import { toast } from "./toast";

const connections = signal<Connection[]>([]);
const activeId = signal<string | null>(null);
const tables = signal<Record<string, string[]>>({});
const columns = signal<Record<string, ColumnsByTable>>({});
const databases = signal<Record<string, string[]>>({});
const connected = signal<Record<string, true>>({});
const loading = signal<Record<string, true>>({});

export const conns = {
  /** 全部连接。 */
  list: () => connections(),
  get: (id: string | null | undefined) => (id ? connections().find((c) => c.id === id) : undefined),
  /** 侧栏聚焦的连接 id。 */
  activeId: () => activeId(),
  active: () => conns.get(activeId()),
  tables: (id: string) => tables()[id],
  columns: (id: string) => columns()[id],
  databases: (id: string) => databases()[id],
  isConnected: (id: string) => !!connected()[id],
  isLoading: (id: string) => !!loading()[id],
};

/** 从磁盘重读连接列表。 */
export function reloadConnections() {
  try {
    connections.set(store.connections());
  } catch (e) {
    toast.error(t("store.load_connections_failed", { error: errorText(e) }));
  }
}

export function activate(id: string) {
  activeId.set(id);
}

function setKey<T>(s: { update(f: (v: Record<string, T>) => Record<string, T>): void }, id: string, v: T | undefined) {
  s.update((m) => {
    const next = { ...m };
    if (v === undefined) delete next[id];
    else next[id] = v;
    return next;
  });
}

/** 加载连接的表/key 列表;成功才标记已连接。关系库在后台顺带拉取整库列信息(失败静默)。 */
export async function loadTables(id: string): Promise<string[]> {
  const conn = conns.get(id);
  if (!conn) return [];
  if (loading()[id]) return tables()[id] ?? [];
  setKey(loading, id, true);
  let list: string[];
  try {
    await db.connect(conn.kind, id);
    list = await db.listObjects(conn.kind, id);
  } catch (e) {
    setKey(loading, id, undefined);
    toast.error(t("store.conn_load_failed", { name: conn.name, error: errorText(e) }));
    return [];
  }
  batch(() => {
    setKey(loading, id, undefined);
    setKey(tables, id, list);
    setKey(connected, id, true);
  });
  if (supportsSql(conn.kind)) {
    db.fetchColumns(conn.kind, id)
      .then((cols) => setKey(columns, id, cols))
      .catch(() => {});
  }
  return list;
}

/** 实例下的库列表(带缓存)。 */
export async function loadDatabases(id: string): Promise<string[]> {
  const cached = databases()[id];
  if (cached) return cached;
  const conn = conns.get(id);
  if (!conn) return [];
  const list = await db.listDatabases(conn.kind, id);
  setKey(databases, id, list);
  return list;
}

/** 丢弃某连接的缓存,下次展开重新加载。 */
export function forget(id: string) {
  batch(() => {
    setKey(tables, id, undefined);
    setKey(columns, id, undefined);
    setKey(databases, id, undefined);
    setKey(connected, id, undefined);
  });
}

/** 保存连接;配置可能变了,丢弃旧缓存。返回保存后的连接。 */
export function saveConnection(conn: Connection, password: string | null): Connection {
  const saved = store.saveConnection(conn, password);
  forget(saved.id);
  reloadConnections();
  return saved;
}

export function deleteConnection(id: string) {
  store.deleteConnection(id);
  forget(id);
  if (activeId() === id) activeId.set(null);
  reloadConnections();
}

/** 切库:同实例已有该库的连接则复用,否则克隆一个。返回目标连接。 */
export function switchDatabase(id: string, database: string): Connection {
  const next = store.switchDatabase(id, database);
  reloadConnections();
  return next;
}

/** 断开连接(服务器侧会话随之终止)并清缓存。 */
export async function disconnect(id: string) {
  const conn = conns.get(id);
  if (!conn) return;
  await db.disconnect(conn.kind, id);
  forget(id);
}

/** 同实例(类型 / 主机 / 端口 / 用户相同)下指向 `database` 的其他连接。 */
export function siblings(conn: Connection, database: string): Connection[] {
  return connections().filter(
    (x) =>
      x.id !== conn.id &&
      x.kind === conn.kind &&
      x.host === conn.host &&
      x.port === conn.port &&
      x.user === conn.user &&
      x.database === database,
  );
}

/** 从缓存的库列表里去掉一个库(DROP DATABASE 之后)。 */
export function forgetDatabase(id: string, database: string) {
  const list = databases()[id];
  if (list) setKey(databases, id, list.filter((d) => d !== database));
}

/** 刷新对象列表(表结构变更后)。 */
export async function refreshTables(id: string) {
  setKey(tables, id, undefined);
  return loadTables(id);
}
