# DSP (deepseek-pi) — Rust + Ratatui 版

TS 版的 Rust 重写。目标是把「Agent 运行时 + DeepSeek 网页版协议 + TUI」用 Rust 表达，
同时**复用同一个配置目录与同一套加密格式**，因此已有的登录凭证可以直接沿用。

## 模块结构

```text
rust/
├── Cargo.toml
└── src/
    ├── main.rs      入口、CLI 参数、Ratatui 事件循环、斜杠命令、`--selftest`
    ├── config.rs    配置目录解析（--config-dir > PI_CONFIG_DIR > ~/.pi/agent）+ AppConfig
    ├── auth.rs      AES-256-GCM + scrypt（与 TS 逐字节一致）+ 凭证读写
    ├── i18n.rs      中英双语文案
    ├── prompt.rs    系统提示词加载 + 工具说明构建
    ├── deepseek.rs  PoW（wasmi 执行官方 sha3 wasm）+ REST 客户端 + completion 编排
    ├── stream.rs    SSE p/o/v patch 状态机
    ├── tools.rs     文本工具调用解析 + write/read/list/exec/search
    ├── agent.rs     一轮对话：流式 → 解析 → **并行**执行工具 → 回灌 → 继续
    └── ui.rs        Ratatui 界面（可滚动、可折叠）
```

## 与 TS 版的关键差异

| 方面 | TS 版 | Rust 版 |
|------|-------|---------|
| 运行时 | Node + 自研行编辑器 | 无 tokio；`reqwest::blocking` + 后台线程 + mpsc |
| TUI | DECSTBM 滚动区域 + 手工光标归位 | Ratatui 每帧整屏重绘 |
| 列偏移坑 | `\n` 不回列 → 需 `\r` 归位 | 不存在（Ratatui 自己排版） |
| 折叠/滚动 | 自研块渲染器 + 重绘 | 自研行模型 + `Paragraph`，逻辑一致 |
| 加密 | Node crypto | `aes-gcm` + `scrypt`，**格式完全兼容** |

## 构建

```bash
cd rust
cargo build --release
./target/release/dsp --selftest     # 自检：打印机器指纹并尝试解密已有凭证
./target/release/dsp                # 交互模式
```

## ⚠️ 本机构建环境注意事项（重要）

本机用户名含中文（`C:\Users\srm木石`），而 Rust 的 **GNU 工具链（MinGW/binutils）
无法正确处理非 ASCII 路径**，表现为：

1. `ring` 的构建脚本：`ar: ...libring_core...a: No such file or directory`
   → 已在 `Cargo.toml` 里把 TLS 后端换成 `native-tls`（Windows 走 schannel，不需要编译 C）。
2. 过程宏链接失败、`ld.exe: cannot find ...\.rustup\...\libstd-*.rlib`
   → 工具链路径含中文。**把 rustup 目录实体复制/安装到纯 ASCII 路径**即可：
   ```powershell
   setx RUSTUP_HOME C:\rustup
   # 或把 .rustup 复制到 C:\rustup（必须实体目录，junction 无效：
   #  rustc 会解析出真实路径，-L 里仍是中文）
   ```
3. 链接阶段 `cannot find -lwinapi_*`
   → cargo 选中了 msys64 的 MinGW gcc，而它没有 `winapi` crate 需要的导入库。

**推荐做法：改用 MSVC 工具链**（MSVC 的 `link.exe` 对 Unicode 路径无问题）：

```powershell
rustup toolchain install stable-x86_64-pc-windows-msvc
# 需要先安装 Visual Studio Build Tools 的「使用 C++ 的桌面开发」工作负载
rustup default stable-x86_64-pc-windows-msvc
cargo build --release
```

若坚持 GNU 工具链：把 msys64 的 mingw 从 `PATH` 移除，并确保使用 rustup 自带的
MinGW（`rustup component add rust-mingw --toolchain stable-x86_64-pc-windows-gnu`）。

## 已移植

- 配置目录 / config.json / models.json（字段名与 TS 一致）
- 凭证加密（scrypt 机器指纹 + AES-256-GCM）—— 可直接解开 TS 版保存的 token
- PoW：下载官方 sha3 WASM 并用 wasmi 执行；按实际签名动态构造参数（兼容 i32/f64 difficulty）
- REST 客户端：伪造 UA / Origin / Referer / sec-ch-ua 系列头、保守限速、代理、WAF 识别
- SSE patch 状态机（p/o 持久化、BATCH 递归、THINK/RESPONSE 区分、FINISHED → done）
- 五个工具 + 文本解析器（含 write 的引号/转义处理）
- Agent 循环：**同一轮多个调用并行执行**，结果按调用顺序回灌
- Ratatui UI：会话记录、历史滚动（滚轮 / Ctrl+↑↓）、思考 spinner、块折叠（点击 / Ctrl+O）、
  状态栏、输入行、`!` shell 模式
- 斜杠命令：`/help /login /logout /thinking /search /thinking-view /model /lang /status /clear /goto /quit`
- `--selftest`：不联网验证「机器指纹 + 凭证解密」

## 尚未移植（下一步）

- **`/login` 浏览器自动化**：Rust 版不做 Playwright；当前从环境变量 `DSP_TOKEN` 注入，
  或先在 TS 版 `/login` 生成凭证（两者格式一致，可直接复用）
- **鼠标拖拽选择 + 右键/Ctrl+C 复制**：目前鼠标只支持滚轮与左键点击折叠
- `/goto` 的交互式选择框：目前是列出编号 + 直接输入编号跳转
- 会话持久化（`sessions/*.json`）：目前会话为内存态
- 折行按字符数而非显示宽度，含大量中文的长行折行点会略有偏差
- 自动重试的退避、回放模式（`contextMode: replay`）细节
