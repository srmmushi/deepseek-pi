# DSP (deepseek-pi) — Rust + Ratatui 版

TS 版的 Rust 重写。配置目录与加密格式和 TS 版**完全一致**，
所以在 TS 版登录过的话，这边可以直接接着用同一份凭证。

## 模块

```text
src/
├── main.rs       CLI 参数、事件循环、斜杠命令、--selftest
├── config.rs     配置目录解析 + AppConfig + models.json
├── auth.rs       scrypt(机器指纹) + AES-256-GCM 凭证读写
├── clipboard.rs  剪贴板（本地工具优先，回落 OSC 52）
├── i18n.rs       中英双语文案
├── prompt.rs     系统提示词加载与工具说明构建
├── deepseek.rs   PoW(wasmi 跑官方 sha3 WASM) + REST 客户端 + completion 编排
├── stream.rs     SSE p/o/v patch 状态机
├── tools.rs      文本工具调用解析 + write/read/list/exec/search
├── agent.rs      一轮对话：流式 → 解析 → 并行跑工具 → 回灌 → 继续
└── ui.rs         Ratatui 界面（滚动 / 折叠 / 选择 / 弹层）
```

## 在 WSL 里构建

```bash
# 1) 装工具链（只需一次）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 2) 编译
cd rust
cargo build --release            # 产物：target/release/dsp

# 3) 自检：打印机器指纹并尝试解开已有凭证（不联网）
./target/release/dsp --selftest

# 4) 运行
./target/release/dsp             # 交互模式
./target/release/dsp --resume    # 接着最近一次会话
```

依赖都是纯 Rust：`reqwest`(rustls) / `ratatui` / `crossterm` / `wasmi` / `aes-gcm` / `scrypt`。
不需要 openssl 开发包，`build-essential` 也不需要（没有 C 代码要编）。
首次编译约 2–4 分钟，之后增量编译几秒。

想全局用：`cargo install --path .`，或者
`sudo ln -s "$PWD/target/release/dsp" /usr/local/bin/dsp`。

## 登录

Rust 版不做浏览器自动化（那需要 chromiumoxide 之类的 CDP 客户端），token 由你提供，三条路径：

```bash
dsp /login <userToken>          # 直接带 token
dsp                             # 进去后输入 /login，在界面里粘贴
DSP_TOKEN=<userToken> dsp       # 环境变量
```

token 取自 `chat.deepseek.com` 的 LocalStorage（key 是 `userToken`）。
因为加密方案一致，**TS 版 `/login` 留下的凭证这边能直接解密复用**——
`--selftest` 就是用来确认这一点的。

## 快捷键

| 按键 | 作用 |
|------|------|
| `Enter` | 发送 |
| `Esc` | 退出；有选区时先取消选区；登录输入中则取消登录 |
| `Ctrl+C` | 中断生成；有选区时改为「复制选区」 |
| `右键` | 复制选区 |
| `Ctrl+T` / `Ctrl+S` | 深度思考 / 智能搜索 |
| `Ctrl+O` | 展开或收起最近一个块（思考、exec 输出） |
| `Ctrl+↑` / `Ctrl+↓` | 跳到历史顶部 / 回到底部 |
| `滚轮` / `PgUp` / `PgDn` | 翻历史 |
| `左键拖动` | 选择文本（反显） |
| `左键单击块头` | 折叠 / 展开该块 |

## 命令

```
/help /login /logout /thinking /search /thinking-view /model /lang
/status /sessions /session /clear /goto /system-prompt /quit
```

`!<命令>` 直接跑 shell，输出只打印、不进对话上下文。
`/goto` 弹出提示词选择框（↑/↓ 选、Enter 跳转），`/goto 3` 直接跳第 3 条。
`/system-prompt` 看当前提示词，`edit` 用 `$EDITOR` 打开，`reset` 恢复默认。

## 和 TS 版的差异

| 方面 | TS 版 | Rust 版 |
|------|-------|---------|
| 运行时 | Node + 自研行编辑器 | 不起 tokio：`reqwest::blocking` + 后台线程 + mpsc |
| 界面 | DECSTBM 滚动区域 + 手工光标归位 | Ratatui 整屏重绘 |
| 列偏移坑 | raw 模式下 `\n` 不回列，需要处处 `\r` | 不存在 |
| 折叠/滚动 | 自研块渲染器 + 重绘 | 同一套思路，行模型换成了 Ratatui 的 `Line` |
| 登录 | Playwright 自动抓 token | 手工提供 token（格式兼容） |

顺带说一句：TS 版折腾最久的两个坑（滚动区域 + `\n` 不回列导致的输出覆盖），
在 Ratatui 下根本不存在——排版交给框架，自己只需要保证
「一个逻辑行 = 一个屏幕行」这一条不变式。

## 已知限制

- 折行按字符数而非显示宽度，含大量中文的长行折行点会偏一点（不影响功能）
- 会话只有一层（`sessions/<id>.json`），没有 TS 版的会话列表分页
- PoW 需要联网下载官方 sha3 WASM，首次请求会慢一下；之后进程内缓存
