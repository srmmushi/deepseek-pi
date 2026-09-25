//! 工具层：文本解析器 + write / read / list / exec / search 五个工具
//!
//! 工具调用以「独占一行的文本」形式出现在模型输出里，例如：
//!   write:"内容",路径     read:路径     list:目录     exec:命令     search:关键词

use crate::config::Lang;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 单次读取的最大字符数
const MAX_READ_CHARS: usize = 120_000;
/// list 最多返回的条目数
const MAX_LIST_ENTRIES: usize = 500;
/// 递归搜索最多扫描的文件数
const MAX_SEARCH_FILES: usize = 6000;
/// 搜索最多返回的匹配数
const MAX_SEARCH_MATCHES: usize = 200;
/// 单个文件参与搜索的最大字节数
const MAX_SEARCH_FILE_BYTES: u64 = 1_500_000;
/// 命令超时（秒）
const EXEC_TIMEOUT_SECS: u64 = 120;
/// 输出上限（字符）
const MAX_OUTPUT_CHARS: usize = 40_000;

/// 支持的五个核心工具
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolName {
    Write,
    Read,
    List,
    Exec,
    Search,
}

impl ToolName {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolName::Write => "write",
            ToolName::Read => "read",
            ToolName::List => "list",
            ToolName::Exec => "exec",
            ToolName::Search => "search",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "write" => Some(ToolName::Write),
            "read" => Some(ToolName::Read),
            "list" => Some(ToolName::List),
            "exec" => Some(ToolName::Exec),
            "search" => Some(ToolName::Search),
            _ => None,
        }
    }
}

/// 解析后的工具调用
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    Write { content: String, path: String },
    Read { path: String },
    List { path: String },
    Exec { command: String },
    Search { query: String },
}

impl ToolCall {
    pub fn name(&self) -> ToolName {
        match self {
            ToolCall::Write { .. } => ToolName::Write,
            ToolCall::Read { .. } => ToolName::Read,
            ToolCall::List { .. } => ToolName::List,
            ToolCall::Exec { .. } => ToolName::Exec,
            ToolCall::Search { .. } => ToolName::Search,
        }
    }
}

/// 工具执行结果
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub ok: bool,
    /// 回传给模型的文本
    pub output: String,
    /// 展示给用户的一行摘要
    pub summary: String,
}

/// 解析结果
#[derive(Debug, Default, Clone)]
pub struct ParseResult {
    pub calls: Vec<ToolCall>,
    pub errors: Vec<String>,
}

/// 去掉成对引号并还原常见转义
fn unquote(input: &str) -> String {
    let s = input.trim();
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= 2 {
        let first = chars[0];
        let last = chars[chars.len() - 1];
        if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
            let inner: String = chars[1..chars.len() - 1].iter().collect();
            let mut out = String::new();
            let mut it = inner.chars();
            while let Some(ch) = it.next() {
                if ch == '\\' {
                    if let Some(next) = it.next() {
                        out.push(match next {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            other => other,
                        });
                        continue;
                    }
                }
                out.push(ch);
            }
            return out;
        }
    }
    s.to_string()
}

/// 解析 write 的参数：`"内容",路径`（兼容不带引号的写法）
fn parse_write_args(rest: &str) -> Result<(String, String), String> {
    let chars: Vec<char> = rest.chars().collect();
    if chars.is_empty() {
        return Err("write 缺少参数".to_string());
    }
    if chars[0] == '"' || chars[0] == '\'' {
        let quote = chars[0];
        let mut i = 1;
        let mut content = String::new();
        let mut closed = false;
        while i < chars.len() {
            let ch = chars[i];
            if ch == '\\' && i + 1 < chars.len() {
                let next = chars[i + 1];
                content.push(match next {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    other => other,
                });
                i += 2;
                continue;
            }
            if ch == quote {
                closed = true;
                i += 1;
                break;
            }
            content.push(ch);
            i += 1;
        }
        if !closed {
            return Err("write 内容缺少结束引号".to_string());
        }
        while i < chars.len() && (chars[i] == ',' || chars[i].is_whitespace()) {
            i += 1;
        }
        let path: String = chars[i..].iter().collect();
        let path = unquote(&path);
        if path.is_empty() {
            return Err("write 缺少文件路径".to_string());
        }
        return Ok((content, path));
    }

    match rest.find(',') {
        Some(idx) => {
            let content = rest[..idx].trim().to_string();
            let path = unquote(&rest[idx + 1..]);
            if path.is_empty() {
                return Err("write 缺少文件路径".to_string());
            }
            Ok((content, path))
        }
        None => Err("write 格式应为 write:\"内容\",路径".to_string()),
    }
}

fn build_call(name: &str, rest: &str) -> Result<ToolCall, String> {
    let tool = ToolName::parse(name).ok_or_else(|| format!("未知工具：{name}"))?;
    match tool {
        ToolName::Write => {
            let (content, path) = parse_write_args(rest)?;
            Ok(ToolCall::Write { content, path })
        }
        ToolName::Read => {
            let path = unquote(rest);
            if path.is_empty() {
                return Err("read 缺少文件路径".to_string());
            }
            Ok(ToolCall::Read { path })
        }
        ToolName::List => {
            let path = unquote(rest);
            Ok(ToolCall::List {
                path: if path.is_empty() { ".".to_string() } else { path },
            })
        }
        ToolName::Exec => {
            let command = unquote(rest);
            if command.is_empty() {
                return Err("exec 缺少命令".to_string());
            }
            Ok(ToolCall::Exec { command })
        }
        ToolName::Search => {
            let query = unquote(rest);
            if query.is_empty() {
                return Err("search 缺少关键词".to_string());
            }
            Ok(ToolCall::Search { query })
        }
    }
}

/// 从 assistant 文本中解析全部工具调用（只识别单独成行的调用，忽略代码围栏）
pub fn parse_tool_calls(text: &str) -> ParseResult {
    let mut result = ParseResult::default();
    for (index, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("```") {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        let Some(colon) = lower.find([':', '：']) else {
            continue;
        };
        let head = &lower[..colon];
        if ToolName::parse(head).is_none() {
            continue;
        }
        // 用原串切片，保留大小写与内容
        let rest = &line[colon + line[colon..].chars().next().map(|c| c.len_utf8()).unwrap_or(1)..];
        let rest = rest.trim();
        match build_call(head, rest) {
            Ok(call) => result.calls.push(call),
            Err(err) => result.errors.push(format!("第 {} 行：[{head}] {err}", index + 1)),
        }
    }
    result
}

/// 工具调用的参数摘要（界面展示用）
pub fn describe_call(call: &ToolCall) -> String {
    match call {
        ToolCall::Write { content, path } => format!("{path}（{} 字节）", content.len()),
        ToolCall::Read { path } => path.clone(),
        ToolCall::List { path } => path.clone(),
        ToolCall::Exec { command } => command.clone(),
        ToolCall::Search { query } => query.clone(),
    }
}

/// 注入提示词里的并行调用说明
pub fn tool_is_parallel_hint(lang: Lang) -> String {
    match lang {
        Lang::Zh => "- 可以一次给出多个调用（每行一个），它们会并行执行。互不依赖的读取/搜索请合并成一批：\n  \
                     read:package.json\n  read:tsconfig.json\n\
- 调用之间有先后依赖时，等结果返回后再发下一个调用。\n\
- 写入是一次性完整写入，不是追加。\n\
- 路径可以是相对路径（相对于当前工作目录）或绝对路径。\n\
- 工具执行结果会以 `[工具结果]` 开头的消息返回。"
            .to_string(),
        Lang::En => "- You may emit several calls at once (one per line) and they run in parallel; \
                     batch independent reads/searches:\n  read:package.json\n  read:tsconfig.json\n\
- When calls depend on each other, wait for the result before emitting the next one.\n\
- Write is a full one-shot write, never an append.\n\
- Paths may be relative to the current working directory or absolute.\n\
- Tool results come back as a message starting with `[tool result]`."
            .to_string(),
    }
}

// ── 路径工具 ────────────────────────────────────────────────

fn to_absolute(cwd: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn display_path(cwd: &Path, abs: &Path) -> String {
    match abs.strip_prefix(cwd) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().replace('\\', "/"),
        _ => abs.to_string_lossy().to_string(),
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / 1024.0 / 1024.0)
    }
}

// ── 五个工具 ────────────────────────────────────────────────

fn run_write(content: &str, path: &str, cwd: &Path, lang: Lang) -> ToolResult {
    let abs = to_absolute(cwd, path);
    if let Some(dir) = abs.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return ToolResult {
                ok: false,
                output: format!("write 失败：{e}"),
                summary: path.to_string(),
            };
        }
    }
    match std::fs::write(&abs, content) {
        Ok(()) => {
            let bytes = content.len();
            ToolResult {
                ok: true,
                output: format!("{} {}（{} 字节）", crate::i18n::tr(lang, "tool.write"), path, bytes),
                summary: format!("{path} · {}", format_bytes(bytes as u64)),
            }
        }
        Err(e) => ToolResult {
            ok: false,
            output: format!("write 失败：{e}"),
            summary: path.to_string(),
        },
    }
}

fn run_read(path: &str, cwd: &Path, lang: Lang) -> ToolResult {
    let abs = to_absolute(cwd, path);
    let meta = match std::fs::metadata(&abs) {
        Ok(m) => m,
        Err(e) => {
            return ToolResult {
                ok: false,
                output: format!("read 失败：{e}"),
                summary: path.to_string(),
            }
        }
    };
    if meta.is_dir() {
        return ToolResult {
            ok: false,
            output: format!("{path} 是目录，请使用 list"),
            summary: path.to_string(),
        };
    }
    let bytes = match std::fs::read(&abs) {
        Ok(b) => b,
        Err(e) => {
            return ToolResult {
                ok: false,
                output: format!("read 失败：{e}"),
                summary: path.to_string(),
            }
        }
    };
    if bytes.contains(&0) {
        return ToolResult {
            ok: false,
            output: format!("{path} 疑似二进制文件，无法读取"),
            summary: path.to_string(),
        };
    }
    let text = String::from_utf8_lossy(&bytes).to_string();
    let line_count = text.lines().count().max(1);
    let (body, truncated) = if text.chars().count() > MAX_READ_CHARS {
        (text.chars().take(MAX_READ_CHARS).collect::<String>(), true)
    } else {
        (text, false)
    };
    let tail = if truncated {
        format!("\n（输出已截断，仅显示前 {MAX_READ_CHARS} 字符）")
    } else {
        String::new()
    };
    ToolResult {
        ok: true,
        output: format!("[read] {path}\n{body}{tail}"),
        summary: format!(
            "{path} · {line_count} {} · {}",
            crate::i18n::tr(lang, "tool.lines"),
            format_bytes(meta.len())
        ),
    }
}

fn run_list(path: &str, cwd: &Path, lang: Lang) -> ToolResult {
    let abs = to_absolute(cwd, if path.is_empty() { "." } else { path });
    let entries = match std::fs::read_dir(&abs) {
        Ok(e) => e,
        Err(e) => {
            return ToolResult {
                ok: false,
                output: format!("list 失败：{e}"),
                summary: path.to_string(),
            }
        }
    };
    let mut rows: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        if rows.len() >= MAX_LIST_ENTRIES {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        rows.push(if is_dir { format!("{name}/") } else { name });
    }
    rows.sort();
    let truncated = rows.len() >= MAX_LIST_ENTRIES;
    let more = if truncated {
        format!("\n（输出已截断，仅显示前 {MAX_LIST_ENTRIES} 项）")
    } else {
        String::new()
    };
    let shown = display_path(cwd, &abs);
    ToolResult {
        ok: true,
        output: format!("[list] {shown}\n{}{more}", rows.join("\n")),
        summary: format!(
            "{shown} · {} {}",
            rows.len(),
            crate::i18n::tr(lang, "tool.entries")
        ),
    }
}

fn run_search(query: &str, cwd: &Path, lang: Lang) -> ToolResult {
    let needle = query.to_lowercase();
    let mut matches: Vec<String> = Vec::new();
    let mut scanned = 0usize;
    let mut stack = vec![cwd.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if matches.len() >= MAX_SEARCH_MATCHES || scanned >= MAX_SEARCH_FILES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if matches.len() >= MAX_SEARCH_MATCHES || scanned >= MAX_SEARCH_FILES {
                break;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else { continue };
            if file_type.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if matches!(name.as_str(), "node_modules" | ".git" | "target" | "dist" | ".next") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            scanned += 1;
            let Ok(meta) = entry.metadata() else { continue };
            if meta.len() > MAX_SEARCH_FILE_BYTES {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else { continue };
            if bytes.contains(&0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let rel = display_path(cwd, &path);
            for (no, line) in text.lines().enumerate() {
                if matches.len() >= MAX_SEARCH_MATCHES {
                    break;
                }
                if line.to_lowercase().contains(&needle) {
                    matches.push(format!("{rel}:{}: {}", no + 1, line.trim()));
                }
            }
        }
    }

    if matches.is_empty() {
        return ToolResult {
            ok: true,
            output: format!("未找到包含 \"{query}\" 的内容"),
            summary: format!("{query} · 无匹配"),
        };
    }
    let truncated = matches.len() >= MAX_SEARCH_MATCHES;
    let tail = if truncated {
        format!("\n（输出已截断，仅显示前 {MAX_SEARCH_MATCHES} 处）")
    } else {
        String::new()
    };
    ToolResult {
        ok: true,
        output: format!("[search] {query}\n{}{tail}", matches.join("\n")),
        summary: format!(
            "{query} · {} {}",
            matches.len(),
            crate::i18n::tr(lang, "tool.matches")
        ),
    }
}

/// 原始 shell 执行结果（`!命令` 复用）
#[derive(Debug, Clone)]
pub struct ShellResult {
    pub ok: bool,
    pub code: i32,
    pub output: String,
    pub duration_ms: u128,
}

/// 解码子进程输出：Windows 下 cmd 常按 GBK 输出，UTF-8 失败时回退 GBK
fn decode_output(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            #[cfg(windows)]
            {
                // 无第三方编码库时做一次保守的 Latin-1 兜底，避免直接丢内容
                bytes.iter().map(|b| *b as char).collect::<String>()
            }
            #[cfg(not(windows))]
            {
                String::from_utf8_lossy(bytes).to_string()
            }
        }
    }
}

/// 执行 shell 命令（同步阻塞；调用方在后台线程里跑）
pub fn run_shell(command: &str, cwd: &Path) -> ShellResult {
    let started = std::time::Instant::now();
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.current_dir(cwd);
    match cmd.output() {
        Ok(out) => {
            let mut text = decode_output(&out.stdout);
            text.push_str(&decode_output(&out.stderr));
            let text = text.trim().to_string();
            ShellResult {
                ok: out.status.success(),
                code: out.status.code().unwrap_or(1),
                output: if text.chars().count() > MAX_OUTPUT_CHARS {
                    text.chars().take(MAX_OUTPUT_CHARS).collect()
                } else {
                    text
                },
                duration_ms: started.elapsed().as_millis(),
            }
        }
        Err(e) => ShellResult {
            ok: false,
            code: 1,
            output: format!("执行失败：{e}"),
            duration_ms: started.elapsed().as_millis(),
        },
    }
}

/// 带超时的 shell 执行（exec 工具用）
fn run_exec(command: &str, cwd: &Path, lang: Lang) -> ToolResult {
    // std::process 没有超时；用子线程 + 轮询等待实现
    let (tx, rx) = std::sync::mpsc::channel();
    let cmd_owned = command.to_string();
    let cwd_owned = cwd.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(run_shell(&cmd_owned, &cwd_owned));
    });
    let result = match rx.recv_timeout(std::time::Duration::from_secs(EXEC_TIMEOUT_SECS)) {
        Ok(r) => r,
        Err(_) => ShellResult {
            ok: false,
            code: 124,
            output: format!("命令超时（{EXEC_TIMEOUT_SECS}s）"),
            duration_ms: EXEC_TIMEOUT_SECS as u128 * 1000,
        },
    };
    let body = if result.output.is_empty() {
        "(无输出)".to_string()
    } else {
        result.output.clone()
    };
    ToolResult {
        ok: result.ok,
        output: format!(
            "[exec] {command}\n{} {}\n{body}",
            crate::i18n::tr(lang, "tool.exec"),
            result.code
        ),
        summary: format!("exit {}", result.code),
    }
}

/// 执行一个工具调用
pub fn execute_tool(call: &ToolCall, cwd: &Path, lang: Lang) -> ToolResult {
    match call {
        ToolCall::Write { content, path } => run_write(content, path, cwd, lang),
        ToolCall::Read { path } => run_read(path, cwd, lang),
        ToolCall::List { path } => run_list(path, cwd, lang),
        ToolCall::Exec { command } => run_exec(command, cwd, lang),
        ToolCall::Search { query } => run_search(query, cwd, lang),
    }
}


