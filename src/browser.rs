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

/// 单个文件最多读这么多：`.ldb` 可能很大，而我们要找的记录通常在后段
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// DeepSeek 的 userToken 长度。盲扫可能多带出几个字节，按这个长度截断。
const TOKEN_LEN: usize = 64;

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
            // 不写死 Default / Profile N：直接枚举根目录下所有子目录，
            // 谁的 Local Storage/leveldb 在就算谁 —— 少见的 profile 名也不会漏
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

/// localStorage 里存 userToken 用的键名
const TOKEN_KEY: &[u8] = b"userToken";
/// 本站域名（键名前不远处应该出现它）
const TOKEN_ORIGIN: &[u8] = b"chat.deepseek.com";

/// 一次扫描的诊断结果。
/// 登录不成功时先看它 —— 路径不对 / 键名不对 / 记录被压缩，三种原因一眼能分。
pub struct Probe {
    pub browser: String,
    pub dir: PathBuf,
    pub log_files: usize,
    pub ldb_files: usize,
    pub bytes: u64,
    /// 文件里出现 `userToken` 的次数
    pub key_hits: usize,
    /// 文件里是否出现本站域名
    pub origin_hit: bool,
    pub token: Option<String>,
    /// 键名后面那一段原始字节（可打印化），用来判断值的真实布局
    pub samples: Vec<String>,
}

/// 扫一遍并如实汇报「看到了什么」
pub fn probe() -> Vec<Probe> {
    let mut out = Vec::new();
    for (name, dir) in leveldb_dirs() {
        let logs = files_by_ext(&dir, "log");
        let ldbs = files_by_ext(&dir, "ldb");
        let mut probe = Probe {
            browser: name,
            dir: dir.clone(),
            log_files: logs.len(),
            ldb_files: ldbs.len(),
            bytes: 0,
            key_hits: 0,
            origin_hit: false,
            token: None,
            samples: Vec::new(),
        };
        // 顺序与 extract_user_token 一致：先写入日志，再落盘的表文件
        for file in logs.iter().chain(ldbs.iter()) {
            let Ok(bytes) = read_capped(file) else { continue };
            probe.bytes += bytes.len() as u64;
            probe.key_hits += count_of(&bytes, TOKEN_KEY);
            probe.origin_hit |= find(&bytes, TOKEN_ORIGIN).is_some();
            if probe.token.is_none() {
                probe.token = find_token(&bytes);
            }
            if probe.samples.len() < 3 {
                if let Some(s) = sample_after_key(&bytes) {
                    probe.samples.push(s);
                }
            }
        }
        out.push(probe);
    }
    out
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

/// 把键名后面那 96 字节按可打印形式取出（诊断用）。
/// 值的真实布局（有没有标记字节、token 多长）看这一段就清楚了。
fn sample_after_key(bytes: &[u8]) -> Option<String> {
    let at = find(bytes, TOKEN_KEY)?;
    let start = at + TOKEN_KEY.len();
    let end = (start + 96).min(bytes.len());
    Some(
        bytes[start..end]
            .iter()
            .map(|b| if b.is_ascii_graphic() { *b as char } else { '.' })
            .collect(),
    )
}

/// 在字节流里找 `userToken` 并取出它后面的值。
///
/// localStorage 记录的布局大致是：
/// `_https://chat.deepseek.com\0\x01userToken\x01<Token>`
/// 键和值在 LevelDB 的数据块里是紧挨着的（长度前缀在键前面），
/// 所以找到键之后往后读一串 token 字符就行。
fn find_token(bytes: &[u8]) -> Option<String> {
    // 只认「键名本身」，但要求它前面不远处就是本站域名。
    //
    // 之前写死了 `chat.deepseek.com\0\x01userToken` 这个完整字节串，结果一个也
    // 匹配不到：Chrome/Edge 在 origin 与键名之间插的标记字节各家版本并不一致
    // （有的带 \x01、有的不带）。改成「键名前 48 字节内出现域名」既容错，
    // 又不会把别处出现的同名字符串误当成 localStorage 记录。
    const KEY: &[u8] = b"userToken";
    const ORIGIN: &[u8] = b"chat.deepseek.com";
    let mut from = 0;
    while let Some(at) = find(&bytes[from..], TOKEN_KEY) {
        let start = from + at;
        let window = start.saturating_sub(48);
        if find(&bytes[window..start], TOKEN_ORIGIN).is_some() {
            if let Some(token) = token_at(bytes, start + TOKEN_KEY.len()) {
                return Some(token);
            }
        }
        from = start + TOKEN_KEY.len();
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
    // 盲扫的麻烦：值后面紧跟着下一条记录的长度前缀，而这些字节本身
    // 也可能落在 token 字符集里，于是多带出几个字符、把凭证弄坏。
    // DeepSeek 的 userToken 是固定 64 字符，按已知长度截断就干净了。
    if text.len() < 24 {
        return None;
    }
    Some(text.chars().take(TOKEN_LEN).collect())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
