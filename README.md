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

不做浏览器自动化，token 由你提供，三条路径：

```bash
dsp /login <userToken>          # 直接带 token
dsp                             # 进去后输入 /login，在界面里粘贴
DSP_TOKEN=<userToken> dsp       # 环境变量
```

token 取自 `chat.deepseek.com` 的 LocalStorage（key 是 `userToken`）。
加密格式固定不变，所以磁盘上已有的凭证可以直接复用 —— `--selftest` 就是确认这件事的。

## 快捷键

| 按键 | 作用 |
|------|------|
| `Enter` | 发送 |
| `Esc` | 退出；有选区时先取消选区；登录输入中则取消登录 |
| `Ctrl+C` | 中断生成；有选区时改为「复制选区」 |
| `右键` | 复制选区 |
| `Ctrl+T` / `Ctrl+S` | 深度思考 / 智能搜索（状态栏常驻显示当前值） |
| `Ctrl+O` | 展开或收起最近一个块（思考、exec 输出） |
| `Ctrl+↑` / `Ctrl+↓` | 跳到历史顶部 / 回到底部 |
| `滚轮` / `PgUp` / `PgDn` | 翻历史 |
| `左键拖动` | 选择文本（反显） |
| `左键单击块头` | 折叠 / 展开该块 |

## 命令

```
/help /login /logout /thinking /search /thinking-view /model /lang
/status /sessions /session /clear /goto /system-prompt
/info /browser /open /quit
```

### 环境与浏览器

`/info` 打印当前环境，`dsp --info` 是同一份内容但不进界面（贴 bug 报告方便）：

```text
环境信息
  程序版本    dsp 0.1.0 (release)
  操作系统    Ubuntu 24.04.1 LTS
  构建号      24.04
  内核        Linux 5.15.167.4-microsoft-standard-WSL2
  架构        x86_64 / linux
  主机名      DESKTOP-XXXX
  配置目录    /home/me/.pi/agent
  虚拟机      WSL2（Windows 构建 10.0.22631.4317）
  浏览器      宿主机 Chrome  /mnt/c/Program Files/Google/Chrome/Application/chrome.exe

  检测到 WSL：可指定用「容器内」还是「宿主机」的浏览器
  * [1] 宿主机 Chrome    /mnt/c/Program Files/Google/Chrome/Application/chrome.exe
    [2] 容器内 系统默认   /usr/bin/xdg-open
  输入 /browser <序号> 选定；/browser auto 交回自动
```

`虚拟机` 一行**只在识别到 WSL 时出现**，其他系统不输出。
WSL 下容器和宿主机是两套环境，容器里往往根本没装浏览器，所以列出来让你选：
`/browser 2` 选定、`/browser auto` 交回自动。Windows / macOS / 原生 Linux 直接自动识别
（Windows 查 `Program Files`，macOS 查 `/Applications`，Linux 扫 `PATH`）。

`/open [url]` 用选定的浏览器打开网页，默认 `chat.deepseek.com` —— 取 userToken 时省得手敲。

`!<命令>` 直接跑 shell，输出只打印、**不进对话上下文**（不消耗 token）：

```text
❯ !git status --short
  ▌ git status --short
   M src/ui.rs
  └ exit 0  0.8s
```

`/goto` 弹出提示词选择框（↑/↓ 选、Enter 跳转），`/goto 3` 直接跳第 3 条。
`/system-prompt` 看当前提示词，`edit` 用 `$EDITOR` 打开，`reset` 恢复默认。

## 几个实现上的选择

- **工具并行执行**：一轮里模型可以同时发多个调用（`Promise.all` 的 Rust 版：`std::thread` + `join`），
  总耗时取决于最慢的那个而非累加；结果按**完成顺序**打印、按**调用顺序**回灌模型。
- **思考折叠**：默认只占一行，`Ctrl+O` 展开回放全文。思考块**一定排在正文上面** ——
  正文是按行实时推给界面的，所以思考在「正文开始的那一刻」就收尾。
- **不发 tokio**：`reqwest::blocking` + 一个后台线程 + `mpsc`，界面线程只管画。
- **身份**：系统提示词里的身份是 `Pi-Agent`；被问到「你是谁 / 叫什么」时统一回答 `deepseek`。
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
