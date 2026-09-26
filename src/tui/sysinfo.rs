//! 运行环境识别：系统 / 架构 / 构建号、WSL 判定、浏览器探测与打开。
//!
//! 只服务于 `/info`、`/browser`、`/open`，不参与对话流程。
//!
//! 浏览器这一块是重点：WSL 下同时能看见「容器里的浏览器」和
//! 「Windows 宿主机里的浏览器」，两者差别很大（容器里往往根本没装），
//! 所以列出来让用户选；其他系统直接自动识别即可。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::Lang;

/// 当前运行环境
pub struct EnvInfo {
    pub app_version: String,
    pub os_name: String,
    /// 构建号：编译时那份源码在 GitHub 上的提交短哈希（build.rs 注入）
    pub commit: String,
    /// 内核（`uname -sr`）
    pub kernel: String,
    pub arch: String,
    pub host: String,
    /// Some("WSL2") 表示跑在 WSL 里；不是 WSL 就是 None
    pub wsl: Option<String>,
    /// WSL 下 Windows 自己的构建号
    pub windows_build: Option<String>,
}

/// 一个可用的浏览器
#[derive(Clone)]
pub struct Browser {
    pub id: String,
    pub label: String,
    pub path: PathBuf,
    /// true = Windows 宿主机里的浏览器（只可能是 WSL 下的产物）
    pub host: bool,
}

/// Windows 上的常见浏览器：id、名字、是否装在 Program Files (x86)、相对路径
const WIN_BROWSERS: &[(&str, &str, bool, &str)] = &[
    ("chrome", "Chrome", false, "Google/Chrome/Application/chrome.exe"),
    ("edge", "Edge", true, "Microsoft/Edge/Application/msedge.exe"),
    ("firefox", "Firefox", false, "Mozilla Firefox/firefox.exe"),
    ("brave", "Brave", false, "BraveSoftware/Brave-Browser/Application/brave.exe"),
];

/// Linux / 容器里的常见浏览器命令
const UNIX_BROWSERS: &[(&str, &str)] = &[
    ("google-chrome", "Chrome"),
    ("google-chrome-stable", "Chrome"),
    ("chromium", "Chromium"),
    ("chromium-browser", "Chromium"),
    ("microsoft-edge", "Edge"),
    ("brave-browser", "Brave"),
    ("firefox", "Firefox"),
];

/// macOS 的浏览器不是命令行，只能给 .app 里的可执行文件完整路径
const MAC_BROWSERS: &[(&str, &str, &str)] = &[
    ("chrome", "Chrome", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
    ("edge", "Edge", "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
    ("firefox", "Firefox", "/Applications/Firefox.app/Contents/MacOS/firefox"),
    ("brave", "Brave", "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
];

// ── 环境信息 ─────────────────────────────────────────────────

pub fn collect() -> EnvInfo {
    let wsl = wsl_version();
    EnvInfo {
        app_version: format!(
            "{} {} ({})",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
            if cfg!(debug_assertions) { "debug" } else { "release" }
        ),
        os_name: os_name(),
        commit: env!("DSP_GIT_HASH").to_string(),
        kernel: kernel(),
        arch: format!("{} / {}", std::env::consts::ARCH, std::env::consts::OS),
        host: hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string()),
        windows_build: wsl.as_ref().and_then(|_| ver_build()),
        wsl,
    }
}

/// WSL 判定。只认 WSL：内核串里带 microsoft/wsl 就算。
pub fn wsl_version() -> Option<String> {
    if let Ok(text) = std::fs::read_to_string("/proc/version") {
        let lower = text.to_lowercase();
        if lower.contains("microsoft") || lower.contains("wsl") {
            return Some(if lower.contains("wsl2") { "WSL2" } else { "WSL1" }.to_string());
        }
    }
    // 兜底：这个环境变量只有 WSL 会设
    std::env::var("WSL_DISTRO_NAME").ok().map(|_| "WSL".to_string())
}

fn os_name() -> String {
    if cfg!(target_os = "windows") {
        // 具体版本号单独给「构建号」一行，这里不重复
        return "Windows".to_string();
    }
    if let Ok(text) = std::fs::read_to_string("/etc/os-release") {
        if let Some(v) = os_release_field(&text, "PRETTY_NAME") {
            return v;
        }
    }
    std::env::consts::OS.to_string()
}

fn kernel() -> String {
    if cfg!(unix) {
        if let Ok(out) = Command::new("uname").arg("-sr").output() {
            if out.status.success() {
                return String::from_utf8_lossy(&out.stdout).trim().to_string();
            }
        }
    }
    String::new()
}

/// 取 os-release 里的某个字段，顺手去掉引号
fn os_release_field(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .map(|v| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
}

/// Windows 版本号。WSL 下靠 cmd.exe 问宿主机，输出形如
/// `Microsoft Windows [Version 10.0.22631.4317]`
fn ver_build() -> Option<String> {
    let exe = if wsl_version().is_some() { "cmd.exe" } else { "cmd" };
    let out = Command::new(exe).args(["/c", "ver"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let start = text.find("[Version ")? + "[Version ".len();
    let end = text[start..].find(']')? + start;
    Some(text[start..end].trim().to_string())
}

// ── 浏览器 ───────────────────────────────────────────────────

/// 探测可用的浏览器。WSL 下既找容器里的，也找 `/mnt/c` 下宿主机的。
pub fn detect_browsers() -> Vec<Browser> {
    let mut out = Vec::new();
    let in_wsl = wsl_version().is_some();

    // 1) 当前系统 / 容器里的浏览器
    for (cmd, label) in UNIX_BROWSERS {
        if let Some(path) = which(cmd) {
            out.push(Browser {
                id: (*cmd).to_string(),
                label: (*label).to_string(),
                path,
                host: false,
            });
        }
    }
    if out.is_empty() {
        // 容器里没装浏览器也没关系，把 URL 交给系统默认处理
        for (cmd, label) in [("xdg-open", "系统默认"), ("wslview", "宿主机默认")] {
            if let Some(path) = which(cmd) {
                out.push(Browser {
                    id: cmd.to_string(),
                    label: label.to_string(),
                    path,
                    host: false,
                });
            }
        }
    }
    if cfg!(target_os = "macos") {
        for (id, label, path) in MAC_BROWSERS {
            let p = PathBuf::from(*path);
            if p.exists() {
                out.push(Browser {
                    id: (*id).to_string(),
                    label: (*label).to_string(),
                    path: p,
                    host: false,
                });
            }
        }
    }

    // 2) Windows 里的浏览器：WSL 下走 /mnt/c，原生 Windows 走 Program Files
    for (id, label, x86, rel) in WIN_BROWSERS {
        let root = if in_wsl {
            Some(PathBuf::from(if *x86 {
                "/mnt/c/Program Files (x86)"
            } else {
                "/mnt/c/Program Files"
            }))
        } else if cfg!(target_os = "windows") {
            program_files(*x86)
        } else {
            None
        };
        let Some(root) = root else { continue };
        let path = root.join(*rel);
        if path.exists() {
            out.push(Browser {
                id: format!("host:{id}"),
                label: if in_wsl {
                    format!("宿主机 {label}")
                } else {
                    (*label).to_string()
                },
                path,
                host: in_wsl,
            });
        }
    }

    out
}

/// 按保存的取值找一个浏览器。支持：空/`auto`、序号（从 1 开始，与 /info 一致）、id
pub fn resolve(list: &[Browser], choice: &str) -> Option<usize> {
    let choice = choice.trim();
    if choice.is_empty() || choice == "auto" {
        return prefer_auto(list);
    }
    if let Ok(n) = choice.parse::<usize>() {
        return if n >= 1 && n <= list.len() { Some(n - 1) } else { None };
    }
    list.iter().position(|b| b.id == choice)
}

/// 自动选择。默认 **Edge**：Windows/WSL 下几乎必然装得有，
/// 而且和浏览器里已有的 DeepSeek 登录态是同一套；其次才是宿主机浏览器。
fn prefer_auto(list: &[Browser]) -> Option<usize> {
    list.iter()
        .rposition(|b| b.label.contains("Edge"))
        .or_else(|| list.iter().position(|b| b.host))
        .or(if list.is_empty() { None } else { Some(0) })
}

/// 用指定浏览器打开 URL。
///
/// `app_window` 为真时走 `--app=<url>`：Edge / Chrome 会起一个没有地址栏
/// 和标签页的独立窗口，等于给登录页单开一个「登录框」。
pub fn open_url(browser: &Browser, url: &str, app_window: bool) -> std::io::Result<()> {
    let mut command = Command::new(&browser.path);
    if app_window {
        command.arg(format!("--app={url}"));
    } else {
        command.arg(url);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// 关掉用完的浏览器。
///
/// WSL 下浏览器跑在 Windows 那边，借 interop 调 taskkill.exe；原生 Windows
/// 用 taskkill；Linux / macOS 用 pkill 按进程名匹配。失败无所谓，不影响已拿到
/// 的凭证。
pub fn close_browser(browser: &str) {
    let edge = browser.contains("Edge");
    let (program, name) = if cfg!(target_os = "windows") {
        ("taskkill", if edge { "msedge.exe" } else { "chrome.exe" })
    } else if cfg!(target_os = "macos") {
        ("pkill", if edge { "Microsoft Edge" } else { "Google Chrome" })
    } else if Path::new("/mnt/c/Windows").is_dir() {
        ("taskkill.exe", if edge { "msedge.exe" } else { "chrome.exe" })
    } else {
        ("pkill", if edge { "microsoft-edge" } else { "google-chrome" })
    };

    let mut command = Command::new(program);
    if program.starts_with("taskkill") {
        command.args(["/IM", name, "/F"]);
    } else {
        command.args(["-f", name]);
    }
    let _ = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 一行描述（给 /info 与 /browser 用）
pub fn describe(browser: &Browser, in_wsl: bool, lang: Lang) -> String {
    let zh = lang == Lang::Zh;
    let kind = match (browser.host, in_wsl, zh) {
        (true, _, true) => "宿主机",
        (true, _, false) => "host",
        (false, true, true) => "容器内",
        (false, true, false) => "container",
        (false, false, true) => "本机",
        (false, false, false) => "local",
    };
    format!("{kind} {}  {}", browser.label, browser.path.display())
}

/// 在 PATH 里找一个可执行文件
fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(cmd))
        .find(|p| p.is_file())
}

fn program_files(x86: bool) -> Option<PathBuf> {
    let key = if x86 { "ProgramFiles(x86)" } else { "ProgramFiles" };
    std::env::var(key).ok().filter(|v| !v.is_empty()).map(PathBuf::from)
}

// ── /info 输出 ───────────────────────────────────────────────

/// token 只留头尾：`/info` 是给人看的，也很可能被截图贴出去
fn mask_token(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() <= 16 {
        return "（过短，已隐藏）".to_string();
    }
    let head: String = chars[..8].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}（{} 字符）", chars.len())
}

/// 生成 `/info` 的多行输出（已排版好）。
///
/// `login` 传当前凭证，只用来在最后加一行「用户名 + 部分省略的 token」。
pub fn report(config_dir: &str, login: Option<&crate::auth::AuthData>, lang: Lang) -> Vec<String> {
    let zh = lang == Lang::Zh;
    let info = collect();

    let mut out = vec![String::new()];
    out.push((if zh { "环境信息" } else { "Environment" }).to_string());

    let l_app = if zh { "程序版本" } else { "Version" };
    let l_os = if zh { "操作系统" } else { "OS" };
    let l_build = if zh { "构建号" } else { "Commit" };
    let l_kernel = if zh { "内核" } else { "Kernel" };
    let l_arch = if zh { "架构" } else { "Arch" };
    let l_host = if zh { "主机名" } else { "Host" };
    let l_vm = if zh { "虚拟机" } else { "VM" };
    let l_user = if zh { "用户名" } else { "User" };
    let l_dir = if zh { "配置目录" } else { "Config dir" };

    out.push(format!("  {}{}", pad(l_app, 12), info.app_version));
    out.push(format!("  {}{}", pad(l_build, 12), info.commit));
    out.push(format!("  {}{}", pad(l_os, 12), info.os_name));
    if !info.kernel.is_empty() {
        out.push(format!("  {}{}", pad(l_kernel, 12), info.kernel));
    }
    out.push(format!("  {}{}", pad(l_arch, 12), info.arch));
    out.push(format!("  {}{}", pad(l_host, 12), info.host));
    // 系统登录名（不是 DeepSeek 账号名 —— 账号名要问服务端，见 README 说明）
    out.push(format!("  {}{}", pad(l_user, 12), whoami::username()));
    out.push(format!("  {}{}", pad(l_dir, 12), config_dir));

    // 虚拟机：只识别 WSL，其他系统不输出这一行
    if let Some(vm) = &info.wsl {
        let extra = match &info.windows_build {
            Some(b) if zh => format!("（Windows 构建 {b}）"),
            Some(b) => format!(" (Windows build {b})"),
            None => String::new(),
        };
        out.push(format!("  {}{vm}{extra}", pad(l_vm, 12)));
    }

    // 登录：系统登录名 + 部分省略的 token（头尾各留几个字符）+ 指纹。
    // 指纹是给排查用的：和浏览器里那份 token 的指纹一比就知道是不是同一个。
    if let Some(auth) = login {
        let l_login = if zh { "登录" } else { "Login" };
        out.push(format!(
            "  {}{} · token {} · {} {}",
            pad(l_login, 12),
            whoami::username(),
            mask_token(&auth.token),
            if zh { "指纹" } else { "fingerprint" },
            crate::auth::token_fingerprint(&auth.token),
        ));
    }

    out
}

/// 按显示宽度补空格（中文算 2 列），让中英双语都对得齐
fn pad(label: &str, width: usize) -> String {
    let w: usize = label.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum();
    format!("{label}{}", " ".repeat(width.saturating_sub(w)))
}
