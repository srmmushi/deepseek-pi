use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::agent::{format_ms, UiEvent};
use crate::config::Lang;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const BANNER: [&str; 6] = [
    "██████╗ ███████╗ ██████╗ ",
    "██╔══██╗██╔════╝ ██╔══██╗",
    "██║  ██║███████╗ ██████╔╝",
    "██║  ██║╚════██║ ██╔═══╝ ",
    "██████╔╝███████║ ██║     ",
    "╚═════╝ ╚══════╝ ╚═╝     ",
];

/// banner 右侧说明文字的起始列（2 缩进 + 24 字形 + 2 间隔）。
/// 让它贴着字形，而不是像表格一样甩到终端最右边。
const BANNER_LABEL_COL: usize = 28;

// 工具名各给一个颜色，扫一眼就知道模型在干什么
fn tool_color(name: &str) -> Color {
    match name {
        "read" => Color::Cyan,
        "write" => Color::Green,
        "list" => Color::Blue,
        "exec" => Color::Yellow,
        "search" => Color::Magenta,
        _ => Color::Gray,
    }
}

pub fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn user_style() -> Style {
    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
}

pub fn ok() -> Style {
    Style::default().fg(Color::Green)
}

pub fn err() -> Style {
    Style::default().fg(Color::Red)
}

pub fn warn() -> Style {
    Style::default().fg(Color::Yellow)
}

/// Ctrl+T / Ctrl+S 想切换的开关。
/// `on_key` 里拿不到 core.config，所以只记一个请求，由主循环落地。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    Thinking,
    Search,
}

/// 扁排后的一屏行
struct Row {
    text: String,
    /// 与 text 同行右对齐的说明（banner 用）
    right: Option<(String, Style)>,
    style: Style,
    owner: Option<usize>,
}

struct Item {
    head: String,
    head_style: Style,
    right: Option<(String, Style)>,
    body: Vec<String>,
    body_style: Style,
    collapsed: bool,
    foldable: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct Pos {
    row: usize,
    col: usize,
}

/// /goto 弹层
struct Goto {
    items: Vec<(String, usize)>,
    cursor: usize,
}

pub struct App {
    items: Vec<Item>,
    rows: Vec<Row>,
    offset: usize,
    /// 上次绘制时的可见窗口 (起始行, 高)，鼠标坐标换算要用
    view: (usize, usize),

    input: String,
    cursor: usize,
    status: String,
    hint: String,

    busy: bool,
    think_since: Option<Instant>,
    spinner: usize,
    /// 本次思考已收到的字数，只用于状态栏
    think_chars: usize,
    pub last_thinking: String,

    /// 登录输入模式：下一次提交当作 token，不发给模型、也不进会话记录
    login_mode: bool,
    login_input: Option<String>,

    /// Ctrl+T / Ctrl+S 的切换请求，交给主循环去改 core.config
    toggle_request: Option<Toggle>,

    anchors: Vec<(String, usize)>,
    selection: Option<(Pos, Pos)>,
    origin: Option<Pos>,
    dragging: bool,
    copied: Option<String>,

    goto: Option<Goto>,
    pending: Option<String>,
    pub quit: bool,
}

impl Default for App {
    fn default() -> Self {
        App {
            items: Vec::new(),
            rows: Vec::new(),
            offset: 0,
            view: (0, 0),
            input: String::new(),
            cursor: 0,
            status: String::new(),
            hint: String::new(),
            busy: false,
            think_since: None,
            spinner: 0,
            think_chars: 0,
            last_thinking: String::new(),
            login_mode: false,
            login_input: None,
            toggle_request: None,
            anchors: Vec::new(),
            selection: None,
            origin: None,
            dragging: false,
            copied: None,
            goto: None,
            pending: None,
            quit: false,
        }
    }
}

impl App {
    pub fn line(&mut self, text: impl Into<String>) {
        self.line_styled(text, Style::default());
    }

    pub fn line_styled(&mut self, text: impl Into<String>, style: Style) {
        self.items.push(Item {
            head: text.into(),
            head_style: style,
            right: None,
            body: Vec::new(),
            body_style: style,
            collapsed: false,
            foldable: false,
        });
    }

    pub fn block(&mut self, head: String, style: Style, body: Vec<String>, collapsed: bool) {
        self.items.push(Item {
            head,
            head_style: style,
            right: None,
            body,
            body_style: dim(),
            collapsed,
            foldable: true,
        });
    }

    /// 启动头：ASCII 字形，右侧配版本与登录状态
    pub fn banner(&mut self, lang: Lang, token_len: Option<usize>) {
        let zh = lang == Lang::Zh;
        let status = match token_len {
            Some(n) => (
                if zh {
                    format!("已登录 · token {n} 字符")
                } else {
                    format!("signed in · token {n}")
                },
                ok(),
            ),
            None => (
                if zh {
                    "未登录 · 输入 /login".to_string()
                } else {
                    "not signed in · run /login".to_string()
                },
                warn(),
            ),
        };

        self.line("");
        for (i, art) in BANNER.iter().enumerate() {
            let right = match i {
                1 => Some(("DSP  (deepseek-pi)".to_string(), Style::default().add_modifier(Modifier::BOLD))),
                2 => Some(status.clone()),
                _ => None,
            };
            self.items.push(Item {
                head: format!("  {art}"),
                head_style: Style::default().fg(Color::Cyan),
                right,
                body: Vec::new(),
                body_style: dim(),
                collapsed: false,
                foldable: false,
            });
        }
        self.line("");
    }

    /// 记一个提示词锚点，指向它即将占用的那一行
    pub fn anchor(&mut self, label: String) {
        self.anchors.push((label, self.rows.len()));
    }

    pub fn apply(&mut self, ev: UiEvent) {
        match ev {
            UiEvent::Line(text) => self.line(text),
            UiEvent::ThinkStart => {
                self.think_since = Some(Instant::now());
                self.spinner = 0;
                self.think_chars = 0;
            }
            UiEvent::ThinkProgress { chars } => {
                self.spinner = self.spinner.wrapping_add(1);
                self.think_chars = chars;
            }
            UiEvent::ThinkEnd { text, ms } => {
                self.think_since = None;
                // 空思考（没开思考 / 模型没输出思考）只清状态，不留一行噪声
                if text.trim().is_empty() {
                    return;
                }
                let chars = text.chars().count();
                let head = format!("▌ 思考 {} · {chars} 字 · Ctrl+O 展开", format_ms(ms));
                let body = text
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| format!("    {l}"))
                    .collect();
                self.last_thinking = text;
                self.block(head, dim(), body, true);
            }
            UiEvent::ToolBatchStart(calls) => {
                self.line("");
                for call in calls {
                    let tool = call
                        .trim_start_matches('▌')
                        .trim()
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_string();
                    self.line_styled(call, Style::default().fg(tool_color(&tool)));
                }
            }
            UiEvent::ToolEnd { tool, result, ms, parallel } => {
                let name = if parallel { format!("{tool:<6} ") } else { String::new() };
                let text = format!("└ {name}{}  {}", result.summary, format_ms(ms));
                self.line_styled(text, if result.ok { Style::default().fg(Color::Gray) } else { err() });
            }
            UiEvent::TurnDone { usage, ms, gen_ms } => {
                let mut bits = Vec::new();
                if let Some(u) = usage {
                    bits.push(format!("{u} tokens"));
                    // 速率只按「流式生成」那段算：把工具执行时间算进去会虚低得离谱
                    if gen_ms > 0 {
                        bits.push(format!("{:.1} tok/s", u as f64 * 1000.0 / gen_ms as f64));
                    }
                }
                bits.push(format_ms(ms));
                self.line("");
                self.line_styled(format!("· {}", bits.join("  ·  ")), dim());
                self.busy = false;
            }
            UiEvent::Notice(text) => self.line_styled(format!("! {text}"), warn()),
            UiEvent::Error(text) => {
                self.busy = false;
                self.think_since = None;
                self.line_styled(format!("! {text}"), err());
            }
            UiEvent::AuthFailed => {
                self.line_styled("凭证已失效，本地凭证已清除，请重新登录。", warn());
            }
            UiEvent::Finished => {
                self.busy = false;
                self.think_since = None;
            }
        }
    }

    pub fn set_busy(&mut self, value: bool) {
        self.busy = value;
        if value {
            self.offset = 0;
        }
    }

    pub fn set_status(&mut self, text: String) {
        self.status = text;
    }

    pub fn set_hint(&mut self, text: String) {
        self.hint = text;
    }

    pub fn take_pending(&mut self) -> Option<String> {
        self.pending.take()
    }

    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    /// /login：切到 token 输入模式（提示符随之改变）
    pub fn start_login(&mut self) {
        self.login_mode = true;
        self.input.clear();
        self.cursor = 0;
    }

    fn cancel_login(&mut self) {
        self.login_mode = false;
        self.input.clear();
        self.cursor = 0;
        self.line_styled("已取消登录。", dim());
    }

    /// 取走用户粘贴的 token
    pub fn take_login_input(&mut self) -> Option<String> {
        self.login_input.take()
    }

    /// 取走 Ctrl+T / Ctrl+S 的切换请求
    pub fn take_toggle(&mut self) -> Option<Toggle> {
        self.toggle_request.take()
    }

    /// 交给主循环写剪贴板（这里只负责把文本准备好）
    pub fn take_copied(&mut self) -> Option<String> {
        self.copied.take()
    }

    /// /goto <序号>：直接跳到第 n 条提示词
    pub fn goto_index(&mut self, index: usize) {
        match self.anchors.get(index) {
            Some((_, row)) => {
                let row = *row;
                self.jump_to_row(row);
            }
            None => self.line_styled(
                format!("编号超出范围（共 {} 条）。", self.anchors.len()),
                warn(),
            ),
        }
    }

    pub fn open_goto(&mut self) {
        if self.anchors.is_empty() {
            self.line_styled("当前会话还没有发送过提示词。", dim());
            return;
        }
        self.goto = Some(Goto {
            items: self.anchors.clone(),
            cursor: 0,
        });
    }

    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        if self.goto.is_some() {
            self.goto_key(key);
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => self.copy_or_clear_selection(),
                KeyCode::Char('o') => self.toggle_last(),
                // 只记请求，真正的开关在主循环里翻（见 Toggle）
                KeyCode::Char('t') => self.toggle_request = Some(Toggle::Thinking),
                KeyCode::Char('s') => self.toggle_request = Some(Toggle::Search),
                KeyCode::Down => self.offset = 0,
                KeyCode::Up => self.scroll(1 << 20),
                _ => {}
            }
            return false;
        }

        match key.code {
            KeyCode::Esc => {
                if self.login_mode {
                    self.cancel_login();
                } else if self.selection.is_some() {
                    self.selection = None;
                } else {
                    return true;
                }
            }
            KeyCode::Enter => {
                let text = std::mem::take(&mut self.input);
                self.cursor = 0;
                if self.login_mode {
                    // token 只交给主循环去保存，不写进会话记录
                    self.login_mode = false;
                    if !text.trim().is_empty() {
                        self.login_input = Some(text.trim().to_string());
                    }
                } else if !text.trim().is_empty() {
                    self.pending = Some(text);
                }
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => {
                if self.cursor < self.input.chars().count() {
                    self.cursor += 1;
                    self.backspace();
                }
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => {
                if self.cursor < self.input.chars().count() {
                    self.cursor += 1;
                }
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.chars().count(),
            KeyCode::Up => self.scroll(1),
            KeyCode::Down => self.scroll(-1),
            KeyCode::PageUp => self.scroll(10),
            KeyCode::PageDown => self.scroll(-10),
            KeyCode::Char(c) => self.insert(c),
            _ => {}
        }
        false
    }

    fn goto_key(&mut self, key: KeyEvent) {
        let Some(goto) = self.goto.as_mut() else { return };
        match key.code {
            KeyCode::Esc => self.goto = None,
            KeyCode::Up | KeyCode::Char('k') => goto.cursor = goto.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                if goto.cursor + 1 < goto.items.len() {
                    goto.cursor += 1;
                }
            }
            KeyCode::Enter => {
                let row = goto.items[goto.cursor].1;
                self.goto = None;
                self.jump_to_row(row);
            }
            _ => {}
        }
    }

    /// 把目标行滚到窗口顶部
    fn jump_to_row(&mut self, row: usize) {
        let total = self.rows.len();
        let height = self.view.1.max(1);
        self.offset = total.saturating_sub(row).saturating_sub(height - 1);
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) {
        match ev.kind {
            MouseEventKind::ScrollUp => self.scroll(3),
            MouseEventKind::ScrollDown => self.scroll(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                self.origin = self.hit(ev.column, ev.row);
                self.dragging = false;
                self.selection = None;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let (Some(origin), Some(pos)) = (self.origin, self.hit(ev.column, ev.row)) else {
                    return;
                };
                if pos != origin {
                    self.dragging = true;
                }
                self.selection = Some((origin, pos));
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let Some(origin) = self.origin.take() else { return };
                if self.dragging {
                    // 拖拽结束：留着高亮，等右键 / Ctrl+C 复制
                    self.dragging = false;
                    return;
                }
                // 没拖动就是单击，折叠对应的块
                self.click_fold(origin.row);
            }
            MouseEventKind::Down(MouseButton::Right) => self.copy_or_clear_selection(),
            _ => {}
        }
    }

    fn hit(&self, col: u16, screen_row: u16) -> Option<Pos> {
        let (start, height) = self.view;
        let line = screen_row as usize;
        if line >= height || start + line >= self.rows.len() {
            return None;
        }
        Some(Pos {
            row: start + line,
            col: col as usize,
        })
    }

    fn click_fold(&mut self, row: usize) {
        let Some(owner) = self.rows.get(row).and_then(|r| r.owner) else {
            return;
        };
        if let Some(item) = self.items.get_mut(owner) {
            if item.foldable {
                item.collapsed = !item.collapsed;
            }
        }
    }

    fn copy_or_clear_selection(&mut self) {
        if let Some(text) = self.selection_text() {
            self.copied = Some(text);
        }
        self.selection = None;
    }

    fn selection_text(&self) -> Option<String> {
        let (a, b) = self.selection?;
        let (from, to) = if a.row < b.row || (a.row == b.row && a.col <= b.col) {
            (a, b)
        } else {
            (b, a)
        };
        let mut out = Vec::new();
        for row in from.row..=to.row {
            let Some(line) = self.rows.get(row) else { continue };
            let chars: Vec<char> = line.text.chars().collect();
            let start = if row == from.row { from.col } else { 0 };
            let end = if row == to.row { to.col.min(chars.len()) } else { chars.len() };
            if start < end {
                out.push(chars[start..end].iter().collect::<String>());
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out.join("\n").trim_end().to_string())
        }
    }

    /// 复制完成后由主循环回一句提示
    pub fn notice_copied(&mut self, chars: usize) {
        self.line_styled(format!("已复制 {chars} 个字符到剪贴板"), dim());
    }

    fn insert(&mut self, c: char) {
        let at = byte_index(&self.input, self.cursor);
        self.input.insert(at, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = byte_index(&self.input, self.cursor - 1);
        self.input.remove(at);
        self.cursor -= 1;
    }

    pub fn scroll(&mut self, delta: isize) {
        let max = self.rows.len().saturating_sub(1) as isize;
        self.offset = (self.offset as isize + delta).clamp(0, max) as usize;
    }

    pub fn toggle_last(&mut self) {
        if let Some(item) = self.items.iter_mut().rev().find(|i| i.foldable) {
            item.collapsed = !item.collapsed;
        }
    }

    pub fn push_thinking(&mut self, text: &str) {
        if text.trim().is_empty() {
            self.line_styled("没有可展开的思考内容。", dim());
            return;
        }
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            self.line_styled(format!("    {line}"), dim());
        }
    }

    fn reflow(&mut self, width: usize) {
        let width = width.max(24);
        let mut rows = Vec::with_capacity(self.rows.len());
        for (index, item) in self.items.iter().enumerate() {
            for seg in wrap(&item.head, width) {
                rows.push(Row {
                    text: seg,
                    right: item.right.clone(),
                    style: item.head_style,
                    owner: Some(index),
                });
            }
            if item.foldable && item.collapsed {
                continue;
            }
            for line in &item.body {
                for seg in wrap(line, width) {
                    rows.push(Row {
                        text: seg,
                        right: None,
                        style: item.body_style,
                        owner: None,
                    });
                }
            }
        }
        self.rows = rows;
    }

    pub fn draw(&mut self, frame: &mut Frame, lang: Lang) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        let view = chunks[0];
        self.reflow(view.width as usize);
        let height = view.height as usize;
        let total = self.rows.len();
        let end = total.saturating_sub(self.offset);
        let start = end.saturating_sub(height);
        self.view = (start, height);

        let width = view.width as usize;
        let lines: Vec<Line> = self.rows[start..end]
            .iter()
            .enumerate()
            .map(|(i, row)| self.render_row(row, start + i, width))
            .collect();
        frame.render_widget(Paragraph::new(lines), view);

        let input_area = chunks[1];
        let (prompt, prompt_style) = if self.login_mode {
            ("token › ", Style::default().fg(Color::Yellow))
        } else if self.input.starts_with('!') {
            ("! ", Style::default().fg(Color::Yellow))
        } else {
            ("❯ ", Style::default().fg(Color::Cyan))
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prompt, prompt_style.add_modifier(Modifier::BOLD)),
                Span::raw(self.input.clone()),
            ])),
            input_area,
        );
        // 光标位置按提示符的实际宽度算，别写死 2
        let offset = prompt.chars().count() as u16;
        let x = (input_area.x + offset + self.cursor as u16)
            .min(input_area.right().saturating_sub(1));
        frame.set_cursor_position((x, input_area.y));

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                self.status_text(lang),
                if self.busy { Style::default().fg(Color::Yellow) } else { dim() },
            ))),
            chunks[2],
        );

        if let Some(goto) = &self.goto {
            self.draw_goto(frame, area, goto, lang);
        }
    }

    fn render_row(&self, row: &Row, index: usize, width: usize) -> Line<'static> {
        let mut spans = self.selection_spans(row, index);
        if let Some((right, style)) = &row.right {
            let used = spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>();
            // 贴在字形右侧的固定列；窗口窄到塞不下时退化成 1 个空格
            let pad = BANNER_LABEL_COL.min(width).saturating_sub(used).max(1);
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(right.clone(), *style));
        }
        Line::from(spans)
    }

    fn selection_spans(&self, row: &Row, index: usize) -> Vec<Span<'static>> {
        let plain = || vec![Span::styled(row.text.clone(), row.style)];
        let Some((a, b)) = self.selection else {
            return plain();
        };
        let (from, to) = if a.row < b.row || (a.row == b.row && a.col <= b.col) {
            (a, b)
        } else {
            (b, a)
        };
        if index < from.row || index > to.row {
            return plain();
        }
        let chars: Vec<char> = row.text.chars().collect();
        let s = if index == from.row { from.col.min(chars.len()) } else { 0 };
        let e = if index == to.row { to.col.min(chars.len()) } else { chars.len() };
        let head: String = chars[..s].iter().collect();
        let mid: String = chars[s..e].iter().collect();
        let tail: String = chars[e..].iter().collect();
        vec![
            Span::styled(head, row.style),
            Span::styled(mid, row.style.add_modifier(Modifier::REVERSED)),
            Span::styled(tail, row.style),
        ]
    }

    fn status_text(&self, lang: Lang) -> String {
        if self.login_mode {
            return match lang {
                Lang::Zh => "把 chat.deepseek.com 的 userToken 粘贴进来，回车确认 · Esc 取消".to_string(),
                Lang::En => "paste the userToken from chat.deepseek.com, Enter to confirm · Esc cancels".to_string(),
            };
        }
        if self.goto.is_some() {
            return match lang {
                Lang::Zh => "↑/↓ 选择 · Enter 跳转 · Esc 取消".to_string(),
                Lang::En => "↑/↓ select · Enter jump · Esc cancel".to_string(),
            };
        }
        if self.busy {
            let secs = self
                .think_since
                .map(|t| t.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            if self.think_since.is_some() {
                let word = if lang == Lang::Zh { "思考" } else { "Thinking" };
                let unit = if lang == Lang::Zh { "字" } else { "chars" };
                let chars = self.think_chars;
                return format!(
                    "{} {word} {secs:.1}s · {chars}{unit}    {}",
                    SPINNER[self.spinner % SPINNER.len()],
                    self.status
                );
            }
            return format!("◆ {secs:.1}s    {}", self.status);
        }
        if self.offset > 0 {
            return format!("↑{}    {}", self.offset, self.status);
        }
        self.status.clone()
    }

    fn draw_goto(&self, frame: &mut Frame, area: Rect, goto: &Goto, lang: Lang) {
        let title = match lang {
            Lang::Zh => "跳转到提示词",
            Lang::En => "Jump to a prompt",
        };
        let width = (area.width * 3 / 4).clamp(40, 100).min(area.width);
        let height = (goto.items.len() as u16 + 2).min(area.height.saturating_sub(4)).max(3);
        let popup = centered(area, width, height);

        frame.render_widget(Clear, popup);
        let items: Vec<Line> = goto
            .items
            .iter()
            .enumerate()
            .map(|(i, (label, _))| {
                let (marker, style) = if i == goto.cursor {
                    (
                        "▸",
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    )
                } else {
                    (" ", Style::default())
                };
                Line::from(Span::styled(format!("{marker} #{:<3}{label}", i + 1), style))
            })
            .collect();
        frame.render_widget(
            Paragraph::new(items).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {title} "))
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
            popup,
        );
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    if text.chars().count() <= width {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut used = 0;
    for ch in text.chars() {
        if used == width {
            out.push(std::mem::take(&mut buf));
            used = 0;
        }
        buf.push(ch);
        used += 1;
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}
