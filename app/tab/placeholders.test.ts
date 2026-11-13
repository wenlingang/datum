// 归一化语句的占位符 → 命名参数。
import { expect, test } from "nsh/test";
import { namedPlaceholders } from "./placeholders";

test("PG:$n 换成 :pN,字符串、引号标识符、注释里的不动", () => {
  const r = namedPlaceholders(`SELECT * FROM "t$1" WHERE a = $1 AND b IN ($2, $10) AND c = '$3' -- $4\n/* $5 */`, "pg");
  expect(r.sql).toBe(`SELECT * FROM "t$1" WHERE a = :p1 AND b IN (:p2, :p10) AND c = '$3' -- $4\n/* $5 */`);
  expect(r.count).toBe(10);
  // 标识符里的 $ 不算
  expect(namedPlaceholders("SELECT a$1 FROM t", "pg").count).toBe(0);
  // PG 的 ? 是 jsonb 运算符,不改
  expect(namedPlaceholders("SELECT j ? 'k' FROM t", "pg").sql).toBe("SELECT j ? 'k' FROM t");
});

test("MySQL:? 按顺序编号,反斜杠转义的字符串跳过", () => {
  const r = namedPlaceholders("SELECT * FROM `a?` WHERE x = ? AND y = 'it\\'s ?' AND z IN (?, ?)", "mysql");
  expect(r.sql).toBe("SELECT * FROM `a?` WHERE x = :p1 AND y = 'it\\'s ?' AND z IN (:p2, :p3)");
  expect(r.count).toBe(3);
});
