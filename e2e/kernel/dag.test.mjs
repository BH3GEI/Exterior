// Exterior DAG 插件的端到端测试（SPEC §3）：真实二进制 --profile exterior，
// 经 JSON-RPC exterior/think 驱动；假模型是另一个 URL；nodes.jsonl 用真的。
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { KernelClient, tmpHome, startFakeModel } from "./driver.mjs";

function startExterior(home, port) {
  // ~/.exterior/config.json（SPEC §2.3 格式），DSH_HOME 指向 ~/.exterior
  const dshHome = path.join(home, ".exterior");
  fs.mkdirSync(dshHome, { recursive: true });
  fs.writeFileSync(
    path.join(dshHome, "config.json"),
    JSON.stringify({
      models: [{ name: "fake", base_url: `http://127.0.0.1:${port}/v1`, model: "fake-1" }],
      default: "fake",
    })
  );
  return new KernelClient(home, "exterior", dshHome);
}

function readNodes(home) {
  const file = path.join(home, ".exterior", "nodes.jsonl");
  if (!fs.existsSync(file)) return [];
  return fs.readFileSync(file, "utf8").trim().split("\n").map(JSON.parse);
}

test("DAG：分支与合并的上下文组装（SPEC §3.1）", async (t) => {
  const home = tmpHome();
  const port = 18300 + Math.floor(Math.random() * 500);
  const fake = startFakeModel(port);
  await fake.ready();
  const client = startExterior(home, port);
  try {
    await client.initialize(home, "fake", "fake-1");
    // 根：问题A → 回答A
    const { run: runA } = await client.request("exterior/think", { parents: [], text: "问题A" });
    await client.wait(runA);
    // 从 user 节点岔开：问题B（SPEC §5.2.3）
    const nodes1 = readNodes(home);
    const userA = nodes1.find((n) => n.text === "问题A");
    const assistantA = nodes1.find((n) => n.text === "回答A：苹果");
    assert.ok(userA && assistantA, "分支 A 的节点落盘");
    assert.deepEqual(assistantA.parents, [userA.id]);

    const { run: runB } = await client.request("exterior/think", { parents: [userA.id], text: "问题B" });
    await client.wait(runB);

    // 分支隔离：B 的请求里不能有 A 的回答
    const reqsB = fake.requests().filter((r) => JSON.stringify(r.messages).includes("问题B"));
    const lastB = reqsB[reqsB.length - 1];
    const textB = JSON.stringify(lastB.messages);
    assert.ok(textB.includes("问题A"), "共同部分在");
    assert.ok(!textB.includes("回答A"), "B 不知道 A 分支后面的内容");

    const nodes2 = readNodes(home);
    const assistantB = nodes2.find((n) => n.text === "回答B：香蕉");
    assert.ok(assistantB, "分支 B 落盘");

    // 合并：两个来路（SPEC §5.2.4）
    const { run: runM } = await client.request("exterior/think", {
      parents: [assistantA.id, assistantB.id],
      text: "结合一下这两条",
    });
    await client.wait(runM);

    const nodes3 = readNodes(home);
    const mergeUser = nodes3.find((n) => n.text === "结合一下这两条");
    assert.deepEqual(mergeUser.parents, [assistantA.id, assistantB.id], "合并节点有两个来路，按选中顺序");

    const reqsM = fake.requests().filter((r) => JSON.stringify(r.messages).includes("结合一下这两条"));
    const lastM = reqsM[reqsM.length - 1];
    const textM = JSON.stringify(lastM.messages);
    assert.ok(textM.includes("[来路 1/2]"), "有来路 1 标记");
    assert.ok(textM.includes("[来路 2/2]"), "有来路 2 标记");
    assert.ok(textM.includes("回答A：苹果"), "来路 1 的内容在");
    assert.ok(textM.includes("回答B：香蕉"), "来路 2 的内容在");
    // 来路顺序：1 是 A，2 是 B
    assert.ok(textM.indexOf("回答A：苹果") < textM.indexOf("回答B：香蕉"), "来路顺序按 parents");
    // 合并的回答落到 nodes.jsonl
    const mergeAnswer = nodes3.find((n) => n.text === "已结合两条路：分支A说A，分支B说B");
    assert.ok(mergeAnswer, "合并回答落盘");
    assert.deepEqual(mergeAnswer.parents, [mergeUser.id]);
  } finally {
    await client.close();
    fake.kill();
  }
});

test("DAG：bash 工具节点（SPEC §3.2.4 + 壳外输出）", async (t) => {
  const home = tmpHome();
  const port = 18300 + Math.floor(Math.random() * 500);
  const fake = startFakeModel(port);
  await fake.ready();
  fs.mkdirSync(path.join(home, "Desktop"), { recursive: true });
  fs.writeFileSync(path.join(home, "Desktop", "桌面文件A.txt"), "");
  const client = startExterior(home, port);
  try {
    await client.initialize(home, "fake", "fake-1");
    const { run } = await client.request("exterior/think", { parents: [], text: "看看我桌面上有什么" });
    await client.wait(run);
    const nodes = readNodes(home);
    const bashNode = nodes.find((n) => n.kind === "bash");
    assert.ok(bashNode, "有 bash 节点");
    assert.equal(bashNode.exit_code, 0);
    assert.ok(bashNode.output.includes("桌面文件A.txt"), "bash 输出进节点");
    assert.ok(bashNode.call_id, "call_id 配对");
    const toolCallNode = nodes.find((n) => n.tool_call);
    assert.ok(toolCallNode, "assistant 节点带 tool_call");
    assert.deepEqual(bashNode.parents, [toolCallNode.id], "bash 挂在带 tool_call 的 assistant 下");
    const finalAnswer = nodes.filter((n) => n.kind === "assistant" && n.text?.includes("基于结果回答"));
    assert.equal(finalAnswer.length, 1, "基于结果的回答");
    assert.deepEqual(finalAnswer[0].parents, [bashNode.id], "回答挂在 bash 节点下");
  } finally {
    await client.close();
    fake.kill();
  }
});

test("DAG：换模型后节点显示新模型名（SPEC §5.2.9 的数据路径）", async (t) => {
  const home = tmpHome();
  const port = 18300 + Math.floor(Math.random() * 500);
  const fake = startFakeModel(port);
  await fake.ready();
  const dshHome = path.join(home, ".exterior");
  fs.mkdirSync(dshHome, { recursive: true });
  fs.writeFileSync(
    path.join(dshHome, "config.json"),
    JSON.stringify({
      models: [
        { name: "fake", base_url: `http://127.0.0.1:${port}/v1`, model: "fake-1" },
        { name: "fake2", base_url: `http://127.0.0.1:${port}/v1`, model: "fake-2" },
      ],
      default: "fake",
    })
  );
  const client = new KernelClient(home, "exterior", dshHome);
  try {
    await client.initialize(home, "fake", "fake-1");
    const r1 = await client.request("exterior/think", { parents: [], text: "你好" });
    await client.wait(r1.run);
    const r2 = await client.request("exterior/think", { parents: [], text: "再聊", model: "fake2" });
    await client.wait(r2.run);
    const nodes = readNodes(home);
    const assistants = nodes.filter((n) => n.kind === "assistant");
    assert.equal(assistants[0].model, "fake");
    assert.equal(assistants[1].model, "fake2");
    // 假服务器收到的第二个请求用的是 fake-2 的 model id
    const reqs = fake.requests();
    assert.equal(reqs[reqs.length - 1].model, "fake-2");
  } finally {
    await client.close();
    fake.kill();
  }
});
