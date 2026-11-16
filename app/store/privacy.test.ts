// 已知来源表的行按连接规则遮罩(数据对比两侧、写操作记录的改动前数据共用)。
import { expect, test } from "nsh/test";
import { mask } from "../lib/native";
import { maskRowsOf } from "./privacy";

test("maskRowsOf:打开遮罩后敏感列打码,null 行原样保留;关掉后原值", () => {
  const cols = ["id", "email", "note"];
  const rows = [["1", "alice@example.com", "hi"], null, ["2", "bob@example.org", null]];
  mask.setEnabled("dev-sqlite", true);
  const out = maskRowsOf("dev-sqlite", "users", cols, rows);
  expect(out[1]).toBe(null);
  expect(out[0]![0]).toBe("1");
  expect(out[0]![1]).not.toBe("alice@example.com");
  expect(out[0]![1]!.endsWith("@example.com")).toBe(true);
  expect(out[2]![2]).toBe(null);
  // 手动标记的列按表名生效
  mask.setOverride("dev-sqlite", "users.note", "mask");
  expect(maskRowsOf("dev-sqlite", "users", cols, rows)[0]![2]).not.toBe("hi");
  mask.setOverride("dev-sqlite", "users.note", null);
  mask.setEnabled("dev-sqlite", false);
  expect(maskRowsOf("dev-sqlite", "users", cols, rows)[0]![1]).toBe("alice@example.com");
  mask.setEnabled("dev-sqlite", null);
});
