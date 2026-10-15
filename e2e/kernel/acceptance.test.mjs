// SPEC §0.5 验收：全部为端到端测试 —— 真实二进制、stdio JSON-RPC、假模型（另一个 URL）、真实 bash 与 JSONL、独立临时 HOME。
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { withKernel, KernelClient, tmpHome, startFakeModel } from "./driver.mjs";

// 0.1 单轮：initialize → prompt → session.event 流 → wait 返回，最终文本正确
test("0.1 单轮", async (t) => {
  await withKernel(t, async ({ home, client }) => {
    const init = await client.initialize(home);
    assert.equal(init.serverInfo.name, "deepseek-harness-sdk-runtime");
    const { messageId } = await client.prompt("s1", "你好");
    assert.ok(messageId);
    await client.wait("s1");
    const events = client.sessionEvents("s1");
    const types = events.map((e) => e.type);
    assert.ok(types.includes("turn/start"), "有 turn/start");
    assert.ok(types.includes("user/message"), "有 user/message");
    assert.ok(types.includes("assistant/message"), "有 assistant/message");
    assert.ok(types.includes("turn/end"), "有 turn/end");
    const assistant = events.find((e) => e.type === "assistant/message");
    const text = assistant.data.message.content.find((b) => b.type === "text")?.text;
    assert.equal(text, "这是对外装代脑的回答");
    const end = events.find((e) => e.type === "turn/end");
    assert.equal(end.data.reason.kind, "completed");
    // 持久化：文件里有同样的事件
    const log = client.sessionLog("s1");
    assert.equal(log[0].type, "session");
    assert.equal(log[0].version, 4);
    assert.ok(log.some((l) => l.type === "assistant/message"));
  });
});

// 0.2 持久 shell：cd /tmp && export X=1 之后 pwd; echo $X 输出 /tmp 和 1
test("0.2 持久 shell", async (t) => {
  await withKernel(t, async ({ home, client }) => {
    await client.initialize(home);
    await client.prompt("s2", "先切换到 /tmp");
    await client.wait("s2");
    await client.prompt("s2", "看看当前状态");
    await client.wait("s2");
    const events = client.sessionEvents("s2");
    const results = events.filter((e) => e.type === "tool/result");
    assert.equal(results.length, 2);
    const second = results[1].data.message.content[0].text;
    assert.ok(second.includes("/tmp"), `pwd 应是 /tmp：${second}`);
    assert.ok(second.split("\n").some((l) => l.trim() === "1"), `X 应为 1：${second}`);
    assert.ok(second.includes("[Command finished with exit code 0]"));
  });
});

// 0.3 超时：timeoutMs=2000 执行 sleep 10 → 超时说明 + shell 重置 + 下次正常
test("0.3 超时", async (t) => {
  const home = tmpHome();
  const port = 18300 + Math.floor(Math.random() * 500);
  const fake = startFakeModel(port);
  await fake.ready();
  // 用户 profile 覆盖 persistent-bash 的 timeoutMs（SPEC §0.2：profile 文件可覆盖配置）
  fs.mkdirSync(path.join(home, ".dsh", "profiles", "sdk-minimal"), { recursive: true });
  fs.writeFileSync(
    path.join(home, ".dsh", "profiles", "sdk-minimal", "cordis.patch.yml"),
    `- insert:\n    - id: persistent-bash\n      name: persistent-bash\n      config:\n        timeoutMs: 2000\n`
  );
  fs.writeFileSync(
    path.join(home, ".dsh", "config.json"),
    JSON.stringify({ providers: [{ name: "fake", base_url: `http://127.0.0.1:${port}/v1` }] })
  );
  const client = new KernelClient(home);
  try {
    await client.initialize(home);
    await client.prompt("s3", "一直睡");
    await client.wait("s3");
    const events = client.sessionEvents("s3");
    const result = events.find((e) => e.type === "tool/result");
    const text = result.data.message.content[0].text;
    assert.ok(text.includes("Your command timed out after 2 seconds"), `超时说明：${text}`);
    assert.ok(text.includes("[Command timed out or OOM]"));
    assert.ok(text.includes("The persistent bash shell was reset"));
    // shell 已重置：下一次调用正常执行
    await client.prompt("s3", "两个命令");
    await client.wait("s3");
    const all = client.sessionEvents("s3").filter((e) => e.type === "tool/result");
    const last = all[all.length - 1].data.message.content[0].text;
    assert.ok(last.includes("第二个命令"), `重置后正常：${last}`);
  } finally {
    await client.close();
    fake.kill();
  }
});

// 0.4 输出截断：超过 maxOutputChars 时截断并附加说明
test("0.4 输出截断", async (t) => {
  await withKernel(t, async ({ home, client }) => {
    await client.initialize(home);
    await client.prompt("s4", "超长输出");
    await client.wait("s4");
    const result = client.sessionEvents("s4").find((e) => e.type === "tool/result");
    const text = result.data.message.content[0].text;
    assert.ok(text.length < 20000, "输出被截断");
    assert.ok(text.includes("<response clipped><NOTE>"), "有截断说明");
    assert.ok(text.includes("[Command finished with exit code 0]"));
    assert.ok(!text.includes("A".repeat(16001)));
  });
});

// 0.5 多个 tool call：一次返回两个，都执行，结果按顺序回到模型
test("0.5 多个 tool call", async (t) => {
  await withKernel(t, async ({ home, client, fake }) => {
    await client.initialize(home);
    await client.prompt("s5", "两个命令");
    await client.wait("s5");
    const events = client.sessionEvents("s5");
    const results = events.filter((e) => e.type === "tool/result");
    assert.equal(results.length, 2);
    assert.ok(results[0].data.message.content[0].text.includes("第一个命令"));
    assert.ok(results[1].data.message.content[0].text.includes("第二个命令"));
    // 模型收到的第二轮请求里两个 tool 结果按顺序排列
    const reqs = fake.requests();
    const second = reqs[1];
    const toolMsgs = second.messages.filter((m) => m.role === "tool");
    assert.equal(toolMsgs.length, 2);
    assert.ok(toolMsgs[0].content.includes("第一个命令"));
    assert.ok(toolMsgs[1].content.includes("第二个命令"));
  });
});

// 0.6 取消：生成中途 shutdown，已流出的文本保留在会话日志中
test("0.6 取消", async (t) => {
  const home = tmpHome();
  const port = 18300 + Math.floor(Math.random() * 500);
  const fake = startFakeModel(port);
  await fake.ready();
  fs.mkdirSync(path.join(home, ".dsh"), { recursive: true });
  fs.writeFileSync(
    path.join(home, ".dsh", "config.json"),
    JSON.stringify({ providers: [{ name: "fake", base_url: `http://127.0.0.1:${port}/v1` }] })
  );
  const client = new KernelClient(home);
  await client.initialize(home);
  await client.prompt("s6", "慢慢来"); // 假模型每 1.5 秒吐一块
  // 等到流出第一块（assistant 还未结算），然后 shutdown（dispose → 中止）
  await client.untilNotification((n) => n.method === "session.status" && n.params.status === "running");
  await new Promise((r) => setTimeout(r, 2000)); // 至少流出一块
  await client.shutdown();
  await client.exited; // shutdown 响应先于 dispose；进程退出后日志才结算完
  const log = client.sessionLog("s6");
  const interrupted = log.find((l) => l.type === "assistant/message" && l.data.interrupted === true);
  assert.ok(interrupted, "有 interrupted:true 的 assistant/message");
  const text = interrupted.data.message.content.find((b) => b.type === "text")?.text ?? "";
  assert.ok(text.length > 0, "已流出文本保留");
  assert.ok(!text.includes("完毕") && text.length < 12, "只是部分内容");
  const end = log.find((l) => l.type === "turn/end");
  assert.equal(end.data.reason.kind, "aborted");
  client.proc.kill();
  fake.kill();
});

// 0.7 重试：先 500 后正常；日志有 llm/retry；结果正确
test("0.7 重试", async (t) => {
  await withKernel(t, async ({ home, client }) => {
    await client.initialize(home);
    await client.prompt("s7", "先失败一次");
    await client.wait("s7");
    const log = client.sessionLog("s7");
    const retry = log.find((l) => l.type === "llm/retry");
    assert.ok(retry, "日志有 llm/retry");
    assert.equal(retry.data.failure.status, 500);
    assert.equal(retry.data.retry, 1);
    assert.ok(log.some((l) => l.type === "llm/retry-started"), "有 llm/retry-started");
    const assistant = log.find((l) => l.type === "assistant/message");
    assert.equal(assistant.data.message.content[0].text, "这是对外装代脑的回答");
    const end = log.find((l) => l.type === "turn/end");
    assert.equal(end.data.reason.kind, "completed");
  });
});

// 0.8 恢复：进程重启后用同一个 sessionId 发消息，请求包含之前的历史
test("0.8 恢复", async (t) => {
  const home = tmpHome();
  const port = 18300 + Math.floor(Math.random() * 500);
  const fake = startFakeModel(port);
  await fake.ready();
  fs.mkdirSync(path.join(home, ".dsh"), { recursive: true });
  fs.writeFileSync(
    path.join(home, ".dsh", "config.json"),
    JSON.stringify({ providers: [{ name: "fake", base_url: `http://127.0.0.1:${port}/v1` }] })
  );
  const c1 = new KernelClient(home);
  await c1.initialize(home);
  await c1.prompt("s8", "你好");
  await c1.wait("s8");
  await c1.shutdown();
  // 重启进程
  const c2 = new KernelClient(home);
  try {
    await c2.initialize(home);
    await c2.prompt("s8", "又来");
    await c2.wait("s8");
    const reqs = fake.requests();
    const last = reqs[reqs.length - 1];
    const text = JSON.stringify(last.messages);
    assert.ok(text.includes("你好"), "请求含之前的问题");
    assert.ok(text.includes("这是对外装代脑的回答"), "请求含之前的回答");
    assert.ok(text.includes("又来"), "请求含新问题");
    // 恢复后的首个请求记 resume
    const log = c2.sessionLog("s8");
    assert.ok(log.some((l) => l.type === "request/header" && l.data.reason === "resume"), "有 resume header");
  } finally {
    await c2.close();
    fake.kill();
  }
});

// 0.9 profile：用户 profile 关闭 working_directory 工具后，请求的工具列表不再出现它
test("0.9 profile 覆盖", async (t) => {
  const home = tmpHome();
  const port = 18300 + Math.floor(Math.random() * 500);
  const fake = startFakeModel(port);
  await fake.ready();
  fs.mkdirSync(path.join(home, ".dsh", "profiles", "sdk-minimal"), { recursive: true });
  fs.writeFileSync(
    path.join(home, ".dsh", "profiles", "sdk-minimal", "cordis.patch.yml"),
    `- insert:\n    - id: tool-working-directory\n      name: tool-working-directory\n      disabled: true\n`
  );
  fs.writeFileSync(
    path.join(home, ".dsh", "config.json"),
    JSON.stringify({ providers: [{ name: "fake", base_url: `http://127.0.0.1:${port}/v1` }] })
  );
  const client = new KernelClient(home);
  try {
    await client.initialize(home);
    await client.prompt("s9", "你好");
    await client.wait("s9");
    const reqs = fake.requests();
    const tools = reqs[0].tools.map((t) => t.function.name);
    assert.ok(tools.includes("bash"), "bash 在");
    assert.ok(!tools.includes("working_directory"), "working_directory 被关闭");
  } finally {
    await client.close();
    fake.kill();
  }
});

// 0.10 与原版一致：系统提示、工具 schema、消息序列与 model-visible.json 快照比对
test("0.10 与 model-visible.json 一致", async (t) => {
  // 快照随仓库 vendor（来自 deepseek-harness @ d7432673 scripts/snapshots/python-sdk-single-exe/minimal/）
  const snapshotPath = process.env.DSH_SNAPSHOT
    ?? new URL("./model-visible.json", import.meta.url).pathname;
  if (!fs.existsSync(snapshotPath)) {
    t.skip("model-visible.json 不在本机");
    return;
  }
  const snapshot = JSON.parse(fs.readFileSync(snapshotPath, "utf8"));
  await withKernel(t, async ({ home, client, fake }) => {
    await client.initialize(home);
    await client.prompt("s10", "Exercise the packaged minimal agent's persistent shell.");
    await client.wait("s10");
    const reqs = fake.requests().map((r) => normalizeWireRequest(r, fs.realpathSync(home)));
    assert.equal(reqs.length, snapshot.length, `请求数一致（${reqs.length} vs ${snapshot.length}）`);
    for (let i = 0; i < snapshot.length; i++) {
      assert.deepEqual(maskIds(reqs[i]), maskIds(snapshot[i]), `第 ${i} 个请求一致`);
    }
  });
});

/** id 每次运行都变（SPEC §0.5.10）：按出现顺序换成占位符 */
function maskIds(req) {
  const clone = JSON.parse(JSON.stringify(req));
  const ids = new Map();
  const mask = (id) => {
    if (!ids.has(id)) ids.set(id, `id-${ids.size}`);
    return ids.get(id);
  };
  for (const m of clone.messages) {
    for (const tc of m.toolCalls ?? []) tc.id = mask(tc.id);
    for (const b of m.content ?? []) {
      if (b.tool_use_id) b.tool_use_id = mask(b.tool_use_id);
    }
  }
  return clone;
}

/** 我的 OpenAI wire 请求 → 快照的规范化形状；真实 cwd / tool 结果换回占位符 */
function normalizeWireRequest(req, home) {
  const system = req.messages.find((m) => m.role === "system")?.content ?? "";
  const tools = (req.tools ?? []).map((t) => ({
    name: t.function.name,
    description: t.function.description,
    input_schema: t.function.parameters,
  }));
  const messages = [];
  for (const m of req.messages.filter((m) => m.role !== "system")) {
    if (m.role === "user") {
      const content = (Array.isArray(m.content) ? m.content : [{ type: "text", text: m.content }])
        .map((b) => ({ ...b, text: b.text?.replaceAll(JSON.stringify(home), '"{{cwd}}"') }));
      messages.push({ role: "user", content });
    } else if (m.role === "assistant") {
      if (m.tool_calls) {
        messages.push({
          role: "assistant",
          toolCalls: m.tool_calls.map((tc) => ({ id: tc.id, name: tc.function.name })),
        });
      }
      const text = typeof m.content === "string" && m.content
        ? [{ type: "text", text: m.content }]
        : [];
      if (text.length) messages.push({ role: "assistant", content: text });
    } else if (m.role === "tool") {
      messages.push({
        role: "user",
        content: [{ type: "tool_result", tool_use_id: m.tool_call_id, content: "{{tool-result}}" }],
      });
    }
  }
  return { system, tools, messages };
}

// 0.11 持久化可关可换：关掉 session-persistence-jsonl 后不生成 JSONL 文件；
// 换成内存实现后进程内正常、重启后历史丢失（行为随实现改变）
test("0.11 持久化插件可关可换", async (t) => {
  const port = 18300 + Math.floor(Math.random() * 500);

  // Part A：关闭 session-persistence-jsonl → 不落盘
  {
    const home = tmpHome();
    fs.mkdirSync(path.join(home, ".dsh", "profiles", "sdk-minimal"), { recursive: true });
    fs.writeFileSync(
      path.join(home, ".dsh", "profiles", "sdk-minimal", "cordis.patch.yml"),
      `- insert:\n    - id: sessions\n      name: session-persistence-jsonl\n      disabled: true\n`
    );
    fs.writeFileSync(
      path.join(home, ".dsh", "config.json"),
      JSON.stringify({ providers: [{ name: "fake", base_url: `http://127.0.0.1:${port}/v1` }] })
    );
    const fake = startFakeModel(port);
    await fake.ready();
    const client = new KernelClient(home);
    try {
      await client.initialize(home);
      await client.prompt("s11a", "你好");
      await client.wait("s11a");
      assert.ok(
        !fs.existsSync(path.join(home, ".dsh", "sessions")),
        "关闭持久化后不应生成 sessions 目录"
      );
      // 功能本身正常：回答照常到达
      const assistant = client.sessionEvents("s11a").find((e) => e.type === "assistant/message");
      assert.ok(assistant, "无持久化时回答照常");
    } finally {
      await client.close();
      fake.kill();
    }
  }

  // Part B：换成内存实现 → 进程内恢复可用，重启后历史丢失
  {
    const home = tmpHome();
    fs.mkdirSync(path.join(home, ".dsh", "profiles", "sdk-minimal"), { recursive: true });
    fs.writeFileSync(
      path.join(home, ".dsh", "profiles", "sdk-minimal", "cordis.patch.yml"),
      `- insert:\n    - id: sessions\n      name: session-persistence-jsonl\n      disabled: true\n    - id: sessions-mem\n      name: session-persistence-memory\n`
    );
    fs.writeFileSync(
      path.join(home, ".dsh", "config.json"),
      JSON.stringify({ providers: [{ name: "fake", base_url: `http://127.0.0.1:${port}/v1` }] })
    );
    const fake = startFakeModel(port);
    await fake.ready();
    const c1 = new KernelClient(home);
    try {
      await c1.initialize(home);
      await c1.prompt("s11b", "你好");
      await c1.wait("s11b");
      assert.ok(
        !fs.existsSync(path.join(home, ".dsh", "sessions")),
        "内存实现不写 JSONL 文件"
      );
      // 进程内恢复：dispose agent 不结束进程，新建进程才丢 —— 这里直接验证重启
      await c1.shutdown();
      await c1.exited;

      const c2 = new KernelClient(home);
      try {
        await c2.initialize(home);
        await c2.prompt("s11b", "又来");
        await c2.wait("s11b");
        const reqs = fake.requests();
        const last = reqs[reqs.length - 1];
        const text = JSON.stringify(last.messages);
        assert.ok(!text.includes("这是对外装代脑的回答"), "重启后历史丢失（内存实现不落盘）");
        assert.ok(text.includes("又来"), "新问题在");
      } finally {
        await c2.close();
      }
    } finally {
      fake.kill();
    }
  }
});
