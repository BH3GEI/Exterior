# Exterior v1 Spec

外装代脑。不是助手，是用户思考的外延。

**最高原则：能不写就不写。** 拿不准要不要加的东西，一律不加。实现中遇到本 spec 没覆盖的情况，先选最少代码的方案，再在 PR 里说明。

**唯一固定的约束：模型可用的工具只有 bash。** 其余部分（模型接入、会话存储、上下文组装、重试、系统提示、界面）全部是插件，可以替换、关闭或新增。

**开发顺序：先完成第 0 节（Rust 版 DeepSeek Harness minimal 内核），通过第 0 节的验收后，再开始第 1–5 节的界面工作。**

## 两件最重要的事

**1. GUI 就是产品。** 后端只是三件很薄的事，Exterior 和一个对话框的区别全在界面上：网络怎么长出来、分支怎么展开、折叠怎么收起。界面卡顿、布局乱跳、点了没反应，都等于功能没做。界面上花的功夫应该远多于后端。

**2. 端到端测试是验收的唯一标准。** 第 5 节的每个场景都必须是一个**自动化**的端到端测试：启动真实的应用，像用户一样点击和打字，检查界面上出现了什么、`nodes.jsonl` 里写了什么。单元测试可以有，但不能代替端到端测试。一个场景没有通过的端到端测试，就当它没做。

---

## 0. 第一步：Rust 版 DeepSeek Harness minimal 内核

### 0.1 目标

用 Rust 实现 DeepSeek Harness 的 `sdk-minimal` profile（`dsh --profile sdk-minimal`），行为与原版一致。Exterior 的 agent 能力全部来自这个内核，界面只是它的一个客户端。

参考实现：

- 仓库：https://github.com/deepseek-ai/deepseek-harness （编写本节时参照的版本为 `release-0.2.1-alpha.2`，commit `d7432673`）
- profile 定义：`packages/bundle/sdk-minimal/cordis.patch.yml`。该文件列出的每一行都对应本节的一个插件。
- 插件框架：`vendor/cordis`、`vendor/loader`
- 模型实际收到的请求快照：`scripts/snapshots/python-sdk-single-exe/minimal/model-visible.json`

原版各部分的 TypeScript 规模（`src/` 行数，供估算工作量）：插件框架 cordis 约 2.7k、loader 约 1.1k、agent-loop 约 2.5k、agent 约 1.7k、session 约 3.3k、JSONL 持久化约 5.0k、持久 bash 的 PTY 后端约 1.4k、bash 工具约 0.5k、JSON-RPC 服务约 0.5k、系统提示约 0.7k、重试约 0.3k。

### 0.2 插件框架

对应 cordis + loader，只实现 minimal profile 用到的部分：

- 插件以 Rust crate 形式编译进二进制。第 0 节不要求动态加载。
- 每个插件注册具名服务，并声明依赖的服务（对应 patch 文件中的 `inject`）。框架按依赖顺序启动，按逆序释放。
- 事件总线：插件可以发布和订阅事件（如 `session/event`、`agent/status`、`turn/end`）。
- profile 文件（TOML 或 YAML）决定加载哪些插件，每行包含 `id`、插件名、`config`、`disabled`。启动时读取，用户可用自己的 profile 文件覆盖、关闭或新增行。

### 0.3 插件清单

与 `cordis.patch.yml` 一一对应：

| 插件 | 原版包 | 需要实现的行为 |
|---|---|---|
| JSON-RPC 服务 | `dsh-sdk-jsonrpc-server`、`dsh-sdk-protocol` | stdio 上的换行分隔 JSON-RPC 2.0。请求：`initialize`、`session/prompt`、`session/wait`、`session/working-directory/get`、`session/working-directory/set`、`shutdown`。通知：`session.event`、`session.status`。stdout 只输出 JSON-RPC 帧 |
| 模型接入 | `dsh-llm`、`dsh-llm-deepseek-api-key`、`dsh-deepseek-llm-api-extensions` | 流式调用 + tool calling。原版只接 DeepSeek，本实现接 OpenAI 兼容的 Chat Completions 接口，DeepSeek 作为其中一种配置。凭据从环境变量读取 |
| 重试 | `dsh-llm-retry` | 在 agent step 边界重试失败的模型请求，重试记录先写入会话日志再退避 |
| 会话 | `dsh-session`、`dsh-session-projection` | 只追加的事件日志，模型看到的历史由日志推导。支持 fork |
| JSONL 持久化 | `dsh-session-persistence-jsonl` | 每个会话一个 JSONL 文件，`compression: none`。进程重启后可按 `sessionId` 恢复 |
| 会话标题 | `dsh-session-title` | 按原版配置生成标题（`fallbackMaxWords: 5` 等） |
| 系统提示 | `dsh-system-prompt` | 组装系统提示和工具 schema。默认 persona 取 `DSH_SYSTEM_PROMPT`，未设置时为 `You are a helpful software engineer assistant.` |
| agent 与循环 | `dsh-agent`、`dsh-agent-loop` | 调用模型 → 执行工具 → 重复。`maxParallelToolCalls` 默认 10。取消时保留已经流式输出的文本 |
| 持久 bash | `dsh-tool-bash-persistent`、`dsh-terminal`、`dsh-terminal-bash`、`dsh-subprocess-local` | 每个 agent 一个持久的 PTY bash，cwd、环境变量、函数、后台任务跨调用保留；同一 agent 的命令串行执行。`timeoutMs` 默认 300000，超时后关闭该 shell 并告知模型下次调用从新 shell 开始。`maxOutputChars` 默认 16000。每条命令结束后追加 `[Command finished with exit code N]`。工具描述文本与原版相同 |
| 工作目录 | `dsh-working-directory`、`dsh-tool-working-directory` | 会话级工作目录及 `working_directory` 工具 |
| 文件系统、沙箱策略 | `dsh-fs-local`、`dsh-sandbox-local`、`dsh-sandbox-policy` | 只需 `danger-full-access` 模式，不做任何限制 |
| MCP 资源 | `dsh-mcp-resources` | 未配置 MCP 服务器时不产生任何工具和提示文本。第 0 节只要求这一行为 |
| 后台任务、定时器 | `dsh-jobs-local`、`cordis-plugin-timer` | 按原版行为实现 |

### 0.4 Exterior 默认 profile

复刻版在 `sdk-minimal` profile 下必须带上述全部插件，用于和原版比对。Exterior 自己的默认 profile 在此基础上关闭 `working_directory` 工具，使模型可用的工具只有 bash。

### 0.5 第 0 节的验收

全部为端到端测试：启动编译好的二进制，通过 stdio 发送 JSON-RPC，模型使用本地假服务器（另一个 URL），bash 和 JSONL 使用真实环境，每个测试使用独立的临时目录。

1. **单轮**：`initialize` → `session/prompt` → 收到 `session.event` 流 → `session/wait` 返回，最终文本正确。
2. **持久 shell**：第一次调用 `cd /tmp && export X=1`，第二次调用 `pwd; echo $X`，输出 `/tmp` 和 `1`。
3. **超时**：把 `timeoutMs` 配置为 2000，执行 `sleep 10`。2 秒左右返回超时说明，shell 被重置，下一次调用正常执行。
4. **输出截断**：输出超过 `maxOutputChars` 时按原版方式截断并附加说明。
5. **多个 tool call**：一次返回两个 tool call，两个都执行，结果按顺序回到模型。
6. **取消**：生成中途取消，已流式输出的文本保留在会话日志中。
7. **重试**：假服务器先返回一次 500，再返回正常结果。会话日志中有重试记录，最终结果正确。
8. **恢复**：结束进程后重新启动，用同一个 `sessionId` 发送新消息，模型收到的请求包含之前的历史。
9. **profile**：在用户 profile 中关闭某个插件（如 `working_directory`），模型请求的工具列表中不再出现该工具。
10. **与原版一致**：用同一个假服务器、同一份对话脚本，分别运行原版 `dsh --profile sdk-minimal` 和本实现，记录假服务器收到的请求。两边的系统提示、工具 schema、消息序列必须一致（忽略 id、时间戳等每次运行都会变化的字段）。另外与仓库中的 `model-visible.json` 快照比对。

第 0 节的 10 项全部通过后，才开始第 1–5 节。

---

## 1. 技术栈

- 桌面应用：Tauri 2
- 后端：Rust（在 Tauri 进程内，不另起服务）
- 前端：TypeScript + React。图布局建议用 React Flow + elkjs/dagre，可替换
- v1 只做 macOS 桌面。手机端不在 v1 里，但布局必须能按窗口形状切换方向（见 4.2），以后手机只是另一个客户端

## 2. 数据

### 2.1 节点

一个节点就是一个念头。**没有 session、没有会话、没有项目。** 只有节点。

```json
{"id":"01J...","parents":["01J..."],"kind":"user","text":"...","created_at":"2026-10-14T10:00:00Z"}
```

| 字段 | 说明 |
|---|---|
| `id` | ULID 或 UUIDv7（可按时间排序） |
| `parents` | 来路，**数组**。`[]` 表示根节点；多个表示合并 |
| `kind` | `user` / `assistant` / `bash` |
| `text` | user、assistant 的正文 |
| `model` | 仅 assistant：生成它的模型名 |
| `tool_call` | 仅 assistant，可选：`{"id":"...","cmd":"..."}`，表示它要求执行 bash |
| `cmd` / `output` / `exit_code` / `call_id` | 仅 bash：命令、合并后的 stdout+stderr、退出码、对应的 tool_call id |
| `created_at` | 创建时间 |

节点**不可变**。不编辑、不删除。

### 2.2 存储

```
~/.exterior/
├── nodes.jsonl    # 所有节点，一行一个，只追加
├── view.json      # 界面状态：哪些节点被折叠
└── config.json    # 模型配置
```

- 启动时读入整个 `nodes.jsonl` 进内存。
- 生成中的 assistant 节点在完成（或被中止）时才写入；bash 节点在命令结束（或被中止）时写入。
- 不要数据库。

### 2.3 config.json

```json
{
  "models": [
    {"name": "claude", "base_url": "https://...", "api_key_env": "ANTHROPIC_API_KEY", "model": "..."},
    {"name": "qwen-local", "base_url": "http://localhost:11434/v1", "model": "qwen3"}
  ],
  "default": "claude"
}
```

只支持 OpenAI 兼容的 Chat Completions 接口（流式 + tool calling）。换模型 = 换 URL。

## 3. 后端

后端由第 0 节的内核加 Exterior 专用插件组成。Exterior 专用的部分只有一项：用节点 DAG 替代内核默认的线性会话，按 3.1 的规则组装模型看到的历史。这一部分作为替换 `dsh-session-projection` 对应插件的新插件实现，不修改内核。

### 3.1 上下文 = 来路

给模型的消息只来自当前节点的祖先，不看任何别的节点。

1. 收集目标节点的全部祖先（沿 `parents` 回溯到所有根）。
2. 排序：**先共同部分，再按 `parents` 的顺序依次放每条分支**，同一条分支内部按拓扑序。每个节点只出现一次。
3. 遇到合并节点（`parents.length > 1`）时，在每条分支开头插入一条说明，例如 `[来路 1/2]`、`[来路 2/2]`，让模型知道这是并列的几条路，不是先后接着说的。
4. 映射：`user` → user 消息；`assistant` → assistant 消息（带 `tool_call` 的转成 tool_calls）；`bash` → tool 消息。
5. bash 输出超过 16k 字符时，只放头尾，中间用 `[...省略 N 字符...]` 代替。完整内容仍在 JSONL 里。
6. 最前面放一段固定的系统提示（见 3.4）。

### 3.2 一次思考

用户在某处输入文字后：

1. 写入 user 节点，`parents` = 用户选中的节点（可以是 0 个、1 个或多个）。
2. 以它为终点组装上下文，流式调用 LLM。
3. 模型返回文本：生成 assistant 节点，挂在上一个节点下面，结束。
4. 模型要求执行 bash：生成带 `tool_call` 的 assistant 节点，再执行命令，结果作为 bash 节点挂在它下面；然后以 bash 节点为终点回到第 2 步。
5. 一次返回多个 tool call 时，按顺序依次执行，每个 bash 节点挂在前一个下面。

多条分支可以**同时**在跑，互不影响。

### 3.3 bash

- 唯一的工具。`bash -lc "<cmd>"`，工作目录为 `$HOME`。
- 由第 0 节的持久 bash 插件提供，行为以第 0 节为准（包括 300 秒超时）。
- **没有沙箱、没有确认、没有黑名单。** 不要写任何检查命令的代码。
- 输出边跑边推给前端。
- 用户可以中止：杀掉进程组，已有输出照常写入节点，`exit_code` 记为中止。

### 3.4 系统提示

短，固定写在代码里，大意是：

> 你是用户思考的一部分，不是助手。你可以用 bash 访问整台电脑。所有过去的念头都在 `~/.exterior/nodes.jsonl` 里，需要时自己去找。

### 3.5 不做的事

- 记忆系统、索引、向量检索、自动找相关节点
- 自我保护、看门狗：进程挂了就重启，数据都在 JSONL 里
- 任何 bash 之外的模型工具

## 4. 前端

### 4.1 入口

打开时显示：

- **最近活跃的几个末端节点**（没有子节点的节点，按时间倒序，约 5 个）。点一下就从那里接着想。
- **一个空输入框**。在这里打字，就生成一个新的根节点。

没有会话列表，没有"新建会话"按钮。

### 4.2 网络

- 当前看到的部分是**一条线**时，就像普通的聊天流一样显示。
- 一旦出现分支或合并，**自然展开成一张网**（DAG）。不需要用户切换模式。
- 方向跟着窗口形状走：**宽窗口从左到右，窄窗口（高 > 宽）从上到下。**
- 可以平移、缩放。新节点出现时，视图跟随过去。

### 4.3 节点操作

| 操作 | 结果 |
|---|---|
| 点击节点 | 选中它，输入框接在它后面，打字就从这里长出新念头 |
| Shift + 点击 多个节点（触屏：长按多选） | 同时选中多个，打字就生成一个**合并节点**，`parents` 按选中顺序排列 |
| 折叠节点 | 把它下面的部分收起来。DAG 里，一个节点只有在**所有**来路都被收起时才隐藏。折叠状态存进 `view.json` |
| 中止 | 正在生成或执行的节点上有停止按钮 |
| 选模型 | 输入框旁边一个下拉框，默认上一次用的模型。assistant 节点上显示模型名 |

带 `tool_call`、还没有 bash 子节点的 assistant 节点**不能**作为来路（否则 tool call 没有结果，上下文不合法）。点它时自动改为选中它的 bash 子节点。

### 4.4 bash 输出

后端只给原始输出，**解析和渲染都在前端**。

- bash 节点**默认折叠**，只显示命令、退出码和前几行输出。点击展开。
- v1 只需要这几种渲染：ANSI 颜色、JSON 格式化、diff 高亮。其余用等宽原文显示。
- 渲染器就是"输出 → 组件"的纯函数，按命令或内容识别，方便以后再加。
- 长输出要虚拟滚动，不能卡住界面。

### 4.5 不做的事

- 文件管理器、文件选择器
- 确认弹窗
- 会话列表
- 设置页（直接改 `config.json`）

## 5. 验收

### 5.1 怎么测

- **驱动真实的 GUI。** 测试启动打包后的 Tauri 应用，通过界面操作（点击、Shift 点击、打字、拖动窗口大小），不直接调后端函数。注意 `tauri-driver` 不支持 macOS，可以在 Linux CI 上用 `tauri-driver` + WebdriverIO 跑；选别的方案需在 PR 里说明。
- **假模型就是另一个 URL。** 写一个本地的 OpenAI 兼容假服务器，按脚本返回文本和 tool call，在测试用的 `config.json` 里指向它。**应用代码里不能有任何测试专用分支。**
- **bash 和 JSONL 用真的。** 每个测试用独立的临时 `HOME`。
- **两边都要检查：** 界面上看到的节点、连线、折叠状态、排列方向，以及 `nodes.jsonl` 里的 `parents` 和内容。
- **留下画面。** 每个场景在关键步骤截图或录屏，CI 保存成产物。改了界面的 PR 必须附上截图，方便人看。
- 所有端到端测试在 CI 上通过，才能合并。

### 5.2 场景

按下面的顺序走一遍，每一步都要符合描述。每一条都是一个端到端测试：

1. **第一个念头**：全新安装后打开，只有一个空输入框。输入一个问题，看到回答以一条线流式出现。
2. **执行**：让它"看看我桌面上有什么"。出现一个折叠的 bash 节点，展开后能看到 `ls` 的输出；它接着基于结果回答。
3. **岔开**：点击这条线上靠前的某个节点，接着打字。界面从一条线变成一张网，新分支的回答不知道另一条分支后面说过什么。
4. **合并**：Shift 选中两条分支的末端，输入"结合一下这两条"。生成的节点同时连着两条来路，回答里两条路的内容都提到了。
5. **并行**：两条分支同时在生成，互不干扰。中止其中一个，另一个继续。
6. **折叠**：折叠某个分支，关掉应用再打开，它还是折叠的。
7. **回来**：重启应用，入口显示最近的末端节点，点一个能接着想。
8. **翻旧账**：在新的根节点里说"找找我之前关于 X 的想法"。它用 bash grep `nodes.jsonl` 找到了，这次搜索作为节点显示出来。
9. **换模型**：在 config 里加一个本地模型（Ollama），切过去，回答的节点上显示新模型名。
10. **方向**：把窗口拉成竖长条，网络改为从上到下排列。

## 6. 规模

不设行数目标。第 0 节以与原版行为一致为准；Exterior 专用插件应保持小，超过 1000 行时先检查哪些部分可以不写。

## 7. 还没定的（不要在 v1 里做）

- loop：讨论和干活的节奏不同，也许只是不同的系统提示
- 多模态输入（语音、截图、草图）
- 手机端怎么连到电脑上的后端
