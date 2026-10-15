// SPEC §5.2 的十个场景，每条一个端到端测试（驱动真实 GUI + 检查 nodes.jsonl）。
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { GuiEnv, ARTIFACTS } from "./run.mjs";

const onCi = !!process.env.CI;

test("场景 1+2+3+4：第一个念头 → 执行 → 岔开 → 合并", { timeout: 180000 }, async (t) => {
  const env = new GuiEnv();
  await env.start();
  try {
    // 1. 第一个念头：空输入框 → 流式回答
    await env.say("问题A");
    const userA = await env.waitNode((n) => n.kind === "user" && n.text === "问题A", "userA");
    const aA = await env.waitNode((n) => n.kind === "assistant" && n.text === "回答A：苹果", "回答A");
    assert.deepEqual(aA.parents, [userA.id], "回答挂在问题下");
    await env.waitText("回答A：苹果");
    await env.shot("s1-first-thought");

    // 2. 执行：bash 节点默认折叠，展开看到 ls 输出，接着基于结果回答
    await env.say("看看我桌面上有什么");
    const bashNode = await env.waitNode((n) => n.kind === "bash", "bash 节点");
    assert.equal(bashNode.exit_code, 0);
    assert.ok(bashNode.output.includes("桌面文件A.txt"), "输出里有桌面文件");
    await env.waitNode((n) => n.kind === "assistant" && n.text?.includes("基于结果回答"), "后续回答");
    await env.shot("s2-bash-collapsed");
    // 展开 bash 节点（默认折叠）
    const expander = await env.browser.$(`[data-testid="bash-expand-${bashNode.id}"]`);
    await expander.waitForExist({ timeout: 5000 });
    await expander.click();
    await env.browser.$(`[data-testid="bash-output"]`).waitForExist({ timeout: 5000 });
    await env.shot("s2-bash-expanded");

    // 3. 岔开：点靠前的节点（userA），打字，线变网；新分支不知道另一分支后面的内容
    await env.clickNode(userA.id);
    await env.say("问题B");
    const uB = await env.waitNode((n) => n.kind === "user" && n.text === "问题B", "userB");
    assert.deepEqual(uB.parents, [userA.id], "B 从 A 岔开");
    const aB = await env.waitNode((n) => n.kind === "assistant" && n.text === "回答B：香蕉", "回答B");
    const reqsB = env.fake.requests().filter((r) => JSON.stringify(r.messages).includes("问题B"));
    const textB = JSON.stringify(reqsB.at(-1).messages);
    assert.ok(!textB.includes("回答A"), "B 分支不知道 A 分支的回答");
    await env.shot("s3-branched");

    // 4. 合并：Shift 选中两条分支末端，输入合并
    const branchAEnd = env.nodes().filter((n) => n.kind === "assistant" && n.text?.includes("基于结果回答"))[0];
    await env.clickNode(branchAEnd.id);
    await env.clickNode(aB.id, { shift: true });
    await env.say("结合一下这两条");
    const mergeUser = await env.waitNode((n) => n.kind === "user" && n.text === "结合一下这两条", "合并节点");
    assert.deepEqual(mergeUser.parents, [branchAEnd.id, aB.id], "parents 按选中顺序");
    await env.waitNode((n) => n.kind === "assistant" && n.text?.includes("已结合两条路"), "合并回答");
    const reqsM = env.fake.requests().filter((r) => JSON.stringify(r.messages).includes("结合一下这两条"));
    const textM = JSON.stringify(reqsM.at(-1).messages);
    assert.ok(textM.includes("[来路 1/2]") && textM.includes("[来路 2/2]"), "有来路标记");
    assert.ok(textM.includes("基于结果回答") && textM.includes("回答B：香蕉"), "两条路的内容都在");
    await env.shot("s4-merged");
  } finally {
    await env.stop();
  }
});

test("场景 5：并行与中止", { timeout: 120000 }, async (t) => {
  const env = new GuiEnv();
  await env.start();
  try {
    await env.say("慢A");
    await env.clearSelection(); // 另起一条分支
    await env.say("慢B");
    // 两个 run 同时跑；中止 A
    const userA = await env.waitNode((n) => n.kind === "user" && n.text === "慢A", "慢A user");
    const userB = await env.waitNode((n) => n.kind === "user" && n.text === "慢B", "慢B user");
    const stopBtn = await env.browser.$(`[data-testid="stop-${userA.id}"]`);
    await stopBtn.waitForExist({ timeout: 10000 });
    await stopBtn.click();
    // B 完成，A 中止（assistant 节点保留部分内容）
    await env.waitNode((n) => n.kind === "assistant" && n.text === "慢B回答完毕", "B 完成");
    const aAbort = await env.waitNode(
      (n) => n.kind === "assistant" && n.parents[0] === userA.id, "A 的中止节点"
    );
    assert.ok(aAbort.text !== "慢A回答完毕", "A 未完成全文");
    await env.shot("s5-parallel-abort");
  } finally {
    await env.stop();
  }
});

test("场景 6+7：折叠持久化 + 入口回来", { timeout: 120000 }, async (t) => {
  const env = new GuiEnv();
  await env.start();
  try {
    await env.say("问题A");
    const userA = await env.waitNode((n) => n.kind === "user" && n.text === "问题A", "userA");
    await env.waitNode((n) => n.kind === "assistant", "回答");
    // 折叠 问题A
    const collapseBtn = await env.browser.$(`[data-testid="collapse-${userA.id}"]`);
    await collapseBtn.waitForExist({ timeout: 5000 });
    await collapseBtn.click();
    const view = JSON.parse(
      fs.readFileSync(path.join(env.home, ".exterior", "view.json"), "utf8")
    );
    assert.deepEqual(view.collapsed, [userA.id], "折叠写进 view.json");
    // 回答不可见
    const hidden = await env.browser.$('//*[contains(text(), "回答A")]');
    await hidden.waitForExist({ timeout: 3000, reverse: true });
    await env.shot("s6-collapsed");
    // 重启应用：仍折叠；入口显示最近末端，点了能接着想
    await env.reload();
    const view2 = JSON.parse(
      fs.readFileSync(path.join(env.home, ".exterior", "view.json"), "utf8")
    );
    assert.deepEqual(view2.collapsed, [userA.id], "重启后折叠保留");
    await env.browser.$('[data-testid="leaves"]').waitForExist({ timeout: 10000 });
    await env.shot("s7-entry");
    // 点叶子 chip 接着想
    const chip = await env.browser.$('[data-testid^="leaf-"]');
    await chip.click();
    await env.say("问题B");
    const uB = await env.waitNode((n) => n.kind === "user" && n.text === "问题B", "接着想");
    assert.equal(uB.parents.length, 1, "接在叶子后面");
  } finally {
    await env.stop();
  }
});

test("场景 8：翻旧账（bash grep nodes.jsonl）", { timeout: 120000 }, async (t) => {
  const env = new GuiEnv();
  await env.start();
  try {
    await env.say("我关于苹果的想法");
    await env.waitNode((n) => n.kind === "assistant", "第一次回答");
    // 新根：翻旧账 → 假模型发 grep tool call
    await env.clearSelection();
    await env.say("找找我之前关于苹果的想法");
    const grepNode = await env.waitNode(
      (n) => n.kind === "bash" && n.cmd?.includes("grep"), "grep 节点"
    );
    assert.ok(grepNode.output.includes("苹果"), "grep 找到了之前的念头");
    await env.shot("s8-grep");
  } finally {
    await env.stop();
  }
});

test("场景 9：换模型", { timeout: 120000 }, async (t) => {
  const env = new GuiEnv();
  await env.start();
  try {
    // 加入第二个模型（config 每次 think 都会重读；模型列表下拉来自启动时的 get_state，需要 reload）
    const cfg = JSON.parse(
      fs.readFileSync(path.join(env.home, ".exterior", "config.json"), "utf8")
    );
    cfg.models.push({ name: "fake2", base_url: `http://127.0.0.1:${env.port}/v1`, model: "fake-2" });
    fs.writeFileSync(path.join(env.home, ".exterior", "config.json"), JSON.stringify(cfg));
    await env.reload();

    await env.say("你好");
    await env.waitNode((n) => n.kind === "assistant" && n.model === "fake", "默认模型回答");
    // 切到 fake2
    await env.browser.$('[data-testid="model-select"]').selectByAttribute("value", "fake2");
    await env.say("再聊");
    await env.waitNode((n) => n.kind === "assistant" && n.model === "fake2", "fake2 回答");
    await env.shot("s9-model-switch");
  } finally {
    await env.stop();
  }
});

test("场景 10：方向随窗口形状", { timeout: 120000 }, async (t) => {
  const env = new GuiEnv();
  await env.start();
  try {
    await env.say("问题A");
    const userA = await env.waitNode((n) => n.kind === "user" && n.text === "问题A", "userA");
    const aA = await env.waitNode((n) => n.kind === "assistant", "回答A");
    const posLR = await nodePos(env, userA.id, aA.id);
    assert.ok(posLR.child.x > posLR.parent.x, "宽窗口从左到右");
    await env.shot("s10-wide");
    // 拉成竖长条
    await env.browser.setWindowSize(500, 900);
    await env.browser.pause(800); // 等 resize + 重排
    const posTB = await nodePos(env, userA.id, aA.id);
    assert.ok(posTB.child.y > posTB.parent.y, "窄窗口从上到下");
    await env.shot("s10-tall");
  } finally {
    await env.stop();
  }
});

async function nodePos(env, parentId, childId) {
  const el = async (id) => env.browser.$(`[data-testid="node-${id}"]`);
  const rect = async (e) => {
    const el = await e;
    const loc = await el.getLocation();
    const size = await el.getSize();
    return { x: Math.round(loc.x + size.width / 2), y: Math.round(loc.y + size.height / 2) };
  };
  return { parent: await rect(el(parentId)), child: await rect(el(childId)) };
}
