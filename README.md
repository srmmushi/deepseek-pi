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

`/login browser` 用自动识别出的浏览器打开 `chat.deepseek.com/sign_in`：
登录后在该页按 F12，控制台执行 `localStorage.getItem('userToken')`，再用 `/login token` 粘回来。
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
/info /open /quit
```

### 环境与浏览器

`/info` 打印当前环境，`dsp --info` 是同一份内容但不进界面（贴 bug 报告方便）：

```text
环境信息
  程序版本    dsp 0.1.0 (release)
  操作系统    Ubuntu 24.04.1 LTS
  构建号      5fdd0af
  内核        Linux 5.15.167.4-microsoft-standard-WSL2
  架构        x86_64 / linux
  主机名      DESKTOP-XXXX
  配置目录    /home/me/.pi/agent
  虚拟机      WSL2（Windows 构建 10.0.22631.4317）
```

`构建号` 是**编译这份二进制时源码所在的 git 提交**（`build.rs` 在编译期注入，
工作区有改动会标 `-dirty`）—— 报 bug 时对得上号。

`虚拟机` 一行**只在识别到 WSL 时出现**，其他系统不输出。

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
