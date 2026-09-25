//! 从浏览器里自动读出 `chat.deepseek.com` 的 userToken。
//!
//! 网页端把 userToken 放在 localStorage 里，而 localStorage 落在
//! `Local Storage/leveldb/` 下的 LevelDB 文件里。完整解析 LevelDB 要几百行
//! 还得解 snappy，这里换了个取巧但够用的办法：直接在原始字节里找 `userToken`
//! 这个键，把它后面的值取出来。
//!
//! 之所以够用：Chrome/Edge 的写入日志（`.log`）是不压缩的，而刚登录写下的
//! 记录一定还在日志里。已经落盘进 `.ldb` 并且被 snappy 压过的旧记录可能扫不到
//! —— 那时退回 `/login token` 手动粘贴即可。
//!
//! 只读文件，不碰浏览器的锁，所以浏览器开着也能读。

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};

/// 支持的浏览器：显示名 + Windows / Linux / macOS 三套相对路径。
/// 顺序就是优先级 —— Edge 在最前面，按需求默认用它。
const BROWSERS: &[(&str, &str, &str, &str)] = &[
    (
        "Edge",
        "Microsoft/Edge/User Data",
        ".config/microsoft-edge",
        "Library/Application Support/Microsoft Edge",
    ),
    (
        "Chrome",
        "Google/Chrome/User Data",
        ".config/google-chrome",
        "Library/Application Support/Google/Chrome",
    ),
    (
        "Chromium",
        "Chromium/User Data",
        ".config/chromium",
        "Library/Application Support/Chromium",
    ),
];

/// 常见的配置目录名（一个人可能有多个 profile）
const PROFILES: &[&str] = &["Default", "Profile 1", "Profile 2", "Profile 3"];

/// 单个文件最多读这么多：`.ldb` 可能很大，而我们要找的记录通常在后段
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// 扫一遍浏览器存储，返回 `(浏览器名, userToken)`。Edge 优先。
pub fn extract_user_token() -> Option<(String, String)> {
    for (name, dir) in leveldb_dirs() {
        // 先看写入日志：刚登录的记录一定在这里，而且是明文
        let mut files = files_by_ext(&dir, "log");
        files.extend(files_by_ext(&dir, "ldb"));
        for file in files {
            if let Ok(bytes) = read_capped(&file) {
                if let Some(token) = find_token(&bytes) {
                    return Some((name, token));
                }
            }
        }
    }
    None
}

/// 找所有可能的 `Local Storage/leveldb` 目录
fn leveldb_dirs() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for (name, win, linux, mac) in BROWSERS {
        for root in user_data_roots(win, linux, mac) {
            for profile in PROFILES {
                let dir = root
                    .join(profile)
                    .join("Local Storage")
                    .join("leveldb");
                if dir.is_dir() {
                    out.push(((*name).to_string(), dir));
                }
            }
        }
    }
    out
}

/// 一份 User Data 目录可能来自多个位置。
/// WSL 里浏览器装在 Windows 那边，所以还要翻 `/mnt/c/Users/*`。
fn user_data_roots(win: &str, linux: &str, mac: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join(win));
    }
    if let Some(home) = home() {
        roots.push(home.join(linux));
        roots.push(home.join(mac));
    }

    let users = Path::new("/mnt/c/Users");
    if users.is_dir() {
        if let Ok(entries) = std::fs::read_dir(users) {
            for entry in entries.flatten() {
                let dir = entry.path();
                if dir.is_dir() {
                    roots.push(dir.join("AppData").join("Local").join(win));
                }
            }
        }
    }
    roots
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// 目录里指定后缀的文件，按修改时间从新到旧（新的更可能含目标记录）
fn files_by_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let want = OsStr::new(ext);
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension() == Some(want))
        .filter_map(|e| {
            let time = e.metadata().ok()?.modified().ok()?;
            Some((time, e.path()))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.into_iter().map(|(_, path)| path).collect()
}

fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_BYTES)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

/// 在字节流里找 `userToken` 并取出它后面的值。
///
/// localStorage 记录的布局大致是：
/// `_https://chat.deepseek.com\0\x01userToken\x01<Token>`
/// 键和值在 LevelDB 的数据块里是紧挨着的（长度前缀在键前面），
/// 所以找到键之后往后读一串 token 字符就行。
fn find_token(bytes: &[u8]) -> Option<String> {
    const KEY: &[u8] = b"userToken";
    let mut from = 0;
    while let Some(at) = find(&bytes[from..], KEY) {
        let after_key = from + at + KEY.len();
        if let Some(token) = token_at(bytes, after_key) {
            return Some(token);
        }
        from = after_key;
    }
    None
}

/// 从 `start` 处取一段像 token 的字符串
fn token_at(bytes: &[u8], start: usize) -> Option<String> {
    let mut i = start;
    // 值前面有个编码标记：\x00 = UTF-16，\x01 = Latin-1（token 是纯 ASCII，走这条）
    let marker = *bytes.get(i)?;
    if marker == 1 {
        i += 1;
    } else if marker == 0 {
        // UTF-16 的值这里不处理：token 不会走到这条分支
        return None;
    }

    let is_token_byte = |b: u8| {
        b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'='
    };
    let begin = i;
    while i < bytes.len() && is_token_byte(bytes[i]) {
        i += 1;
    }
    let text = std::str::from_utf8(&bytes[begin..i]).ok()?;
    // userToken 是长串；太短的更可能是键名残留或别的字段
    if text.len() >= 24 {
        Some(text.to_string())
    } else {
        None
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
