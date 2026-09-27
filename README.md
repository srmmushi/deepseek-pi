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
  程序版本    dsp 1.0.1-alpha (release)
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

## 联网搜索与引用

`Ctrl+S`（状态栏那个开关）打开后，模型会自己决定要不要联网。搜索**不是**一个工具调用 ——
它由服务端完成，客户端只负责把结果展示出来。响应里是这么走的（实测抓下来的）：

```text
① 先出一个 type=SEARCH 的片段，带 queries（搜了什么）
② 再用补丁 response/fragments/-1/results 下发来源数组：
   [{url, title, snippet, cite_index, site_name, site_icon, query_indexes}, …]
③ 片段自身补上 content = "搜索到 6 个网页"
④ 正文里出现 [citation:1] [citation:2] 这类引用标记，数字对应 cite_index
```

于是终端里呈现为：

```text
▌ search-web  搜索到 6 个网页 · 点击或 Ctrl+O 展开
    [1] 2026年9月26日
        http://www.joyurl.cn/calendar/date_2026_9_26.html
    [2] 今日是什么日子
        https://huangli.txcx.com/jintian-shenmerizi.html
```

- 折叠块默认收起，`Ctrl+O` 或点块头展开
- **点带网址的行 → 用系统浏览器打开**（选哪个浏览器沿用 `/browser`；终端里没有"应用内浏览器"，
  所以点链接一律是外部打开）
- 正文里的 `[citation:3]` 显示成 `[3]` —— 只改显示，**送回模型的仍是原文**
- 不是每次问答都会搜（`search_triggered` 为假时没有结果块），没搜就不显示，不留空壳

识别用的是「路径以 `/results` 结尾」+「确实是含 `url` 的对象数组」双重判断，
形状变了最坏只是不显示，不会把别的东西当来源。

## 插件

插件就是一个 zip 包，装到 `<配置目录>/plugins/<名字>/`：

```bash
dsp install my-plugin.zip     # 装在配置目录下；同名覆盖 = 升级
dsp plugins                   # 列出：[开]/[关] + 版本号 + 描述
dsp uninstall my-plugin       # 卸载
```

开关（关掉的插件不参与系统提示词）：

```bash
dsp plugins my-plugin disable   # 关掉；也收 dsp plugins disable my-plugin
dsp plugins my-plugin enable    # 打开
```

界面里是同一套：`/plugins` · `/plugins my-plugin disable` · `/plugins my-plugin enable`。
两条路共用 `plugins::apply` 一份解析与文案，不会出现「命令行能跑、界面里另一套说法」。

包结构 —— `plugin.json` 放包根，或者整体套一层目录都行：

```text
my-plugin/
├── plugin.json     { "name": "my-plugin", "version": "1.0.0", "description": "…" }
└── prompt.md       可选：内容追加进系统提示词
```

`name` 决定目录名，所以只允许字母数字与 `- _ .`，且不能以点开头。安装**先把整包
路径核完再落盘** —— 包里带 `../`、盘符或 NTFS 备用数据流的条目整包拒绝，既不会写到
插件目录之外，也不会留下半个插件。

`prompt.md` 是当前唯一的载荷类型，它追加在系统提示词的**最末尾**。放末尾是有意的：
插件提示词对每个会话都一样，放在最后不会打乱前面那段所有会话共用的前缀，缓存照样命中。
改动要**下次启动（或 `/new`）之后**才生效 —— 同一会话内的前缀是冻结的。

> 解压不依赖系统里的 `unzip`，也没多拉 zip 库：`infra/zip.rs` 自己读中央目录，
> 支持存储与 deflate 两种（deflate 借 `flate2`，它本来就在依赖树里 —— `image` 解 png 用到）。

## 项目规则：`AGENTS.md` 与 `/init`

项目根目录放 `AGENTS.md`（`CLAUDE.md` 也认，`AGENTS.md` 优先），内容会**自动并入系统
提示词的末尾** —— 每个项目一份的规则，不用每次在对话里重复交代。超过 16KB 会截断并
在提示词里写明（按字符边界切，中文不会切成乱码）；塞满上下文反而挤掉正事。

`/init` 让模型先读一遍项目（结构、README、构建配置），再写出一份 `AGENTS.md`：

```text
/init      已经有 AGENTS.md 就不动，避免覆盖你手写的内容
```

生成的是**项目特有**的东西：构建/测试/运行命令、代码风格、目录职责、已知的坑、以及
「不要做什么」。它写完的文件也走 `write` 工具，所以 `/undo` 一样能退。

## 自定义命令：一个 `.md` 就是一个命令

照抄 Claude Code（`.claude/commands/*.md`）和 OpenCode（`.opencode/command/*.md`）的做法：

```text
<项目>/.dsp/commands/review.md     项目级（同名时优先）
<配置目录>/commands/review.md       用户级
```

文件名即命令名，正文就是发给模型的提示词，`$ARGUMENTS` 换成命令后面的参数
（正文里没有 `$ARGUMENTS` 就把参数附在末尾）：

```markdown
# 审查改动
逐个看我改过的文件，指出真正的问题（不是风格偏好）。$ARGUMENTS
```

于是 `/review src/` 就等于把这段提示词发了出去。`/commands` 列出全部可用命令。

**内置命令永远优先**：查不到才轮到命令文件（判定放在命令 `match` 的最后那个分支），
所以叫 `help.md` 的命令顶不掉 `/help`，不会把人锁在外面。

## 撤销：`/undo`

借鉴 Aider 的 `/undo` —— 它的回退单位是「一次改动批次」而不是单个文件，
那才符合「模型这一轮改砸了」的实际情况。这里不依赖 git：`write` / `edit`
**落盘之前**，先把目标文件当前内容复制到 `<配置目录>/undo/<会话 id>/`。

```text
/undo        退掉最后一轮碰过的全部文件（恢复成那一轮动手之前的样子）
/undo all    退掉本会话记录到的全部改动（每个文件回到会话开始时的样子）
```

要点：

- **每次动手都留一份底**。只留第一次是不够的 —— 文件第一轮改过、第二轮又改了，
  退第二轮时它也得跟着回去；这恰恰是最需要退的情况（这个坑是单元测试抓出来的）；
- 恢复是**倒序**执行的，同一文件在一轮里被改多次也能落回正确状态；
- 只覆盖 `write` / `edit`。`exec` 里跑的 `rm`、`git checkout`、构建产物之类管不到 ——
  报错信息与 HELP 里都写明了，不含糊；
- 账本按**会话**存（换会话换一本）。`<配置目录>/undo/` 下的目录可以随时删，
  删了只是退不回去，不影响别的。

## 会话存储

启动时**默认接着「本目录上一次」的会话**继续 —— 会话按目录存，进同一个项目就该看到
同一份上下文，否则每次启动都像「存不下来」。要开新的用 `/new`；要跨目录接最近一次用 `--resume`。

系统提示词末尾带上了**当前工作目录**（模型看不到进程的 cwd，只能靠这段文字知道自己在哪个
项目里）。它放在最后是有意的：这段每个会话都不同，而前面的提示词与工具说明是所有会话共用的，
共用部分越靠前，跨会话的上下文缓存越容易命中。

换会话即换工作目录 —— 工具用的 cwd 取自会话本身，所以 `/session <序号>` 会连目录一起切。

`/session web` 列的是**本工具建过的网页端会话**（本地有绑定，也就是首轮下发过系统提示词的
那些），显示网页端起的名字与它当时的工作目录；`/session web <序号>` 进入该会话并把目录切过去。
网页端里不是本工具建的会话不列出 —— 它们没有我们的系统提示词，也没有本地记录可续。


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
