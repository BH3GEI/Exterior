// 假模型：本地 OpenAI 兼容服务器，按脚本返回文本和 tool call（SPEC §5.1）
// 用法: node e2e/fake-model/server.mjs <port> [requests.log]
// 规则按最后一条 user 消息内容匹配；若最后是 tool 消息，则回“基于结果”的总结。
import http from "node:http";
import fs from "node:fs";

const port = Number(process.argv[2] || 8399);
const logFile = process.argv[3];

const rules = [
  {
    match: /看看.*桌面/,
    tool: "ls -la ~/Desktop",
  },
  {
    match: /找找.*之前/,
    tool: "grep -o '[^\"]*想法[^\"]*' ~/.exterior/nodes.jsonl | head -5",
  },
  { match: /慢A/, stream: ["慢", "A", "回答", "完毕"], delay: 800 },
  { match: /慢B/, stream: ["慢", "B", "回答", "完毕"], delay: 400 },
  { match: /结合一下这两条/, text: "已结合两条路：分支A说A，分支B说B" },
  { match: /.*/, text: "这是对外装代脑的回答" },
];

function sse(res, obj) {
  res.write(`data: ${JSON.stringify(obj)}\n\n`);
}

const server = http.createServer((req, res) => {
  if (req.method !== "POST" || !req.url.endsWith("/chat/completions")) {
    res.writeHead(404).end();
    return;
  }
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    const parsed = JSON.parse(body);
    if (logFile) fs.appendFileSync(logFile, JSON.stringify(parsed) + "\n");
    const msgs = parsed.messages;
    const last = msgs[msgs.length - 1];
    const model = parsed.model;

    res.writeHead(200, {
      "content-type": "text/event-stream",
      "cache-control": "no-cache",
    });
    const head = {
      id: "chatcmpl-fake",
      object: "chat.completion.chunk",
      created: 0,
      model,
    };
    let closed = false;
    res.on("close", () => (closed = true));

    const sendText = (chunks, delay = 30) => {
      let i = 0;
      const tick = () => {
        if (closed) return;
        if (i < chunks.length) {
          sse(res, { ...head, choices: [{ index: 0, delta: { content: chunks[i++] } }] });
          setTimeout(tick, delay);
        } else {
          sse(res, { ...head, choices: [{ index: 0, delta: {}, finish_reason: "stop" }] });
          res.write("data: [DONE]\n\n");
          res.end();
        }
      };
      tick();
    };

    if (last.role === "tool") {
      sendText([`基于结果回答：${String(last.content).slice(0, 80)}`]);
      return;
    }
    const text = last.content ?? "";
    const rule = rules.find((r) => r.match.test(text));
    if (rule.tool) {
      sse(res, {
        ...head,
        choices: [{
          index: 0,
          delta: {
            tool_calls: [{
              index: 0,
              id: `call_${Date.now()}`,
              type: "function",
              function: { name: "bash", arguments: JSON.stringify({ command: rule.tool }) },
            }],
          },
          finish_reason: "tool_calls",
        }],
      });
      res.write("data: [DONE]\n\n");
      res.end();
    } else if (rule.stream) {
      sendText(rule.stream, rule.delay);
    } else {
      sendText([rule.text]);
    }
  });
});

server.listen(port, "127.0.0.1", () => {
  console.log(`fake model on :${port}`);
});
