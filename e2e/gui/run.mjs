// GUI 端到端测试（SPEC §5.1/§5.2）：tauri-driver + WebdriverIO 驱动真实应用。
// 每个场景组：独立临时 HOME、假模型（另一个 URL）、真实 bash 与 nodes.jsonl、截图留证。
// 本文件是编排器；场景在 scenarios.mjs。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { remote } from "webdriverio";
import { startFakeModel } from "../kernel/driver.mjs";

const APP_BIN = process.env.APP_BIN
  ?? path.resolve(import.meta.dirname, "../../src-tauri/target/debug/exterior");
const TAURI_DRIVER = process.env.TAURI_DRIVER ?? "tauri-driver";
export const ARTIFACTS = path.resolve(import.meta.dirname, "../artifacts");

fs.mkdirSync(ARTIFACTS, { recursive: true });

let driverPort = 4444;

export class GuiEnv {
  constructor() {
    this.home = fs.mkdtempSync(path.join(os.tmpdir(), "exterior-gui-"));
    this.port = 18300 + Math.floor(Math.random() * 500);
    this.driverPort = ++driverPort;
  }

  async start(extraModels = []) {
    this.fake = startFakeModel(this.port);
    await this.fake.ready();
    fs.mkdirSync(path.join(this.home, ".exterior"), { recursive: true });
    fs.mkdirSync(path.join(this.home, "Desktop"), { recursive: true });
    fs.writeFileSync(path.join(this.home, "Desktop", "桌面文件A.txt"), "");
    fs.writeFileSync(path.join(this.home, "Desktop", "桌面文件B.txt"), "");
    const models = [
      { name: "fake", base_url: `http://127.0.0.1:${this.port}/v1`, model: "fake-1" },
      ...extraModels,
    ];
    fs.writeFileSync(
      path.join(this.home, ".exterior", "config.json"),
      JSON.stringify({ models, default: "fake" })
    );
    // tauri-driver 拉起应用（继承这里的 HOME/DSH_HOME）
    this.driver = spawn(TAURI_DRIVER, ["--port", String(this.driverPort)], {
      env: { ...process.env, HOME: this.home, DSH_HOME: path.join(this.home, ".exterior") },
      stdio: "inherit",
    });
    await this.driverReady();
    this.browser = await remote({
      hostname: "127.0.0.1",
      port: this.driverPort,
      path: "/",
      logLevel: "error",
      capabilities: {
        browserName: "wry",
        "tauri:options": { application: APP_BIN },
      },
    });
    await this.browser.setWindowSize(1200, 800);
    // 等输入框出现
    await this.browser.$('[data-testid="input"]').waitForExist({ timeout: 15000 });
    return this;
  }

  async driverReady() {
    for (let i = 0; i < 200; i++) {
      try {
        await fetch(`http://127.0.0.1:${this.driverPort}/status`);
        return;
      } catch {
        await new Promise((r) => setTimeout(r, 100));
      }
    }
    throw new Error("tauri-driver 没起来");
  }

  /** 重启应用（同一 HOME），用于折叠持久化/入口场景 */
  async reload() {
    await this.browser.reloadSession();
    await this.browser.$('[data-testid="input"]').waitForExist({ timeout: 15000 });
  }

  nodes() {
    const f = path.join(this.home, ".exterior", "nodes.jsonl");
    if (!fs.existsSync(f)) return [];
    return fs.readFileSync(f, "utf8").trim().split("\n").map(JSON.parse);
  }

  async shot(name) {
    await this.browser.saveScreenshot(path.join(ARTIFACTS, `${name}.png`));
  }

  /** 输入并发送（回车等效点击发送按钮） */
  async say(text) {
    const input = await this.browser.$('[data-testid="input"]');
    await input.setValue(text);
    await this.browser.$('[data-testid="send"]').click();
  }

  /** 等 nodes.jsonl 里出现满足条件的节点 */
  async waitNode(pred, what, timeoutMs = 30000) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const hit = this.nodes().find(pred);
      if (hit) return hit;
      await new Promise((r) => setTimeout(r, 100));
    }
    throw new Error(`等节点超时: ${what}`);
  }

  /** 等界面上出现包含文本的元素 */
  async waitText(text, timeoutMs = 30000) {
    const el = await this.browser.$(`//*[contains(text(), "${text}")]`);
    await el.waitForExist({ timeout: timeoutMs });
    return el;
  }

  async clickNode(id, { shift = false } = {}) {
    const el = await this.browser.$(`[data-testid="node-${id}"]`);
    await el.waitForExist({ timeout: 10000 });
    if (!shift) {
      await el.click();
      return;
    }
    // Shift+点击：按住 Shift 的指针点击
    await this.browser.performActions([
      { type: "key", id: "k", actions: [{ type: "keyDown", value: "\uE008" }] },
    ]);
    try {
      await el.click();
    } finally {
      await this.browser.performActions([
        { type: "key", id: "k", actions: [{ type: "keyUp", value: "\uE008" }] },
      ]);
    }
  }

  /** 点击画布空白处，清空选择（新根） */
  async clearSelection() {
    const pane = await this.browser.$(".react-flow__pane");
    await pane.click();
  }

  async stop() {
    try { await this.browser.deleteSession(); } catch {}
    this.driver?.kill();
    this.fake?.kill();
  }
}
