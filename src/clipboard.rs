//! 往系统剪贴板写文本。
//!
//! 优先用本地工具，失败再退回 OSC 52 让终端自己处理：
//!   - WSL 下 `clip.exe` 直通 Windows 剪贴板，最省事
//!   - 桌面 Linux 常见 wl-copy（Wayland）/ xclip / xsel
//!   - OSC 52 在 Windows Terminal、iTerm2、kitty 等终端里都能用

use std::io::Write;
use std::process::{Command, Stdio};

fn tools() -> Vec<(&'static str, Vec<&'static str>)> {
    if std::path::Path::new("/mnt/c/Windows/System32/clip.exe").exists() {
        return vec![("clip.exe", vec![])];
    }
    match std::env::var("WAYLAND_DISPLAY") {
        Ok(_) => vec![("wl-copy", vec![]), ("xclip", vec!["-selection", "clipboard"])],
        Err(_) => vec![
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
            ("wl-copy", vec![]),
        ],
    }
}

fn pipe(tool: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(stdin) = child.stdin.as_mut() {
        if stdin.write_all(text.as_bytes()).is_err() {
            return false;
        }
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// 复制文本；返回是否成功
pub fn copy(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    for (tool, args) in tools() {
        if pipe(tool, &args, text) {
            return true;
        }
    }
    osc52(text)
}

/// OSC 52：把 base64 后的内容交给终端写进剪贴板
fn osc52(text: &str) -> bool {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    let encoded = B64.encode(text.as_bytes());
    // 有些终端对单条 OSC 长度有限制，过长时放弃
    if encoded.len() > 100_000 {
        return false;
    }
    let mut out = std::io::stdout();
    let _ = write!(out, "\u{1b}]52;c;{encoded}\u{7}");
    let _ = out.flush();
    true
}
