// 补全里「低基数列的实际取值」:按 连接 / 表 / 列 缓存,同一列并发只查一次。
// 取值由原生侧在前几千行里抽样(只读、限时),不同的值超过 30 个、或类型不像枚举时为 null(不再提示)。

import { db } from "../lib/native";
import type { DbKind } from "../lib/types";

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

const NUMERIC_TYPE = /int|serial|bit|bool/i;

/** 取值插入编辑器的写法:整数类列且值是数字时原样,否则写成字符串字面量。 */
export function valueInsert(value: string, dataType: string, literal: (v: string) => string): string {
  return NUMERIC_TYPE.test(dataType) && /^-?\d+$/.test(value) ? value : literal(value);
}
