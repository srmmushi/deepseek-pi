# DSP (deepseek-pi)

终端编程助手。只通过 **DeepSeek 网页版**（`chat.deepseek.com`）推理，不依赖官方 API Key ——
复刻网页端的鉴权与 PoW 流程，拿网页会话的 `userToken` 直接用。

纯 Rust + Ratatui，约 4300 行。

```text
  ██████╗ ███████╗ ██████╗   DSP  (deepseek-pi)
  ██╔══██╗██╔════╝ ██╔══██╗   已登录 · token 64 字符
  ██║  ██║███████╗ ██████╔╝
  ██║  ██║╚════██║ ██╔═══╝
  ██████╔╝███████║ ██║
  ╚═════╝ ╚══════╝ ╚═╝

❯ 同时读取 package.json 和 tsconfig.json，各用一句话概括

  ✻ 思考 1.2s · 128 字

  ▌ 思考 2.4s · 431 字 · Ctrl+O 展开
    （折叠状态只占一行，Ctrl+O 或点块头展开全文）

  ▌ read  package.json
  ▌ read  tsconfig.json
  └ read    package.json · 28 行 · 806B  7ms
  └ read    tsconfig.json · 24 行 · 611B  5ms
  · 2 个工具并行  ·  8ms

  两个文件分别描述……

  · 715 tokens  ·  119.2 tok/s  ·  6.0s

◆ 概括两个配置文件的用途 · deepseek-chat · Ctrl+T 深度思考 开 · Ctrl+S 智能搜索 关 · zh
```

## 构建

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

cargo build --release            # 产物：target/release/dsp
./target/release/dsp --selftest  # 自检：打印机器指纹并尝试解开已有凭证（不联网）
./target/release/dsp             # 交互模式
```

依赖全是纯 Rust（`reqwest`+rustls / `ratatui` / `crossterm` / `wasmi` / `aes-gcm` / `scrypt`），
**不需要 openssl-dev，也不需要 build-essential**。首次编译约 2–4 分钟。

想全局用：`cargo install --path .`，或
`sudo ln -s "$PWD/target/release/dsp" /usr/local/bin/dsp`。

## 登录

四种方式，都是 `/login` 的子命令：

```text
/login            打开登录页（同 /login browser）
/login token      手动粘贴 userToken（输入掩码显示）
/login passwd     手机号 / 邮箱 + 密码（两步输入，密码掩码）
/login wechatqr   微信扫码：二维码画在终端里，过期自动换一张，确认后自动写入凭证
```

### `/login` 会自动去浏览器里取凭证

浏览器（默认 **Edge**，其次 Chrome/Chromium）把 userToken 存在 localStorage 里，
而 localStorage 落在 `Local Storage/leveldb/` 的 LevelDB 文件里。所以 `/login` 分两步：

1. **先直接扫一遍浏览器存储** —— 如果浏览器里已经登录过，连页面都不用开，凭证直接到手
2. 没扫到 → 用 Edge 打开 `chat.deepseek.com/sign_in`，然后在后台**每 2 秒扫一次**，
   你扫码/输密码登录成功的那一刻，凭证自动被读回来（最多等 5 分钟）

于是整个流程不需要手动复制粘贴，也不用按 F12 翻控制台。

实现上没有去完整解析 LevelDB（那要几百行还得解 snappy），而是直接在原始字节里
找 `userToken` 这个键、把后面的值取出来 —— Chrome/Edge 的写入日志（`.log`）不压缩，
刚登录的记录一定在里面。已经落盘进 `.ldb` 且被 snappy 压过的旧记录可能扫不到，
那时退回 `/login token` 手动粘贴。

`DSP_TOKEN=<token> dsp` 依然可用，`/login` 时会优先采用。

加密格式固定不变，所以磁盘上已有的凭证可以直接复用 —— `--selftest` 就是确认这件事的。

### 微信扫码是怎么走的

```text
GET  chat.deepseek.com/sign_in
       └─ 从 HTML 里抠出 <img class="js_qrcode" src="/connect/qrcode/<编号>">
GET  open.weixin.qq.com/connect/qrcode/<编号>      ← 微信直接返回二维码图片
       └─ 解出二维码内容，再用 qrcode 重画成终端字符画
轮询 long.open.weixin.qq.com/connect/l/qrconnect?uuid=<编号>
       └─ 408 未扫 · 404 已扫待确认 · 405 已确认（带 wx_code）· 403 过期
       └─ 403 就换一张码重来，405 则拿 wx_code 去换 userToken
```

取编号、下图片、轮询 errcode 这三步都是按网页端真实行为实现的；**只有最后用
`wx_code` 换 `userToken` 那个路径查不到**（DeepSeek 没有公开文档），是按同类接口
的形状推测的。跑不通时错误信息会带上服务端原始响应，照着改一行即可。

二维码图片不直接缩放像素 —— 图片里模块数和白边都不确定，缩放比例一旦和模块数对不上
就扫不出来了；所以先用 `rqrr` 解出内容，再用 `qrcode` 重画，保证每个模块正好一个格。

整个流程跑在后台线程、结果通过事件回传，所以等扫码的时候界面照常响应，
不会卡死。

> `/login passwd` 用的 `/api/v0/users/login` 有第三方项目佐证，比上面的换 token 那步可靠。

## 提示词与工具调用格式

两层分工：`system-prompt.md` 你可以随便改，工具格式那层碰不到 ——

| 层 | 内容 | 位置 |
|----|------|------|
| 系统提示词 | 角色、工具选择、干活纪律、准确性、输出规范 | `system-prompt.md`（可编辑，`/system-prompt` 查看） |
| 工具说明 | 五个工具的调用格式、正反例、并行规则 | 每次请求注入，不落盘 |

系统提示词现在是九节：选工具 / 动手之前 / 写代码 / 验证 / 准确性 / 输出 / 安全与边界 / 身份 / 会话。
其中「验证」一节专门治谎报（没跑过的不许说「已通过」，无法验证就明说），
「写代码」一节治乱改（对齐既有风格、改动最小、不擅自引依赖、覆盖写必须含未改动部分）。

### 前缀要稳，缓存才命中

拼进请求最前面的是 `系统提示词 + 工具说明`，服务端的上下文缓存按**前缀逐字节**命中。
所以这段文本是刻意「冻」住的：

- 内部顺序按「几乎不变 → 可能变」排（身份与规则在前，会话相关在后），
  且**不掺任何会变的东西** —— 时间、目录、计数一律不进前缀，否则每轮都失效；
- 会话内由 `Core::system_text` 按会话 id 冻结：中途 `/system-prompt reset`
  不会让已建立的前缀作废（Reuse 模式首轮就定下来了，改了本来也不会重新下发），
  界面会提示「/new 后生效」；
- Reuse 模式下这段只在网页会话的首轮下发一次，之后由网页端自己留着 —— 前缀天然被复用。

也正因为前缀是复用的，把提示词写长是**划算**的：多出来的规则只付一次费用。

### 升级不会覆盖你的改动

写出提示词时同时落一份 `system-prompt.generated`。升级时拿它比对：
逐字相等 = 你没动过 → 换成新版；改过一个字 → 一个字节都不碰。
早期安装没有这份记录，退回内置旧版全文比对（所以代码里只留了最早那几版）。

### 格式是硬约束

模型最常失手的地方是给调用「加装饰」：列表符号、加粗、反引号、缺冒号、把说明写在调用同一行。

解析器对这些装饰**做了容错**（`- read:x`、`**read**:x`、反引号包住、`READ:x`、
全角冒号 `read：x`、`read : x` 这种空格都认）—— 判成「没命中」等于白丢一轮。
但夹在说明文字里的 `read:x` 不算调用：那多半只是在描述，误执行更糟。

解析失败时错误会**回灌给模型**（附正确格式与常见错法），它下一轮能自己改对；
同一批里只要有别的调用成功，失败的那几个也会单独说明，不会让模型以为全都跑了。

## 快捷键

| 按键 | 作用 |
|------|------|
| `Enter` | 发送 |
| `Esc` ×2 | 停止本轮（等待第二下时状态栏会提示）；单次按下：有选区先清选区、登录输入中则取消登录 |
| `Ctrl+C` ×2 | 退出程序（等待第二下时状态栏会提示）；有选区时改为「复制选区」 |
| `右键` | 复制选区 |
| `Ctrl+T` / `Ctrl+S` | 深度思考 / 智能搜索（状态栏常驻显示当前值） |
| `Ctrl+O` | 展开或收起最近一个块（思考、exec 输出） |
| `Ctrl+↑` / `Ctrl+↓` | 跳到历史顶部 / 回到底部 |
| `滚轮` / `PgUp` / `PgDn` | 翻历史 |
| `左键拖动` | 选择文本（反显） |
| `左键单击块头` | 折叠 / 展开该块 |

## 命令

```
/help /login /logout /new /session /clear /goto /thinking /search
/model /lang /status /info /open /export /system-prompt /quit
```

### 环境与浏览器

`/info` 打印当前环境，`dsp --info` 是同一份内容但不进界面（贴 bug 报告方便）：

```text
环境信息
  程序版本    dsp 0.2.1 (release)
  操作系统    Ubuntu 24.04.1 LTS
  构建号      5fdd0af
  内核        Linux 5.15.167.4-microsoft-standard-WSL2
  架构        x86_64 / linux
  主机名      DESKTOP-XXXX
  配置目录    /home/me/.pi/agent
  虚拟机      WSL2（Windows 构建 10.0.22631.4317）
  登录        alice · token eyJhbGci…9f2c（512 字符）· 指纹 3f8a91c2d4e5f607
```

`构建号` 是**编译这份二进制时源码所在的 git 提交**（`build.rs` 在编译期注入，
工作区有改动会标 `-dirty`）—— 报 bug 时对得上号。

`虚拟机` 一行**只在识别到 WSL 时出现**，其他系统不输出。

`!<命令>` 直接跑 shell，输出只打印、**不进对话上下文**（不消耗 token）：

```text
❯ !git status --short
  ▌ git status --short
   M src/tui/app.rs
  └ exit 0  0.8s
```

`/goto` 弹出提示词选择框（↑/↓ 选、Enter 跳转），`/goto 3` 直接跳第 3 条。
`/system-prompt` 看当前提示词，`edit` 用 `$EDITOR` 打开，`reset` 恢复默认。

## 会话存储

每个会话一个目录，正文是一份可直接翻看的 Markdown：

```text
~/.pi/agent/sessions/<编号>/context.md
```

文件开头是一条 HTML 注释形式的元数据（JSON：id、标题、工作目录、时间、网页会话绑定），
之后每条消息用 `<!-- msg: 角色 -->` 分隔，角色取 user / assistant / think / tool。
思考也一起存下来 —— 所以回放时能连思考一起看。

不用 `## 用户` 这类标题做分隔是有意的：模型自己写的内容里就可能有 `## `，
那样读回来会把一条消息切成两条。HTML 注释不会和正文撞车。

`/session <序号>` 会**清屏并完整回放**这份记录，不再只给一行「载入 N 条上下文」；
思考折成可展开的块，和当时看到的一样。旧版 `sessions/<id>.json` 仍会被列出，
下次保存时自动转成新结构。

界面上思考的位置也变了：**状态栏不再显示思考**（原来那儿是 spinner + 字数），
改成输出区里一个「正在思考」块 —— 默认一行，点块头或 `Ctrl+O` 展开看实时内容，
结束后就地变成 `▌ 思考 3.2s · 128 字`。流中途出错时会冻结成「思考（已中断）」，不会一直转。

## 排障

三条不进界面的诊断命令 —— 输出直接可复制，比在 TUI 里拖选省事：

```bash
dsp --selftest        # 凭证能不能解开（不联网）
dsp --dump-session    # 会话绑定：网页会话 id、按 id 查标题的结果、列表接口的原始返回（含状态码）
dsp --dump-turn       # 发一轮（关思考、开联网搜索、另开新会话）并打印原始 SSE
```

`--dump-session` 排查「会话名取不回来」是主力：它把失败点拆成三段 ——
网页会话 id 有没有绑上（没绑的话标题查询根本不会发出）、按 id 查标题成不成、
列表接口到底返回什么。第二种情况最常见，原因很可能是**返回形状和聊天接口不同**：
聊天接口是 `{code, data:{biz_data}}` 那套信封，会话列表却不一定是 ——
所以 `session_page` 走原始通道、**不套信封**，拿到 JSON 后再用宽容的 `session_list`
去找列表（直接是数组、或包在 data / biz_data 里都能吃）。

## 几个实现上的选择

- **工具并行执行**：一轮里模型可以同时发多个调用（`Promise.all` 的 Rust 版：`std::thread` + `join`），
  总耗时取决于最慢的那个而非累加；结果按**完成顺序**打印、按**调用顺序**回灌模型。
- **思考折叠**：默认只占一行，`Ctrl+O` 展开回放全文。思考块**一定排在正文上面** ——
  正文是按行实时推给界面的，所以思考在「正文开始的那一刻」就收尾。
- **不发 tokio**：`reqwest::blocking` + 一个后台线程 + `mpsc`，界面线程只管画。
- **身份**：系统提示词把「Pi-Agent」定位成**程序代号**，你的名字是 `DeepSeek` ——
  自我介绍或被问「你是谁 / 什么模型」时统一回答 `我是 DeepSeek`，不提其他名称。
- **会话名**：首条提示词回车即作为标题顶掉「新会话」，首轮结束后再向网页端取它自动起的名字覆盖。

## 已知限制

- 折行按字符数而非显示宽度，含大量中文的长行折行点会偏一点（不影响功能）
- 会话只有一层（`sessions/<id>.json` 列表），没有分页浏览
- PoW 需要联网下载官方 sha3 WASM，首次请求会慢一下；之后进程内缓存
- 不做浏览器自动化登录，token 需手工提供

## 历史

早期是一份等价的 TypeScript 实现（Node + 自研行编辑器 + DECSTBM 滚动区域），
现已删除并在 Rust 版里重写，配置目录 `~/.pi/agent` 与凭证格式完全沿用。
需要那份代码的话看 git 历史（`2b25748` 及之前）。
