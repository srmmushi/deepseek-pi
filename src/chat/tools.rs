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

/// 支持的工具
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolName {
    Write,
    Read,
    List,
    Exec,
    Search,
    /// 按行改文件（改单行或多行）
    Edit,
}

impl ToolName {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolName::Write => "write",
            ToolName::Read => "read",
            ToolName::List => "list",
            ToolName::Exec => "exec",
            ToolName::Search => "search",
            ToolName::Edit => "edit",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "write" => Some(ToolName::Write),
            "read" => Some(ToolName::Read),
            "list" => Some(ToolName::List),
            "exec" => Some(ToolName::Exec),
            "search" => Some(ToolName::Search),
            "edit" => Some(ToolName::Edit),
            _ => None,
        }
    }
}

/// 解析后的工具调用
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    Write { content: String, path: String },
    /// 读文件；`range` 给了就只读这几行（闭区间，从 1 起）
    Read {
        path: String,
        range: Option<(usize, usize)>,
    },
    List { path: String },
    Exec { command: String },
    Search { query: String },
    /// 按行改文件：把 `path` 的第 from–to 行（闭区间）换成 `content`。
    /// `from == 0` 表示在文件末尾追加。
    Edit {
        content: String,
        path: String,
        from: usize,
        to: usize,
    },
}

impl ToolCall {
    pub fn name(&self) -> ToolName {
        match self {
            ToolCall::Write { .. } => ToolName::Write,
            ToolCall::Read { .. } => ToolName::Read,
            ToolCall::List { .. } => ToolName::List,
            ToolCall::Exec { .. } => ToolName::Exec,
            ToolCall::Search { .. } => ToolName::Search,
            ToolCall::Edit { .. } => ToolName::Edit,
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

/// 拆出开头的引号内容，返回 `(内容, 结束引号之后的剩余部分)`。
fn split_quoted(rest: &str) -> Result<(String, String), String> {
    let chars: Vec<char> = rest.chars().collect();
    if chars.is_empty() || (chars[0] != '"' && chars[0] != '\'') {
        return Err("内容需要用引号包起来，形如 \"内容\",路径".to_string());
    }
    let quote = chars[0];
    let mut i = 1;
    let mut content = String::new();
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
        if ch == quote && looks_like_terminator(&chars[i + 1..], quote) {
            let tail: String = chars[i + 1..].iter().collect();
            return Ok((content, tail));
        }
        content.push(ch);
        i += 1;
    }
    Err("内容缺少结束引号（结尾应为 \",路径）".to_string())
}

/// 结束引号后面那段是不是「,路径」该有的样子。
///
/// 这个判断是必须的：写 HTML/JS 时内容里几乎一定有没转义的引号，
/// 只要见到引号就收尾，整段内容会被从中间截断（而且不会有任何报错）。
fn looks_like_terminator(after: &[char], quote: char) -> bool {
    let rest: String = after.iter().collect();
    let rest = rest.trim_start().trim_start_matches(',').trim();
    if rest.is_empty() {
        return false;
    }
    // 路径就剩最后一段，不该再出现换行或引号
    !rest.contains('\n') && !rest.contains(quote) && !rest.contains('"') && !rest.contains('\'')
}

/// 解析 `12` / `12-20` / `0`（0 表示文件末尾）
fn parse_range(s: &str) -> Option<(usize, usize)> {
    let s = s.trim();
    if let Some((a, b)) = s.split_once('-') {
        return match (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
            (Ok(a), Ok(b)) => Some((a, b)),
            _ => None,
        };
    }
    s.parse::<usize>().ok().map(|n| (n, n))
}

/// 从参数尾部拆出 `,12-20` 这样的行范围。
/// 只有确实长得像范围才拆 —— 免得把文件名里的逗号当成分隔符。
fn split_line_range(s: &str) -> (String, Option<(usize, usize)>) {
    let Some((head, tail)) = s.rsplit_once(',') else {
        return (s.to_string(), None);
    };
    match parse_range(tail) {
        Some(r) => (head.to_string(), Some(r)),
        None => (s.to_string(), None),
    }
}

/// 解析 write 的参数：`"内容",路径`（兼容不带引号的写法）
fn parse_write_args(rest: &str) -> Result<(String, String), String> {
    let trimmed = rest.trim_start();
    if trimmed.starts_with('"') || trimmed.starts_with('\'') {
        let (content, tail) = split_quoted(trimmed)?;
        let path = unquote(tail.trim_start_matches(',').trim());
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
            // `read:路径` 读全文；`read:路径,12-20` 只读这段（带行号，便于随后按行改）
            let (path_part, range) = split_line_range(rest);
            let path = unquote(path_part.trim());
            if path.is_empty() {
                return Err("read 缺少文件路径".to_string());
            }
            Ok(ToolCall::Read { path, range })
        }
        ToolName::Edit => {
            let (content, tail) = split_quoted(rest.trim_start())?;
            let tail = tail.trim_start_matches(',').trim();
            // 允许两种顺序：`内容,路径,12-20` 或 `内容,12-20,路径`
            let (path_part, range) = match split_line_range(tail) {
                (p, Some(r)) => (p, Some(r)),
                (_, None) => {
                    let (first, rest2) = tail.split_once(',').unwrap_or((tail, ""));
                    match parse_range(first) {
                        Some(r) => (rest2.trim().to_string(), Some(r)),
                        None => (tail.to_string(), None),
                    }
                }
            };
            let path = unquote(path_part.trim());
            if path.is_empty() {
                return Err("edit 缺少文件路径".to_string());
            }
            let (from, to) = range.ok_or_else(|| {
                "edit 缺少行范围，形如 edit:\"新内容\",路径,12-20（单行写 12，末尾追加写 0）"
                    .to_string()
            })?;
            Ok(ToolCall::Edit {
                content,
                path,
                from,
                to,
            })
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

/// 剥掉一行两端那些「装饰性」的 markdown / 列表符号：
/// `- ` `* ` `+ ` `> ` `1. ` 这类列表与编号，以及包在工具名外面的 `**` `_` `` ` `` `#`。
///
/// 模型很爱把调用写成 `- read:src/x.rs` 或 `` `read:x` ``。那都是明确的调用意图，
/// 判成「没命中」就是白丢一轮；而清理只动这些装饰字符，参数本身一个字节都不碰。
fn strip_markup(line: &str) -> &str {
    let mut s = line.trim();
    loop {
        let before = s;
        if let Some(rest) = s.strip_prefix('>') {
            s = rest.trim_start();
        }
        for p in ["- ", "* ", "+ ", "• "] {
            if let Some(rest) = s.strip_prefix(p) {
                s = rest.trim_start();
            }
        }
        // `1. ` / `1) ` 这类编号
        let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 {
            let rest = s[digits..].trim_start();
            if let Some(r) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
                if r.starts_with(char::is_whitespace) {
                    s = r.trim_start();
                }
            }
        }
        s = s.trim_start_matches(['*', '_', '`', '#', ' ']);
        if s == before {
            return s;
        }
    }
}

/// 这一行是不是工具调用？
///
/// 界面上用到：模型输出的调用行会另外以「▌ 工具名 参数」列出来，
/// 正文里再原样打一遍就是重复。
pub fn is_tool_call_line(line: &str) -> bool {
    call_head(line).is_some()
}

/// 这一行是不是一个工具调用的开头？是则给出小写的工具名。
///
/// 工具名上的强调符号在这里一并剥掉（`**read**:x` 也算 `read`）。
fn call_head(line: &str) -> Option<String> {
    let s = strip_markup(line);
    let colon = s.find([':', '：'])?;
    let head: String = s[..colon]
        .chars()
        .filter(|c| !matches!(*c, '*' | '_' | '`'))
        .collect();
    let head = head.trim().to_ascii_lowercase();
    ToolName::parse(&head)?;
    Some(head)
}

/// 从 assistant 文本中解析全部工具调用（只识别单独成行的调用，忽略代码围栏）
///
/// `write` 的正文允许跨多行 —— 写文件时内容本来就有换行，如果只认单行，
/// 模型会被迫把整个文件压成一行（缩进全丢，还要多花几万 token 思考）。
/// 所以 write 解析不完整时继续往下拼，直到拼出完整的 `write:"…",路径`。
pub fn parse_tool_calls(text: &str) -> ParseResult {
    let mut result = ParseResult::default();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        // 先剥掉列表符号与强调符号：`- read:x`、`` `read:x` ``、`**read**:x` 都算调用
        let line = strip_markup(lines[i]);
        if line.is_empty() || line.starts_with("```") {
            i += 1;
            continue;
        }
        let Some(head) = call_head(line) else {
            i += 1;
            continue;
        };
        let colon = line.find([':', '：']).unwrap_or(0);
        // 用原串切片，保留大小写与内容
        let offset = colon + line[colon..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        // 参数末尾的反引号 / 句号是模型顺手带上的装饰，不算参数本身
        let mut body = line[offset..]
            .trim()
            .trim_end_matches(['`', '。'])
            .trim_end()
            .to_string();
        let start_line = i + 1;

        let mut call = build_call(&head, &body);
        // write 与 edit 的内容都可能跨很多行，边拼边试
        if head == "write" || head == "edit" {
            let mut used = 0;
            while call.is_err() && i + 1 < lines.len() && used < 2000 {
                let next = strip_markup(lines[i + 1]);
                // 撞上另一个工具调用，说明已经吃过头了
                if call_head(next).is_some() {
                    break;
                }
                i += 1;
                used += 1;
                body.push('\n');
                body.push_str(lines[i]);
                call = build_call(&head, &body);
            }
        }

        match call {
            Ok(call) => result.calls.push(call),
            Err(err) => result.errors.push(format!("第 {start_line} 行：[{head}] {err}")),
        }
        i += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calls(text: &str) -> Vec<ToolCall> {
        parse_tool_calls(text).calls
    }

    #[test]
    fn plain_calls() {
        assert_eq!(calls("read:src/a.rs").len(), 1);
        assert_eq!(calls("list:src").len(), 1);
        assert_eq!(calls("search:foo").len(), 1);
    }

    /// 模型最爱的几种装饰写法：列表符号、加粗、反引号、编号
    #[test]
    fn tolerates_markup_around_the_call() {
        assert_eq!(calls("- read:src/a.rs").len(), 1);
        assert_eq!(calls("* read:src/a.rs").len(), 1);
        assert_eq!(calls("**read**:src/a.rs").len(), 1);
        assert_eq!(calls("`read:src/a.rs`").len(), 1);
        assert_eq!(calls("1. read:src/a.rs").len(), 1);
        assert_eq!(calls("> read:src/a.rs").len(), 1);
    }

    #[test]
    fn tolerates_spacing_case_and_fullwidth_colon() {
        assert_eq!(calls("read : src/a.rs").len(), 1);
        assert_eq!(calls("read：src/a.rs").len(), 1);
        assert_eq!(calls("READ:src/a.rs").len(), 1);
    }

    #[test]
    fn multi_line_write_keeps_its_content() {
        let got = calls("write:\"line1\nline2\n\",src/a.txt");
        assert_eq!(got.len(), 1);
        match &got[0] {
            ToolCall::Write { content, path } => {
                assert_eq!(content.as_str(), "line1\nline2\n");
                assert_eq!(path.as_str(), "src/a.txt");
            }
            other => panic!("期望 write，实际 {other:?}"),
        }
    }

    /// 夹在说明文字里的「调用」不算调用 —— 否则会误执行用户没要求的事
    #[test]
    fn prose_is_not_a_call() {
        assert!(calls("我先 read:src/a.rs 看看").is_empty());
        assert!(calls("这不是一个调用").is_empty());
    }
}

/// 工具调用的参数摘要（界面展示用）
pub fn describe_call(call: &ToolCall) -> String {
    match call {
        ToolCall::Write { content, path } => format!("{path}（{} 字节）", content.len()),
        ToolCall::Read { path, range } => match range {
            Some((a, b)) => format!("{path} · {a}-{b} 行"),
            None => path.clone(),
        },
        ToolCall::Edit { path, from, to, .. } => {
            if *from == 0 {
                format!("{path} · 末尾追加")
            } else {
                format!("{path} · {from}-{to} 行")
            }
        }
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

fn run_read(path: &str, range: Option<(usize, usize)>, cwd: &Path, lang: Lang) -> ToolResult {
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
    let all: Vec<&str> = text.lines().collect();
    let line_count = all.len().max(1);

    // 指定了行范围：带行号返回，模型照着行号就能用 edit 精确改
    if let Some((from, to)) = range {
        let from = from.max(1);
        let to = to.min(line_count).max(from);
        if from > line_count {
            return ToolResult {
                ok: false,
                output: format!("read 失败：{path} 只有 {line_count} 行，读不到第 {from} 行"),
                summary: path.to_string(),
            };
        }
        let body = (from..=to)
            .filter_map(|n| all.get(n - 1).map(|l| format!("{n:>5}  {l}")))
            .collect::<Vec<_>>()
            .join("\n");
        return ToolResult {
            ok: true,
            output: format!(
                "[read] {path} 第 {from}-{to} 行（全文 {line_count} 行，行号可直接用于 edit）\n{body}"
            ),
            summary: format!("{path} · {from}-{to} 行"),
        };
    }

    let (body, truncated) = if text.chars().count() > MAX_READ_CHARS {
        (text.chars().take(MAX_READ_CHARS).collect::<String>(), true)
    } else {
        (text, false)
    };
    let tail = if truncated {
        format!("\n（已截断，仅前 {MAX_READ_CHARS} 字符；可用 read:路径,起始-结束 分段读）")
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

/// 按行改：把第 from–to 行换成 content；`from == 0` 表示在末尾追加。
///
/// 有了它，模型改一行不必把整个文件重写一遍 —— 省 token，也不会因为
/// 顺带重排而改坏别的部分。
fn run_edit(content: &str, path: &str, from: usize, to: usize, cwd: &Path) -> ToolResult {
    let abs = to_absolute(cwd, path);
    let text = match std::fs::read_to_string(&abs) {
        Ok(t) => t,
        Err(e) => {
            return ToolResult {
                ok: false,
                output: format!("edit 失败：{e}"),
                summary: path.to_string(),
            }
        }
    };
    let keep_trailing_newline = text.ends_with('\n');
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let total = lines.len();
    let new_lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();

    let what = if from == 0 {
        lines.extend(new_lines.iter().cloned());
        "末尾追加".to_string()
    } else {
        if from > total || to < from || to > total {
            return ToolResult {
                ok: false,
                output: format!(
                    "edit 失败：行范围 {from}-{to} 超出 1-{total}\
                     （先用 read:{path},起始-结束 确认行号；末尾追加写 0）"
                ),
                summary: path.to_string(),
            };
        }
        lines.splice((from - 1)..to, new_lines.iter().cloned());
        format!("{from}-{to} 行")
    };

    let mut out = lines.join("\n");
    if keep_trailing_newline {
        out.push('\n');
    }
    if let Err(e) = std::fs::write(&abs, out) {
        return ToolResult {
            ok: false,
            output: format!("edit 写入失败：{e}"),
            summary: path.to_string(),
        };
    }
    ToolResult {
        ok: true,
        output: format!(
            "[edit] {path}\n已替换 {what}（写入 {} 行，现在共 {} 行）",
            new_lines.len(),
            lines.len()
        ),
        summary: format!("{path} · {what}"),
    }
}

/// 执行一个工具调用
pub fn execute_tool(call: &ToolCall, cwd: &Path, lang: Lang) -> ToolResult {
    match call {
        ToolCall::Write { content, path } => run_write(content, path, cwd, lang),
        ToolCall::Read { path, range } => run_read(path, *range, cwd, lang),
        ToolCall::List { path } => run_list(path, cwd, lang),
        ToolCall::Exec { command } => run_exec(command, cwd, lang),
        ToolCall::Search { query } => run_search(query, cwd, lang),
        ToolCall::Edit {
            content,
            path,
            from,
            to,
        } => run_edit(content, path, *from, *to, cwd),
    }
}


