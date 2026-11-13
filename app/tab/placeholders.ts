// 慢 SQL 统计(pg_stat_statements、performance_schema 摘要)里的语句是归一化过的:
// 常量换成了 `$1` / `?`。把它们改写成编辑器的命名参数 `:p1`、`:p2` …,
// Explain / 执行前就会弹框让人填实际值。字符串、带引号的标识符、注释里的不动。

import type { DbKind } from "../lib/types";

export function namedPlaceholders(sql: string, kind: DbKind): { sql: string; count: number } {
  let out = "";
  let n = 0;
  let max = 0;
  let i = 0;
  while (i < sql.length) {
    const c = sql[i];
    // 引号:'…'(''转义)、"…"、`…`
    if (c === "'" || c === '"' || c === "`") {
      let j = i + 1;
      while (j < sql.length) {
        if (sql[j] === c) {
          if (sql[j + 1] === c) {
            j += 2;
            continue;
          }
          break;
        }
        if (c === "'" && sql[j] === "\\" && kind === "mysql") j++;
        j++;
      }
      out += sql.slice(i, j + 1);
      i = j + 1;
      continue;
    }
    // 注释:-- 行注释、/* 块注释 */
    if (c === "-" && sql[i + 1] === "-") {
      const end = sql.indexOf("\n", i);
      const j = end < 0 ? sql.length : end;
      out += sql.slice(i, j);
      i = j;
      continue;
    }
    if (c === "/" && sql[i + 1] === "*") {
      const end = sql.indexOf("*/", i + 2);
      const j = end < 0 ? sql.length : end + 2;
      out += sql.slice(i, j);
      i = j;
      continue;
    }
    // PG:$1 $2 …(前面不能是标识符字符,避免误伤 a$1 这样的名字)
    if (kind === "pg" && c === "$" && /[0-9]/.test(sql[i + 1] ?? "") && !/[\w$]/.test(sql[i - 1] ?? "")) {
      let j = i + 1;
      while (j < sql.length && /[0-9]/.test(sql[j])) j++;
      const k = Number(sql.slice(i + 1, j));
      max = Math.max(max, k);
      out += `:p${k}`;
      i = j;
      continue;
    }
    // MySQL:? 按出现顺序编号
    if (kind === "mysql" && c === "?") {
      n++;
      out += `:p${n}`;
      i++;
      continue;
    }
    out += c;
    i++;
  }
  return { sql: out, count: kind === "pg" ? max : n };
}
