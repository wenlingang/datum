// Redis 值的查看格式偏好:按「连接 + key 前缀」记住手动选的格式,落盘到配置目录的 redis_formats.json。
// 同一前缀下的 key 一般是同一种序列化(user:1、user:2 都是 MessagePack),选一次对整组生效。
// 选回「自动」即删除记录,文件里只留手动选过的。

import { signal } from "nsh";
import { store } from "../lib/native";
import type { RedisViewFormat } from "../lib/types";

export const VIEW_FORMATS: RedisViewFormat[] = ["auto", "text", "hex", "json", "msgpack", "gzip", "zlib", "java", "protobuf"];

/** 连接 id → 前缀 → 格式 */
type Data = Record<string, Record<string, RedisViewFormat>>;

function load(): Data {
  try {
    const raw = store.loadRedisFormats() as { formats?: unknown } | null;
    const all = raw?.formats;
    if (!all || typeof all !== "object") return {};
    const out: Data = {};
    for (const [conn, m] of Object.entries(all as Record<string, unknown>)) {
      if (!m || typeof m !== "object") continue;
      const ok = Object.entries(m as Record<string, unknown>).filter(
        (e): e is [string, RedisViewFormat] => VIEW_FORMATS.includes(e[1] as RedisViewFormat) && e[1] !== "auto",
      );
      if (ok.length) out[conn] = Object.fromEntries(ok);
    }
    return out;
  } catch {
    return {};
  }
}

const data = signal<Data>(load());

/** key 的前缀:最后一个 `:` 之前(`user:profile:1` → `user:profile`);没有 `:` 时为 key 本身。 */
export function keyPrefix(key: string): string {
  const i = key.lastIndexOf(":");
  return i >= 0 ? key.slice(0, i) : key;
}

/** 这个 key 记住的查看格式;没选过为 auto。 */
export function viewFormat(connId: string, key: string): RedisViewFormat {
  return data()[connId]?.[keyPrefix(key)] ?? "auto";
}

/** 记住选择(选 auto 即清掉记录)。写盘失败静默:偏好丢了不值得打扰用户。 */
export function setViewFormat(connId: string, key: string, format: RedisViewFormat) {
  const prefix = keyPrefix(key);
  const conn = { ...(data()[connId] ?? {}) };
  if (format === "auto") delete conn[prefix];
  else conn[prefix] = format;
  const next = { ...data() };
  if (Object.keys(conn).length) next[connId] = conn;
  else delete next[connId];
  data.set(next);
  try {
    store.saveRedisFormats({ version: 1, formats: next });
  } catch {
    // 同上
  }
}
