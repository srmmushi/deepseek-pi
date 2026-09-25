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

const KEY: &[u8] = b"userToken";
const ORIGIN: &[u8] = b"chat.deepseek.com";
/// 单个文件最多读这么多
const MAX_BYTES: u64 = 8 << 20;
/// 遍历的最大层数
const MAX_DEPTH: usize = 6;

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
    for (browser, dir) in leveldb_dirs() {
        let mut hit = Hit {
            browser,
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

/// 所有 `Local Storage/leveldb` 目录，Edge 优先。
///
/// 不拼路径：浏览器版本、安装位置、profile 名各不相同，拼出来十有八九对不上。
/// 改成在几个根目录下有界遍历，找「父目录叫 Local Storage、自己叫 leveldb」的目录。
fn leveldb_dirs() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for root in search_roots() {
        walk(&root, 0, &mut out);
    }
    out.sort_by_key(|(name, _)| if name.as_str() == "Edge" { 0 } else { 1 });
    out
}

/// 遍历起点：本机用户目录，以及 WSL 下 Windows 那边的用户目录
fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        roots.push(PathBuf::from(local));
    }
    if let Some(home) = home() {
        roots.push(home.join(".config"));
        roots.push(home.join("Library").join("Application Support"));
    }
    if let Ok(entries) = std::fs::read_dir("/mnt/c/Users") {
        for entry in entries.flatten() {
            let local = entry.path().join("AppData").join("Local");
            if local.is_dir() {
                roots.push(local);
            }
        }
    }
    roots
}

/// 有界遍历。只往可能相关的目录里钻，不会把整个盘走一遍。
fn walk(dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "leveldb" {
            if path
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|p| p == OsStr::new("Local Storage"))
            {
                out.push((browser_of(&path), path));
            }
            continue;
        }
        if is_relevant_dir(&name) {
            walk(&path, depth + 1, out);
        }
    }
}

fn is_relevant_dir(name: &str) -> bool {
    matches!(
        name,
        "User Data"
            | "Local Storage"
            | "Local"
            | "AppData"
            | "Application Support"
            | ".config"
            | "Microsoft"
            | "Google"
            | "Chromium"
            | "Edge"
            | "Chrome"
            | "Default"
    ) || name.starts_with("Profile")
}

fn browser_of(path: &Path) -> String {
    let text = path.to_string_lossy().to_lowercase();
    if text.contains("edge") {
        "Edge".to_string()
    } else if text.contains("chromium") {
        "Chromium".to_string()
    } else if text.contains("chrome") {
        "Chrome".to_string()
    } else {
        "浏览器".to_string()
    }
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
    let head = &rest[..rest.len().min(640)];
    match head.first() {
        // 0 = 值按 UTF-16LE 存（token 本身是 ASCII，但浏览器偶尔会这么存）
        Some(0) => {
            let units: Vec<u16> = head[1..]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            pick(&String::from_utf16_lossy(&units))
        }
        // 1 = Latin-1
        Some(1) => pick(std::str::from_utf8(&head[1..]).ok()?),
        _ => pick(std::str::from_utf8(head).ok()?),
    }
}

/// 从一段文本里挑出 token：优先 JSON 的 value 字段，其次按裸 token 兜底
fn pick(text: &str) -> Option<String> {
    if let Some(token) = json_value(text) {
        return Some(token);
    }
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
