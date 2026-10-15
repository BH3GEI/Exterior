// 内核 e2e 驱动：拉起编译好的 exterior-kernel，走 stdio JSON-RPC（SPEC §0.5 怎么测）
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

export const KERNEL_BIN = process.env.KERNEL_BIN
  ?? path.resolve(import.meta.dirname, "../../kernel/target/debug/exterior-kernel");
export const FAKE_SERVER = path.resolve(import.meta.dirname, "../fake-model/server.mjs");

export function tmpHome() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "exterior-e2e-"));
}

export function startFakeModel(port) {
  const log = path.join(os.tmpdir(), `fake-requests-${port}.log`);
  fs.rmSync(log, { force: true });
  const out = fs.openSync(log + ".out", "w");
  const proc = spawn("node", [FAKE_SERVER, String(port), log], { stdio: ["ignore", out, out] });
  return {
    log,
    proc,
    kill: () => proc.kill(),
    requests: () => {
      try {
        return fs.readFileSync(log, "utf8").trim().split("\n").filter(Boolean).map(JSON.parse);
      } catch {
        return [];
      }
    },
    async ready() {
      for (let i = 0; i < 100; i++) {
        try {
          await fetch(`http://127.0.0.1:${port}/`, { method: "GET" });
          return;
        } catch {
          await new Promise((r) => setTimeout(r, 50));
        }
      }
      throw new Error("fake model 没起来");
    },
  };
}

export class KernelClient {
  constructor(home, profile = "sdk-minimal") {
    this.home = home;
    fs.mkdirSync(path.join(home, ".dsh"), { recursive: true });
    this.proc = spawn(KERNEL_BIN, ["--profile", profile], {
      env: { ...process.env, HOME: home, DSH_HOME: path.join(home, ".dsh") },
      stdio: ["pipe", "pipe", "inherit"],
    });
    this.buf = "";
    this.pending = new Map();
    this.notifications = [];
    this.nextId = 1;
    this.exited = new Promise((resolve) => {
      this.proc.on("exit", () => {
        for (const [, p] of this.pending) p.reject(new Error("kernel 进程退出"));
        this.pending.clear();
        resolve();
      });
    });
    this.proc.stdout.on("data", (c) => {
      this.buf += c.toString();
      let i;
      while ((i = this.buf.indexOf("\n")) >= 0) {
        const line = this.buf.slice(0, i);
        this.buf = this.buf.slice(i + 1);
        if (!line.trim()) continue;
        const msg = JSON.parse(line);
        if (msg.id !== undefined && (msg.result !== undefined || msg.error !== undefined)) {
          const p = this.pending.get(msg.id);
          if (p) {
            this.pending.delete(msg.id);
            msg.error ? p.reject(new Error(msg.error.message)) : p.resolve(msg.result);
          }
        } else if (msg.method) {
          this.notifications.push(msg);
        }
      }
    });
  }

  request(method, params, timeoutMs = 30000) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} 超时`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (v) => { clearTimeout(timer); resolve(v); },
        reject: (e) => { clearTimeout(timer); reject(e); },
      });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    });
  }

  initialize(cwd, provider = "fake", model = "fake-1") {
    return this.request("initialize", { cwd, provider, model });
  }

  prompt(sessionId, text) {
    return this.request("session/prompt", { sessionId, contentBlocks: [{ type: "text", text }] });
  }

  wait(sessionId) {
    return this.request("session/wait", { sessionId });
  }

  shutdown() {
    return this.request("shutdown", {});
  }

  /** 等到出现满足条件的通知 */
  async untilNotification(pred, timeoutMs = 15000) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const hit = this.notifications.find(pred);
      if (hit) return hit;
      await new Promise((r) => setTimeout(r, 20));
    }
    throw new Error("等通知超时");
  }

  sessionEvents(sessionId) {
    return this.notifications
      .filter((n) => n.method === "session.event" && n.params.sessionId === sessionId)
      .map((n) => n.params.event);
  }

  sessionLog(sessionId) {
    const file = path.join(this.home, ".dsh", "sessions", sessionId, "session.v4.jsonl");
    return fs.readFileSync(file, "utf8").trim().split("\n").map(JSON.parse);
  }

  async close() {
    try { await this.shutdown(); } catch { /* already gone */ }
    this.proc.kill();
  }
}

export async function withKernel(t, fn, profile = "sdk-minimal") {
  const home = tmpHome();
  fs.mkdirSync(path.join(home, ".dsh"), { recursive: true });
  const port = 18300 + Math.floor(Math.random() * 500);
  const fake = startFakeModel(port);
  await fake.ready();
  fs.writeFileSync(
    path.join(home, ".dsh", "config.json"),
    JSON.stringify({ providers: [{ name: "fake", base_url: `http://127.0.0.1:${port}/v1` }] })
  );
  const client = new KernelClient(home, profile);
  try {
    await fn({ home, fake, client, port });
  } finally {
    await client.close();
    fake.kill();
  }
}
