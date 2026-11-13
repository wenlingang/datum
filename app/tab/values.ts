// 补全里「低基数列的实际取值」:按 连接 / 表 / 列 缓存,同一列并发只查一次。
// 取值由原生侧在前几千行里抽样(只读、限时),不同的值超过 30 个、或类型不像枚举时为 null(不再提示)。
// JSON 列的键名(`col->'…`)同理:原生侧取前若干行推断整棵键树,这里一样按列缓存。

import { db } from "../lib/native";
import type { DbKind, JsonKeyTree, JsonPath } from "../lib/types";

/** 缓存多久后重新查(数据会变)。 */
const TTL_MS = 5 * 60 * 1000;

const cache = new Map<string, { at: number; values: Promise<string[] | null> }>();

export function columnValues(kind: DbKind, connId: string, table: string, column: string, dataType: string) {
  const key = `${connId}\u0000${table}\u0000${column}`;
  const hit = cache.get(key);
  if (hit && Date.now() - hit.at < TTL_MS) return hit.values;
  // 查询失败(超时、权限)也缓存成 null,别每敲一个字都去打数据库
  const values = db.distinctValues(kind, connId, table, column, dataType).catch(() => null);
  cache.set(key, { at: Date.now(), values });
  return values;
}

const keyTrees = new Map<string, { at: number; tree: Promise<JsonKeyTree | null> }>();

/** JSON 列的键树(补全 `col->'` 的键名):同样按 连接 / 表 / 列 缓存,整棵树取一次,逐级补全在本地沿路径查。 */
export function jsonKeyTree(kind: DbKind, connId: string, table: string, column: string) {
  const key = `${connId}\u0000${table}\u0000${column}`;
  const hit = keyTrees.get(key);
  if (hit && Date.now() - hit.at < TTL_MS) return hit.tree;
  const tree = db.jsonKeyTree(kind, connId, table, column).catch(() => null);
  keyTrees.set(key, { at: Date.now(), tree });
  return tree;
}

/** 键树沿路径往下(键找同名项,下标进数组元素的合并结构);走不通为 null。 */
export function keysAt(tree: JsonKeyTree, path: JsonPath): JsonKeyTree | null {
  let cur: JsonKeyTree | undefined = tree;
  for (const seg of path) {
    cur = typeof seg === "number" ? cur.items : cur.keys.find((k) => k.name === seg)?.tree;
    if (!cur) return null;
  }
  return cur;
}

/** 列类型是 JSON(PG 的 json / jsonb、MySQL 的 json)。 */
export const isJsonType = (dataType: string | null | undefined) => /json/i.test(dataType ?? "");

const NUMERIC_TYPE = /int|serial|bit|bool/i;

/** 取值插入编辑器的写法:整数类列且值是数字时原样,否则写成字符串字面量。 */
export function valueInsert(value: string, dataType: string, literal: (v: string) => string): string {
  return NUMERIC_TYPE.test(dataType) && /^-?\d+$/.test(value) ? value : literal(value);
}
