//! 从浏览器读 chat.deepseek.com 的 userToken。
//!
//! 网页端把它放在 localStorage 里，而 localStorage 落在 `Local Storage/leveldb`
//! 下。这里不做完整的 LevelDB 解析，只在字节里定位这个键、取出它的值。
//!
//! 值存的是 JSON（`{"value":"<token>",...}`），不是裸 token —— 这一点很容易踩坑。
//! 只读文件、不碰浏览器锁，所以浏览器开着也能读。

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};

/// 浏览器：显示名 + Windows / Linux / macOS 三套 User Data 路径。Edge 优先。
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

const KEY: &[u8] = b"userToken";
const ORIGIN: &[u8] = b"chat.deepseek.com";
/// 单个文件最多读这么多
const MAX_BYTES: u64 = 8 << 20;

/// 一个候选目录的扫描结果
pub struct Hit {
    pub browser: String,
    pub dir: PathBuf,
    /// 文件里出现 userToken 的次数
    pub key_hits: usize,
    /// 文件里是否出现本站域名
    pub origin: bool,
    pub token: Option<String>,
}

/// 扫遍所有候选目录
pub fn scan() -> Vec<Hit> {
    let mut out = Vec::new();
    for (name, dir) in leveldb_dirs() {
        let mut hit = Hit {
            browser: name,
            dir: dir.clone(),
            key_hits: 0,
            origin: false,
            token: None,
        };
        // 先写入日志（新记录都在这儿、且不压缩），再落盘的表文件
        let mut files = files_by_ext(&dir, "log");
        files.extend(files_by_ext(&dir, "ldb"));
        for file in files {
            let Ok(bytes) = read_capped(&file) else { continue };
            hit.key_hits += count_of(&bytes, KEY);
            hit.origin |= find(&bytes, ORIGIN).is_some();
            if hit.token.is_none() {
                hit.token = token_in(&bytes);
            }
        }
        out.push(hit);
    }
    out
}

/// 取第一个能拿到的凭证
pub fn extract_user_token() -> Option<(String, String)> {
    scan()
        .into_iter()
        .find_map(|hit| hit.token.map(|token| (hit.browser, token)))
}

/// 所有可能的 `Local Storage/leveldb` 目录，按浏览器优先级排序
fn leveldb_dirs() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for (name, win, linux, mac) in BROWSERS {
        for root in user_data_roots(win, linux, mac) {
            // 不写死 profile 名：枚举子目录，谁的 leveldb 在就算谁
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in entries.flatten() {
                let dir = entry.path().join("Local Storage").join("leveldb");
                if dir.is_dir() {
                    out.push(((*name).to_string(), dir));
                }
            }
        }
    }
    out
}

/// User Data 目录的候选位置。WSL 下浏览器在 Windows 那边，还要翻 /mnt/c/Users。
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
                if entry.path().is_dir() {
                    roots.push(entry.path().join("AppData").join("Local").join(win));
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

/// 目录里指定后缀的文件，按修改时间从新到旧
fn files_by_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let want = OsStr::new(ext);
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension() == Some(want))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
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

/// 在字节流里找本站的 userToken 值。
/// 键名要离域名足够近，否则可能是别处出现的同名字符串。
fn token_in(bytes: &[u8]) -> Option<String> {
    let mut from = 0;
    while let Some(at) = find(&bytes[from..], KEY) {
        let start = from + at;
        let window = start.saturating_sub(48);
        if find(&bytes[window..start], ORIGIN).is_some() {
            if let Some(token) = token_after(&bytes[start + KEY.len()..]) {
                return Some(token);
            }
        }
        from = start + KEY.len();
    }
    None
}

/// 键名之后的字节里取 token
fn token_after(rest: &[u8]) -> Option<String> {
    // 值前面可能有一个编码标记字节（0 = UTF-16，1 = Latin-1）
    let mut head = rest;
    if matches!(head.first(), Some(0 | 1)) {
        head = &head[1..];
    }
    let take = head.len().min(320);
    let text = std::str::from_utf8(&head[..take]).ok()?;

    // 正常形态是 JSON：{"value":"<token>",...}
    if let Some(token) = json_value(text) {
        return Some(token);
    }
    // 兼容没包 JSON 的形态：直接跟在后面的裸 token
    let bare: String = text.chars().take_while(|c| is_token_char(*c)).collect();
    (bare.len() >= 16).then_some(bare)
}

/// 从 `"value":"<token>"` 里取值
fn json_value(text: &str) -> Option<String> {
    const FIELD: &str = "\"value\"";
    let rest = text.get(text.find(FIELD)? + FIELD.len()..)?;
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let token = &rest[..rest.find('"')?];
    (token.len() >= 16).then(|| token.to_string())
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '=')
}

fn count_of(haystack: &[u8], needle: &[u8]) -> usize {
    let mut count = 0;
    let mut from = 0;
    while let Some(at) = find(&haystack[from..], needle) {
        count += 1;
        from += at + needle.len();
    }
    count
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
