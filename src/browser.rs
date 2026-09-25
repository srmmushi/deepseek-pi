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
    /// 命中点之后 120 字节的可打印原文（最多 3 条），取不到凭证时用它定位
    pub traces: Vec<String>,
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
            traces: Vec::new(),
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
            if hit.traces.len() < 3 {
                let mut from = 0;
                while hit.traces.len() < 3 {
                    let Some(at) = find(&bytes[from..], KEY) else {
                        break;
                    };
                    hit.traces.push(trace_after(&bytes, from + at));
                    from += at + KEY.len();
                }
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

/// 遍历起点。
///
/// 要同时应付三种跑法：
///   · Linux/macOS 原生二进制  → 家目录下的 .config / Application Support
///   · Windows 原生二进制      → LOCALAPPDATA / USERPROFILE
///   · Windows 二进制在 WSL 里跑（本项目的常规用法）→ 上面两个环境变量都可能是空的，
///     但进程的当前目录是真实的 Windows 路径（C:\Users\me\Desktop\...），
///     从它就能切出 C:\Users\me，进而找到 AppData\Local
pub fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        if !local.is_empty() {
            roots.push(PathBuf::from(local));
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if !profile.is_empty() {
            roots.push(PathBuf::from(profile).join("AppData").join("Local"));
        }
    }
    if let Some(home) = home() {
        roots.push(home.join(".config"));
        roots.push(home.join("Library").join("Application Support"));
    }

    // 从当前目录反推：Windows 二进制在 WSL 里跑时，只有这条能拿到真实路径
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(home) = user_home_from(&cwd) {
            let local = home.join("AppData").join("Local");
            if local.is_dir() {
                roots.push(local);
            }
        }
        if let Some(drive) = drive_of(&cwd) {
            if let Ok(entries) = std::fs::read_dir(format!("{drive}:\\Users")) {
                for entry in entries.flatten() {
                    let local = entry.path().join("AppData").join("Local");
                    if local.is_dir() {
                        roots.push(local);
                    }
                }
            }
        }
    }

    // WSL 原生二进制：Windows 盘挂在 /mnt/c
    if let Ok(entries) = std::fs::read_dir("/mnt/c/Users") {
        for entry in entries.flatten() {
            let local = entry.path().join("AppData").join("Local");
            if local.is_dir() {
                roots.push(local);
            }
        }
    }

    roots.sort();
    roots.dedup();
    roots
}

/// 从 `C:\Users\srm木石\Desktop\x` 里切出 `C:\Users\srm木石`
fn user_home_from(path: &Path) -> Option<PathBuf> {
    const MARK: &str = ":\\Users\\";
    let text = path.to_string_lossy();
    // 不在小写串上取下标：那会和原串的字节长度对不上（比如土耳其语 İ）
    let at = text.find(MARK).or_else(|| text.find(":\\users\\"))?;
    let name = text[at + MARK.len()..].split(['\\', '/']).next()?;
    if name.is_empty() {
        return None;
    }
    Some(PathBuf::from(format!("{}:\\Users\\{}", &text[..at], name)))
}

/// 取路径的盘符（`C:\...` → `C`）
fn drive_of(path: &Path) -> Option<char> {
    let text = path.to_string_lossy();
    let mut chars = text.chars();
    let letter = chars.next()?;
    (chars.next() == Some(':') && letter.is_ascii_alphabetic()).then_some(letter)
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

/// 命中点之后 120 字节的可打印原文（非可打印字符显示成 '.'）。
/// 只在 --grab 里打出来用 —— 取不到凭证时，看一眼就知道值的布局长什么样。
fn trace_after(bytes: &[u8], at: usize) -> String {
    let start = (at + KEY.len()).min(bytes.len());
    let end = (start + 120).min(bytes.len());
    bytes[start..end]
        .iter()
        .map(|b| if (0x20..0x7f).contains(b) { *b as char } else { '.' })
        .collect()
}

/// 键名之后的字节里取 token。
///
/// 键和值之间隔多少字节是不固定的，不能写死：
///   · `.ldb` 表块是 `[长度][键][值]`，值紧跟键
///   · `.log` 写入日志是 `[crc][长度][类型][键长][键][值长][值]`，
///     键和值之间还夹着一个变长长度字段
///   · 值本身可能带一个编码标记字节（0 = UTF-16LE，1 = Latin-1）
///
/// 所以不猜偏移：从键后往前试几个起点，找 JSON（形式最明确，不会认错），
/// 找不到再退回裸 token。
fn token_after(rest: &[u8]) -> Option<String> {
    for skip in 0..12usize {
        let Some(tail) = rest.get(skip..) else { break };
        for text in decode(tail) {
            if let Some(token) = json_value(&text) {
                return Some(token);
            }
        }
    }
    for skip in 0..4usize {
        let Some(tail) = rest.get(skip..) else { break };
        for text in decode(tail) {
            let bare: String = text.chars().take_while(|c| is_token_char(*c)).collect();
            if bare.len() >= 32 {
                return Some(bare);
            }
        }
    }
    None
}

/// 同一段字节按可能的编码各解一次
fn decode(bytes: &[u8]) -> Vec<String> {
    let head = &bytes[..bytes.len().min(1024)];
    let mut out = Vec::new();
    if let Ok(text) = std::str::from_utf8(head) {
        out.push(text.to_string());
    }
    // 首位是 0 说明按 UTF-16LE 存
    if head.first() == Some(&0) && head.len() > 8 {
        let units: Vec<u16> = head[1..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        out.push(String::from_utf16_lossy(&units));
    }
    out
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
