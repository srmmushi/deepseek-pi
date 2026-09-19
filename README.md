# pi-deepseek-web

融合 [pi](https://github.com/earendil-works/pi)（Pi Agent 终端版）与 [ds-free-api](https://github.com/NIyueeE/ds-free-api)（DeepSeek 网页版逆向 API）的能力、重写而成的**精简版 Pi Agent**：

- **只有一个供应商**：DeepSeek 网页版（`chat.deepseek.com`），启动即用，无需选择。
- **`/login` 自动抓 token**：拉起可见浏览器 → 用户正常登录 → 自动从 LocalStorage 读取凭证 → 加密落盘，**全程不需要打开 DevTools 复制任何东西**。
- **不再单独起后端服务**：DeepSeek 的调用逻辑（伪造 UA、PoW、SSE 解析）已内嵌为 TypeScript 模块。
- **零重型依赖**：运行时只依赖 `playwright`（登录）与 `undici`（可选代理）。

> ⚠️ 本项目为学习 / 自用性质的逆向实现。请勿商用，避免给 DeepSeek 官方服务器造成压力。

---

## 1. 目录结构

```text
Pi-DeepSeek-Web/
├── package.json                 # 依赖与构建脚本（bin: pi-deepseek-web）
├── tsconfig.json                # ESM + NodeNext + 严格模式
├── README.md
├── src/
│   ├── index.ts                 # CLI 入口（--help / --config-dir / 单命令模式）
│   ├── app.ts                   # 应用状态中枢（配置 + 凭证 + 客户端 + 求解器 + 历史）
│   │
│   ├── cli/
│   │   └── args.ts              # 命令行参数解析与帮助文本
│   │
│   ├── config/
│   │   ├── dirs.ts              # 配置目录解析（--config-dir > PI_CONFIG_DIR > ~/.pi/agent）
│   │   └── store.ts             # config.json / models.json 读写与默认值
│   │
│   ├── i18n/
│   │   └── index.ts             # 中英双语文案 + 系统语言探测
│   │
│   ├── auth/
│   │   ├── crypto.ts            # 基于机器标识派生密钥的 AES-256-GCM
│   │   ├── store.ts             # auth/deepseek-web.json 读写
│   │   └── login.ts             # Playwright 登录，轮询 LocalStorage 抓 userToken
│   │
│   ├── deepseek/
│   │   ├── client.ts            # REST 客户端（伪造 UA / Origin / Referer + 限速 + 代理）
│   │   ├── pow.ts               # WASM PoW（DeepSeekHashV1）求解与缓存
│   │   ├── stream.ts            # SSE p/o/v patch 状态机 → 结构化事件
│   │   ├── prompt.ts            # DeepSeek 原生 ChatML 标签提示词构建
│   │   └── provider.ts          # 单次 completion 编排（建会话→PoW→流式→清理）
│   │
│   ├── tools/
│   │   ├── types.ts             # ToolCall / ToolContext / ToolResult
│   │   ├── parser.ts            # 文本工具调用解析器（write/read/list/exec/search）
│   │   ├── fs-tools.ts          # write / read / list / search
│   │   ├── exec.ts              # exec（shell 命令）
│   │   └── index.ts             # 工具分发 + 提示词中的工具说明（双语）
│   │
│   ├── prompt/
│   │   └── system-prompt.ts     # system-prompt.md 加载 / 编辑 / 重置
│   │
│   ├── agent/
│   │   ├── loop.ts              # Agent 主循环（流式 → 解析工具 → 执行 → 回灌）
│   │   ├── commands.ts          # 斜杠命令实现
│   │   └── repl.ts              # 交互式 REPL（流式渲染 / 状态栏 / 快捷键）
│   │
│   ├── ui/
│   │   ├── banner.ts            # ASCII 字形
│   │   ├── line-editor.ts       # 自研单行编辑器（替代 node:readline）
│   │   ├── output.ts            # ANSI 配色与输出原语
│   │   ├── palette.ts           # 命令面板 / 补全提示
│   │   ├── statusbar.ts         # 固定底部状态栏（滚动区域）
│   │   └── text.ts              # 显示宽度对齐（CJK / ANSI 感知）
│   │
└── （构建输出）
    └── dist/                    # npm run build 产物
```

### 与上游项目的关系

本项目把两个上游项目的能力**重新实现为 TypeScript**，仓库中不包含它们的源码副本：

- [pi](https://github.com/earendil-works/pi)（Pi Agent 终端版）：借鉴其 Agent 循环、工具抽象与系统提示词组织方式。
  多供应商抽象与自研 TUI 未采用，改为内置单一供应商 + 自研行编辑器与底部状态栏。
- [ds-free-api](https://github.com/NIyueeE/ds-free-api)（DeepSeek 网页版逆向 API，Rust）：
  其 `ds_core` 的调用链（PoW 求解、SSE patch 状态机、原生 ChatML 提示词、伪造请求头）
  已完整移植到 `src/deepseek/*`，本仓库不需要编译 Rust 二进制。

### 关键文件职责

| 文件 | 对应 ds-free-api 的实现 | 职责 |
|------|------------------------|------|
| `src/auth/login.ts` | （新增，替代账号密码登录） | 可见浏览器登录 + LocalStorage 抓 `userToken` |
| `src/auth/crypto.ts` | （新增） | 机器标识派生密钥的 AES-256-GCM 加密 |
| `src/deepseek/client.ts` | `ds_core/src/accounts/client.rs` | REST 端点 + 伪造浏览器头 |
| `src/deepseek/pow.ts` | `ds_core/src/accounts/pow.rs` | WASM PoW（`DeepSeekHashV1`） |
| `src/deepseek/stream.ts` | `ds_core/src/chat/response.rs` | SSE patch 状态机 |
| `src/deepseek/prompt.ts` | `src/openai_adapter/request/prompt.rs` | DeepSeek 原生标签提示词 |
| `src/deepseek/provider.ts` | `ds_core/src/chat/request.rs` | 一次 completion 的完整生命周期 |
| `src/agent/loop.ts` | `packages/agent/src/agent-loop.ts`（精简） | Agent 主循环 |
| `src/agent/repl.ts` | `packages/coding-agent/src/modes/interactive`（精简） | 交互式界面 |
| `src/agent/session-store.ts` | `packages/coding-agent/src/core/agent-session.ts`（精简） | 会话持久化 + 与网页会话 1:1 绑定 |
| `src/prompt/system-prompt.ts` | `packages/coding-agent/src/core/system-prompt.ts`（精简） | 系统提示词 |

---

## 2. 快速开始

```bash
# 1) 安装依赖（跳过 Playwright 浏览器下载，默认复用本机 Chrome/Edge）
PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm install
# Windows PowerShell:
#   $env:PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1; npm install

# 2) 首次登录：重新构建后进入交互模式，然后直接输入 login
npm run build
npm start
#   进入后输入：  login        （也支持 /login；默认用 Edge 打开）

# 3) 登录后可校验凭证是否真的有效
#   输入：        /status
```

**登录行为说明**

- 默认使用 **Edge** 打开登录页；可通过环境变量覆盖：
  ```powershell
  $env:PI_LOGIN_BROWSER = "chrome"    # msedge（默认）| chrome | chromium
  ```
- 只认 LocalStorage 中**精确键名** `userToken` / `user_token`，不会再误抓其它带 token 的键。
- 抓到后会**先用一次真实鉴权请求校验**（创建会话→删除会话），
  **校验通过才算登录成功**；校验失败会继续等待你完成登录，并打印失败原因。
- 界面顶部会显示项目 ASCII banner，以及凭证状态（token 长度）。

> 若本机没有 Chrome / Edge，请安装 Chromium：`npx playwright install chromium`。

### 常用命令

```bash
pi-deepseek-web                          # 交互模式
pi-deepseek-web /login                   # 单命令模式：登录后退出
pi-deepseek-web --config-dir ./my-conf   # 自定义配置目录
PI_CONFIG_DIR=./my-conf pi-deepseek-web  # 用环境变量指定配置目录
pi-deepseek-web --help
```

---

## 3. 配置目录

优先级：`--config-dir` > 环境变量 `PI_CONFIG_DIR` > 默认 `~/.pi/agent`。

```text
~/.pi/agent/
├── auth/
│   └── deepseek-web.json     # 加密后的 token + UA（明文仅 UA 与时间戳）
├── sessions/
│   └── <uuid>.json           # 每个会话：标题 / 工作目录 / 网页会话 id / parentMessageId / 消息历史
├── config.json               # 语言 / 开关 / 模型 / UA / wasmUrl / proxy / contextMode 等
├── models.json               # 只有一个 provider：deepseek-web
└── system-prompt.md          # 可编辑的系统提示词
```

### `config.json` 字段

| 字段 | 说明 |
|------|------|
| `language` | `zh` / `en`，默认跟随系统 |
| `thinking` | 深度思考开关（对应 `thinking_enabled`） |
| `search` | 智能搜索开关（对应 `search_enabled`） |
| `model` | `deepseek-chat` / `deepseek-reasoner` |
| `userAgent` | 登录时捕获的浏览器 UA，所有请求复用 |
| `wasmUrl` | PoW WASM 地址（上游更新后需替换） |
| `apiBase` | 默认 `https://chat.deepseek.com/api/v0` |
| `clientVersion` / `clientPlatform` / `clientLocale` | 对应 `x-client-*` 请求头 |
| `proxy` | 可选，`http://` 或 `socks5://`，用于绕过 WAF 区域限制 |
| `requestIntervalMs` | 两次上游请求的最小间隔，默认 1200ms（保守限速） |
| `maxToolSteps` | 单轮最多工具调用轮数，默认 25 |
| `contextMode` | `reuse`（默认，复用网页会话只发增量）或 `replay`（每轮打包完整历史 + 一次性会话） |

---

## 4. 命令与快捷键

| 命令 | 说明 |
|------|------|
| `/help` | 帮助 |
| `/login`（或直接输入 `login`） | 用 Edge 拉起浏览器登录，抓取 token 并**校验通过后才保存** |
| `/logout`（或直接输入 `logout`） | 清除本地凭证 |
| `/thinking [on\|off]` | 切换深度思考；无参数则反转 |
| `/search [on\|off]` | 切换智能搜索；无参数则反转 |
| `/model [id]` | 查看 / 切换模型 |
| `/lang [zh\|en]` | 查看 / 切换界面语言 |
| `/system-prompt [edit\|reset]` | 查看 / 编辑 / 重置系统提示词 |
| `/new` | 新建会话（网页会话在首次发送提示词时才创建） |
| `/session` | 查看当前会话 |
| `/session all` | 列出全部会话（按最近使用排序，`*` 标记当前） |
| `/session <序号\|id前缀>` | 切换到指定会话：**同时切换工作目录与网页会话** |
| `/clear` | 重置当前会话上下文（下次发送会新建网页会话） |
| `/status` | 查看配置目录、模型、开关，并**真实校验凭证**（token 长度 + 指纹 + 接口结果） |
| `/quit` | 退出 |

快捷键：`Ctrl+T` 切深度思考，`Ctrl+S` 切智能搜索，`Ctrl+C` 中断生成（空闲时退出）。

### 界面

```text
  ██████╗ ██╗   pi-deepseek-web
  ██╔══██╗██║   Pi Agent · 仅 DeepSeek 网页版
  ██████╔╝██║   ─────────────────────────────
  ██╔═══╝ ██║   会话        新会话
  ██║     ██║   配置目录    C:\Users\…\.pi\agent
  ╚═╝     ╚═╝   凭证        ✓ 已加载（token 64 字符）· /status 可校验

  直接输入即可对话 · 输入 / 查看全部命令 · /help 查看帮助

❯ 读取 src/index.ts                          ← 自研行编辑器（历史/光标/CJK 宽度）

  ✻ 思考…
  这是一个入口文件……

  ⏺ read(src/index.ts)                       ← 工具调用
  ⎿  已读取 src/index.ts                     ← 工具结果

  · 39 tokens

❯ /                                          ← 输入 "/" 弹出全部命令面板
▸ /session /search /status /system-prompt    ← 提示行（实时补全）
◆ 新会话 · deepseek-chat · 思考 关 · 搜索 开 · zh   ← 状态栏（固定在最底部）
```

- **底部固定状态栏**：提示行 + 状态栏两行通过终端滚动区域（DECSTBM）钉在屏幕底部，
  主输出在它上方滚动，**不会把状态栏顶走**。
- **输入行紧贴状态栏上方**：启动时把光标定位到滚动区域底部，因此 `❯` 输入行始终位于
  提示行（分隔线）与状态栏的正上方，输出内容在它上方逐行堆积。
- **自研行编辑器**：不再使用 `node:readline`。readline 刷新输入行时会发出「擦除到屏幕末尾」
  (`ESC[0J`)，会把底部状态栏一起擦掉，也无法做命令面板。自研编辑器只清当前输入行
  (`ESC[2K`)，支持历史上下翻、Ctrl+A/E/U/K/W、Home/End、中文宽度感知与超长横向滚动。
- **输入 `/` 显示全部命令**：立刻把命令面板（含中英文说明）打印到输出区；
  继续输入如 `/s` 会实时筛选，提示行显示 `▸ /session /search /status /system-prompt`。
- **降级**：非 TTY（管道、重定向）或设置环境变量 `PI_UI=plain` 时自动退化为普通逐行输出。

```powershell
$env:PI_UI = "plain"   # 关闭固定状态栏与面板
```

### 会话与上下文（重要）

**pi 会话 ↔ 网页会话是 1:1 绑定的**，一个本地会话对应一个 `chat.deepseek.com` 上的会话。

| 环节 | 行为 |
|------|------|
| `/new` 新建会话 | 只写本地 `sessions/<id>.json`，**不在网页端建会话** |
| 首次发送提示词 | 才真正调用 `/chat_session/create`，拿到 `deepseekSessionId` 并落盘（**懒创建**） |
| 后续发送 | 复用同一个 `chat_session_id`，并把上一轮的 `response_message_id` 作为 `parent_message_id` 链式追加 |
| `/session all` | 列出全部会话；`/session <序号>` 切换后，**工作目录与网页会话同时切换** |
| `/clear` | 清空本地消息并解绑网页会话，下次发送会新建一个网页会话 |

**保证一个 pi 会话只对应一个网页会话。** 为此修掉了三条会产生「多余会话」的路径：

1. **回合失败/中断时绑定丢失**：之前只有流正常跑完才把 `deepseekSessionId` 落盘，
   一旦中途出错（限流、网络、Ctrl+C），下一轮会误以为还没建会话而重新创建，
   旧会话就留在了网页端。现在改为在 `finally` 中**无条件落盘**。
2. **凭证校验产生孤儿会话**：`/status`、`/login` 之前用「创建会话 + 删除会话」来校验 token，
   一旦删除失败就会留下空会话。现在改用**纯读取**的 `GET /users/current`，零副作用。
3. **空会话残留**：本轮由客户端新建、但完全没跑起来（PoW 失败、PoW 之前就异常）的会话，
   会在 `finally` 中被立即删除并解绑。

**上下文不需要"压缩后打包重发"。** 既然会话是 1:1 的，就直接复用网页会话、每轮只发**增量**，历史由服务端保存：

- 系统提示词 + 工具说明**只在会话的第一条消息**下发一次（判据是 `parentMessageId == null`），之后不再重复；
- 工具执行结果作为下一条消息回灌，前缀为 `[工具结果]`；
- 好处：token 消耗从 O(轮数²) 降到 O(轮数)，且网页端能看到与终端一致的完整对话。

> `contextMode: "replay"` 是兜底方案：每轮把完整历史重新拼成 DeepSeek 原生 ChatML prompt，发到**一次性会话**里用完即删（即 `ds-free-api` 的做法）。它最稳，但 token 消耗随轮数平方增长，且**不满足 1:1 绑定**（网页端只会看到一堆临时会话）。只有在 `reuse` 模式遇到上游行为异常时才切过去。

---

## 5. 工具调用

请求中会注入**精简**的工具说明（随语言切换），模型以「独占一行」的文本格式调用工具：

```text
write:"文件内容",文件路径      # 一次性完整写入（非追加）
read:文件路径
list:目录路径
exec:命令
search:关键词
```

`src/tools/parser.ts` 负责解析（容忍全角冒号、引号包裹、`\n` 等转义）；解析失败会把错误回灌给模型让它自我修正。相对路径以**当前工作目录**为基准。

---

## 6. 依赖变更说明

**新增（本项目的全部运行时依赖）：**

| 依赖 | 用途 |
|------|------|
| `playwright` | `/login` 拉起可见浏览器并读取 LocalStorage |
| `undici` | 仅在配置了 `proxy` 时用于 `ProxyAgent`；未配置则不加载 |

**开发依赖：** `typescript`、`@types/node`、`tsx`。

**相对 `pi` 移除的依赖（及其能力）：**

- `@earendil-works/*`（`pi-ai` / `pi-agent-core` / `pi-tui` / `chord` 等）——多供应商抽象与自研 TUI 全部去掉，改为内置单供应商 + readline。
- `photon-node`、`grok-mermaid`、`highlight.js`、`typebox`、`proper-lockfile`、`semver`、`yaml`、`diff`、`minimatch`、`ignore`、`hosted-git-info`、`cross-spawn`、`chalk`、`jiti` —— 图像处理、Mermaid 渲染、代码高亮、JSON Schema 校验、包管理、云同步/遥测等非核心能力全部去掉；配色改为内置 ANSI 实现。

**相对 `ds-free-api` 移除的运行形态：** 不再编译 Rust 二进制、不再需要 `config.toml` / `Cargo.toml` / Web 管理面板，DeepSeek 调用逻辑以 TS 模块内嵌。

---

## 7. 构建与运行

```bash
npm run typecheck   # 仅类型检查
npm run build       # tsc -> dist/
npm start           # node dist/index.js
npm run dev         # tsx 直接运行源码
```

`package.json` 的 `bin.pi-deepseek-web` 指向 `dist/index.js`，`npm link` 后可直接用 `pi-deepseek-web` 命令。

---

## 8. 架构与数据流

```text
用户输入
   │
   ▼
Agent 循环 (src/agent/loop.ts)
   │  buildPrompt(历史 + 系统提示词 + 工具说明)   ← src/deepseek/prompt.ts
   ▼
DeepSeek 编排 (src/deepseek/provider.ts)
   │  ① create_session
   │  ② create_pow_challenge → WASM 求解 → x-ds-pow-response
   │  ③ POST /chat/completion（SSE）
   │  ④ 逐块解析 → StreamEvent（think / content / done）
   │  ⑤ stop_stream + delete_session（清理）
   ▼
文本工具调用解析 (src/tools/parser.ts)
   │  write / read / list / exec / search
   ▼
工具执行 (src/tools/*) → 结果回灌为 <｜tool▁outputs▁begin｜> 段
   │
   └──► 无工具调用 → 本轮结束
```

---

## 9. 已知风险与后续可改进点

### 风险

1. **TLS 指纹不等于真实浏览器**：`ds-free-api` 使用 BoringSSL 模拟 Chrome 136 的 TLS 指纹，而 Node 的 `fetch`（undici）无法做到。若上游风控收紧到 TLS 层，请求可能被拒。**缓解**：配置非美国地区的 HTTP 代理（`config.json` 的 `proxy`），并保持保守的 `requestIntervalMs`。
2. **WASM PoW 地址会变**：`wasmUrl` 中的 hash 由上游静态资源版本决定，上游发版后可能失效。**缓解**：报错信息会明确提示更新 `wasmUrl`；未硬编码 `__wbindgen_export_0`，而是按名称动态探测导出。
3. **JS 无法按函数签名筛选 WASM 导出**：Rust 版可依赖签名匹配，TS 版只能按名称探测，并在失败时退化到「除已知符号外唯一函数」策略，兼容性略低于原实现。
4. **网页版接口属非公开接口**：字段（`fragments`、`p/o/v`、`biz_code`）随时可能变更，本项目对已知错误码做了中文提示，但无法覆盖全部情况。
5. **账号风控**：DeepSeek 对网页端有 session 级限流，累计请求过多可能被临时禁言。**缓解**：默认 1200ms 间隔 + 流式请求失败指数退避重试；建议个人低频使用。
6. **`exec` 工具可执行任意命令**：与所有编码 Agent 相同，仅在可信目录使用。
7. **`Ctrl+S` 在极少数终端可能被当作流控（XOFF）**：readline 已处于 raw 模式，通常无影响；若失效请改用 `/search on|off`。
8. **上下文由服务端保存**：`reuse` 模式下历史存在 DeepSeek 服务端。若你在网页端手动删除了该会话，本地的 `parentMessageId` 会失效 → 用 `/clear` 重新开始即可。
9. **凭证文件存在 ≠ 已登录**：`auth/deepseek-web.json` 只是一个加密容器。若你手动放入了无效 token，程序会认为"已登录"但请求会返回 `40003`。现在 `/login` 会先校验再落盘，`/status` 也可随时做真实校验；聊天时若收到 `40003`，会自动清掉本地凭证并提示重新登录。
10. **PoW 依赖 `expire_at` 字段**：服务端返回的是 snake_case（`expire_at` / `target_path`），
    必须在客户端映射为内部 camelCase，否则拼接出的 prefix 会变成 `salt_undefined_`，
    WASM 永远求不出解（表现为「WASM 未求出解」）。该映射已在 `client.createPowChallenge` 中处理，
    并在字段缺失时直接报错而不是继续发请求。

### 故障排查

| 现象 | 原因 / 处理 |
|------|-------------|
| `PoW 计算失败：WASM 未求出解` | 挑战字段映射异常（已修复）或 `wasmUrl` 过期 → 更新 `config.json` 的 `wasmUrl` |
| `凭证校验：✘ 40003` | token 失效 → `logout` 后重新 `login` |
| 请求被 202 拦截 | CloudFront WAF（美国 IP）→ 配置 `proxy` |
| 界面错乱 / 状态栏异常 | 终端不支持滚动区域 → `$env:PI_UI="plain"` |

### 可改进点

- **会话复用**：改用 `edit_message` + 复用 `chat_session_id`，减少往返并利用服务端上下文缓存。
- **自动发现 `wasmUrl`**：抓取 `chat.deepseek.com` 首页 JS，正则提取最新的 `sha3_wasm_bg.*.wasm`，免去手改配置。
- **增量历史压缩**：目前每轮重发完整 `<｜User｜>/<｜Assistant｜>` 历史，超长时可用 `ds-free-api` 的「历史文件上传」回退策略分块。
- **更强的 TLS 拟真**：接入支持 TLS 指纹自定义的 HTTP 客户端（如 `curl-impersonate` 包装）。
- **工具扩展**：当前严格保留 5 个核心工具；如需 `edit`（局部替换）可基于 `write` + diff 实现。
- **多账号轮转**：`ds-free-api` 的账号池能力未移植（单用户单账号场景下无必要）。
- **流式工具调用的增量解析**：目前等整段回答结束再解析工具调用；可在流式过程中提前识别并打断，降低延迟。

---

## 10. 许可

仅供个人学习研究使用。DeepSeek 官方 API 价格低廉，请优先支持官方服务。
