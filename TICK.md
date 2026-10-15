# TICK — 进度提醒

> 规则：每次开工前先 `git pull`（SPEC 可能更新）；每完成一项立刻勾掉并提交；做出成果就 PR + merge。
> SPEC 已更新（dfec433）：先做第 0 节 Rust 版 DeepSeek Harness minimal 内核，通过 §0.5 验收后再继续界面。参考实现已克隆到 /tmp/deepseek-harness @ d7432673。

## 里程碑

- [x] M1 脚手架 + 粗后端 + 最小前端（PR #1，场景 1/2 手动验证）——注意：SPEC 更新后后端将重写为内核+插件
- [x] M0 第 0 节内核（PR 见 feat/kernel；本地 10/10 通过 ×3，CI 待验）
  - [x] cordis 插件框架（具名服务/inject 顺序/事件总线/profile 文件）
  - [x] JSON-RPC stdio 服务（initialize、session/prompt、session/wait、working-directory get/set、shutdown；通知 session.event、session.status）
  - [x] 会话事件日志 + surface 推导（turn/step/user/assistant/tool/system/request 事件）
  - [ ] JSONL 持久化（$DSH_HOME/sessions/\<id\>/session.v4.jsonl，compression:none，重启恢复）
  - [x] LLM：OpenAI 兼容流式 + tool calling，凭据从环境变量
  - [x] 重试：llm/retry 事件 → 退避 → llm/retry-started（500/429/超时/传输错误，默认 5 次 500ms 起指数退避）
  - [x] 持久 bash：标记包裹、串行、timeoutMs 300000、maxOutputChars 16000、`[Command finished with exit code N]`、超时重置 shell
  - [x] working-directory 服务 + 工具 + runtime context 快照
  - [x] 系统提示（persona=DSH_SYSTEM_PROMPT 或默认）+ 会话标题（fallbackMaxWords 5 / 40B / 80B）
  - [x] agent-loop：turn/step、取消保留已流出文本、maxParallelToolCalls 10、fork
  - [x] jobs + timer（最小实现）
  - [x] §0.5 十项 e2e 全绿
- [ ] M2 Exterior 插件：节点 DAG 替代线性会话投影（不改内核）；GUI 接入内核
- [ ] M3 GUI 端到端测试（§5.2 十场景）+ CI

## §0.5 内核验收（每条一个 e2e）

- [x] 0.1 单轮：initialize → prompt → session.event 流 → wait 返回，文本正确
- [x] 0.2 持久 shell：cd /tmp && export X=1 后 pwd;echo $X 输出 /tmp 和 1
- [x] 0.3 超时：timeoutMs=2000 跑 sleep 10 → 超时说明 + shell 重置 + 下次正常
- [x] 0.4 输出截断：超过 maxOutputChars 按原版截断并附加说明
- [ ] 0.5 多 tool call：一次返回两个，都执行，结果按顺序回模型
- [x] 0.6 取消：中途取消，已流出文本留在日志（assistant/message interrupted:true）
- [x] 0.7 重试：先 500 后正常；日志有 llm/retry；结果正确
- [x] 0.8 恢复：重启进程同 sessionId 发消息，请求含之前历史
- [x] 0.9 profile：用户 profile 关闭插件后工具列表变化
- [x] 0.10 与原版一致：同一假服务器同一脚本，系统提示/工具 schema/消息序列与 model-visible.json 一致

## 验收场景（SPEC §5.2，GUI，待内核完成后接入）

- [ ] 1 第一个念头：空输入框 → 回答流式出现（手动已验证，待接入内核后重写 e2e）
- [ ] 2 执行：bash 节点默认折叠，展开看到输出，接着基于结果回答（手动已验证，同上）

## 验收场景（SPEC §5.2，每条一个 e2e 测试）

- [ ] 1 第一个念头：空输入框 → 回答流式出现（手动已验证，待 e2e 自动化）
- [ ] 2 执行：bash 节点默认折叠，展开看到输出，接着基于结果回答（手动已验证，待 e2e 自动化）
- [ ] 3 岔开：点靠前节点打字 → 线变网；新分支不知道另一分支的内容
- [ ] 4 合并：Shift 选两末端 → 合并节点有两个 parents，回答提到两条路
- [ ] 5 并行：两分支同时生成互不干扰；中止一个，另一个继续
- [ ] 6 折叠：折叠状态重启后保留
- [ ] 7 回来：重启显示最近末端，点击接着想
- [ ] 8 翻旧账：模型用 bash grep nodes.jsonl，搜索作为节点显示
- [ ] 9 换模型：切到第二个模型，节点上显示新模型名
- [ ] 10 方向：窗口拉成竖长条 → 从上到下排列
