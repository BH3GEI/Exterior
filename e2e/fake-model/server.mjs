// 假模型：本地 OpenAI 兼容服务器，按脚本返回文本和 tool call（SPEC §5.1 / §0.5）
// 用法: node e2e/fake-model/server.mjs <port> <requests.log>
// 规则按最后一条 user 消息内容匹配；最后是 tool 消息时按已执行 tool 数决定下一步。
// 应用代码里没有任何测试专用分支；本文件只属于测试设施。
import http from "node:http";
import fs from "node:fs";

const port = Number(process.argv[2] || 8399);
const logFile = process.argv[3];
const failOnce = new Set(); // 先失败一次规则的状态

const rules = [
  { match: /看看.*桌面/, tool: "ls -la ~/Desktop" },
  { match: /找找.*之前/, tool: "grep -o '[^\"]*想法[^\"]*' ~/.exterior/nodes.jsonl | head -5" },
  { match: /慢A/, stream: ["慢", "A", "回答", "完毕"], delay: 800 },
  { match: /慢B/, stream: ["慢", "B", "回答", "完毕"], delay: 400 },
  { match: /结合一下这两条/, text: "已结合两条路：分支A说A，分支B说B" },
  { match: /问题A/, text: "回答A：苹果" },
  { match: /问题B/, text: "回答B：香蕉" },
  // ---- 内核验收用 ----
  { match: /Exercise the packaged minimal agent/, tool: "cd /tmp && pwd" }, // 第二个 tool call 由 tool 分支处理
  { match: /先切换到 \/tmp/, tool: "cd /tmp && export X=1" },
  { match: /看看当前状态/, tool: "pwd; echo $X" },
  { match: /两个命令/, tools: ["echo 第一个命令", "echo 第二个命令"] },
  { match: /超长输出/, tool: "printf 'A%.0s' {1..20000}" },
  { match: /一直睡/, tool: "sleep 10" },
  { match: /慢慢来/, stream: ["这是", "一段", "很慢的", "流式", "回答"], delay: 1500 },
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

    // 先失败一次：每个“先失败一次”会话的第一条请求返回 500
    const failKey = msgs.find((m) => m.role === "user" && /先失败一次/.test(JSON.stringify(m.content))) ? "k" : null;
    if (failKey && !failOnce.has(failKey + JSON.stringify(msgs[0]))) {
      failOnce.add(failKey + JSON.stringify(msgs[0]));
      res.writeHead(500, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: { message: "internal error" } }));
      return;
    }

    res.writeHead(200, {
      "content-type": "text/event-stream",
      "cache-control": "no-cache",
    });
    const head = { id: "chatcmpl-fake", object: "chat.completion.chunk", created: 0, model };
    let closed = false;
    res.on("close", () => (closed = true));

    const sendText = (chunks, delay = 30) => {
      let i = 0;
      const tick = () => {
        if (closed) { console.log("[fake] aborted early"); return; }
        if (i < chunks.length) {
          sse(res, { ...head, choices: [{ index: 0, delta: { content: chunks[i++] } }] });
          setTimeout(tick, delay);
        } else {
          sse(res, { ...head, choices: [{ index: 0, delta: {}, finish_reason: "stop" }] });
          res.write("data: [DONE]\n\n");
          res.end();
          console.log(`[fake] response complete (closed=${closed})`);
        }
      };
      tick();
    };
    const sendTools = (cmds) => {
      cmds.forEach((cmd, i) => {
        sse(res, {
          ...head,
          choices: [{
            index: 0,
            delta: {
              tool_calls: [{
                index: i,
                id: `call_${i}_${Date.now()}`,
                type: "function",
                function: { name: "bash", arguments: JSON.stringify({ command: cmd }) },
              }],
            },
            ...(i === cmds.length - 1 ? { finish_reason: "tool_calls" } : {}),
          }],
        });
      });
      res.write("data: [DONE]\n\n");
      res.end();
    };

    if (last.role === "tool") {
      const toolCount = msgs.filter((m) => m.role === "tool").length;
      // Exercise 脚本：第一个 tool 结果后再来一个 tool call，第二个之后给最终文本
      if (/Exercise the packaged minimal agent/.test(JSON.stringify(msgs)) && toolCount === 1) {
        sendTools(["pwd"]);
        return;
      }
      const outs = msgs.filter((m) => m.role === "tool").map((m) => String(m.content).slice(0, 120));
      sendText([`基于结果回答：${outs.join(" | ")}`]);
      return;
    }
    // 内容可能是 parts 数组（连续 user 消息在 wire 上合并）：
    // 提示词 = 最后一个非 runtime-context / 非来路标记的 part
    let text = "";
    if (typeof last.content === "string") {
      text = last.content;
    } else if (Array.isArray(last.content)) {
      const parts = last.content
        .map((p) => String(p?.text ?? ""))
        .filter((t) => !t.startsWith("Current runtime context.") && !t.startsWith("[来路 "));
      text = parts.at(-1) ?? "";
    }
    const rule = rules.find((r) => r.match.test(text));
    if (rule.tools) sendTools(rule.tools);
    else if (rule.tool) sendTools([rule.tool]);
    else if (rule.stream) sendText(rule.stream, rule.delay);
    else sendText([rule.text]);
  });
});

server.listen(port, "127.0.0.1", () => {
  console.log(`fake model on :${port}`);
});
