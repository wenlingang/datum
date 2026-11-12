#!/usr/bin/env python3
"""测试用的 OpenAI 兼容服务:任何 POST 都以 SSE 回同一段内容(argv[1] 文件),
请求体追加写到 argv[2]。启动后把端口写到 argv[3],一直运行到被杀掉。"""
import http.server
import json
import sys

REPLY = open(sys.argv[1], encoding="utf-8").read()
LOG = sys.argv[2]


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        with open(LOG, "ab") as f:
            f.write(body + b"\n")
        frame = json.dumps({"choices": [{"delta": {"content": REPLY}}]}, ensure_ascii=False)
        out = f"data: {frame}\n\ndata: [DONE]\n\n".encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def log_message(self, *args):
        pass


server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
with open(sys.argv[3], "w") as f:
    f.write(str(server.server_address[1]))
server.serve_forever()
