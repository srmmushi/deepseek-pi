//! Ratatui 界面：会话记录（可滚动、可折叠）+ 输入行 + 状态栏
//!
//! 与 TS 版的重要差异：Ratatui 每帧整屏重绘，所以不需要 TS 里那套
//! DECSTBM 滚动区域 + 手工光标归位的技巧，也没有「\n 不回列」的坑。
//! 这里唯一要自己做的是：把长行预先折好，保证「一个逻辑行 = 一个屏幕行」，
//! 这样鼠标/滚动的行号映射才是精确的。

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as RBlock, Borders, Paragraph};
use ratatui::Frame;

use crate::agent::{format_ms, UiEvent};

/// 一行文本 + 它所属的可折叠块下标
#[derive(Debug, Clone)]
pub struct Row {
    pub text: String,
    pub owner: Option<usize>,
    pub style: Style,
}

/// 可折叠块（普通文本行也用一个不可折叠的块表示，便于统一渲染）
#[derive(Debug, Clone)]
pub struct Item {
    pub head: String,
    pub head_style: Style,
    pub body: Vec<String>,
    pub body_style: Style,
    pub collapsed: bool,
    pub foldable: bool,
}

/// 思考 spinner 帧
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// 界面状态
pub struct App {
    pub items: Vec<Item>,
    /// 扁平化后的行（每行恰好占一屏行）
    pub rows: Vec<Row>,
    /// 从底部向上滚动的行数（0 = 贴底）
    pub offset: usize,
    pub input: String,
    pub cursor: usize,
    pub hint: String,
    pub status: String,
    pub busy: bool,
    /// 思考中的起始时刻（用于 spinner 计时）
    pub thinking_since: Option<std::time::Instant>,
    pub spinner: usize,
    /// 最近一次思考全文（Ctrl+O 展开回放）
    pub last_thinking: String,
    /// 用户提示词的锚点行号（/goto 用）
    pub anchors: Vec<(String, usize)>,
    /// 输入被提交前的回显，交给主循环处理
    pub pending_input: Option<String>,
    pub should_quit: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            rows: Vec::new(),
            offset: 0,
            input: String::new(),
            cursor: 0,
            hint: String::new(),
            status: String::new(),
            busy: false,
            thinking_since: None,
            spinner: 0,
            last_thinking: String::new(),
            anchors: Vec::new(),
            pending_input: None,
            should_quit: false,
        }
    }
}

impl App {
    /// 追加一行普通文本（不可折叠）
    pub fn push_line(&mut self, text: impl Into<String>) {
        self.items.push(Item {
            head: text.into(),
            head_style: Style::default(),
            body: Vec::new(),
            body_style: Style::default(),
            collapsed: false,
            foldable: false,
        });
    }

    /// 追加带样式的普通行
    pub fn push_styled(&mut self, text: impl Into<String>, style: Style) {
        self.items.push(Item {
            head: text.into(),
            head_style: style,
            body: Vec::new(),
            body_style: style,
            collapsed: false,
            foldable: false,
        });
    }

    /// 追加可折叠块
    pub fn push_block(&mut self, head: String, body: Vec<String>, collapsed: bool) {
        self.items.push(Item {
            head,
            head_style: Style::default(),
            body,
            body_style: Style::default().fg(Color::DarkGray),
            collapsed,
            foldable: true,
        });
    }

    /// 记录一个提示词锚点（指向当前最后一行）
    pub fn anchor(&mut self, label: String) {
        let row = self.rows.len();
        self.anchors.push((label, row));
    }

    /// 处理后台线程事件
    pub fn apply(&mut self, event: UiEvent) {
        match event {
            UiEvent::Line(text) => self.push_line(text),
            UiEvent::Block {
                head,
                body,
                collapsed,
            } => self.push_block(head, body, collapsed),
            UiEvent::ThinkStart => {
                self.thinking_since = Some(std::time::Instant::now());
                self.spinner = 0;
            }
            UiEvent::ThinkProgress { .. } => {
                self.spinner = self.spinner.wrapping_add(1);
            }
            UiEvent::ThinkEnd { text, ms } => {
                self.thinking_since = None;
                self.last_thinking = text.clone();
                let chars = text.chars().count();
                let head = format!("▌ 思考 {} · {chars} 字 · Ctrl+O 展开", format_ms(ms));
                let body: Vec<String> = text
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| format!("    {l}"))
                    .collect();
                // 折叠态：只留一行摘要，正文按需展开
                self.push_block(head, body, true);
            }
            UiEvent::ToolBatchStart(lines) => {
                self.push_line("");
                for line in lines {
                    self.push_styled(line, Style::default().fg(Color::Cyan));
                }
            }
            UiEvent::ToolEnd {
                tool,
                result,
                ms,
                parallel,
            } => {
                let name = if parallel {
                    format!("{:<6}", tool)
                } else {
                    String::new()
                };
                let text = format!("└ {name}{}  {}", result.summary, format_ms(ms));
                let style = if result.ok {
                    Style::default().fg(Color::Gray)
                } else {
                    Style::default().fg(Color::Red)
                };
                self.push_styled(text, style);
            }
            UiEvent::TurnDone { usage, ms } => {
                let mut parts = Vec::new();
                if let Some(u) = usage {
                    parts.push(format!("{u} tokens"));
                }
                parts.push(format_ms(ms));
                self.push_line("");
                self.push_styled(
                    format!("· {}", parts.join("  ·  ")),
                    Style::default().fg(Color::DarkGray),
                );
                self.busy = false;
            }
            UiEvent::Notice(text) => {
                self.push_styled(format!("! {text}"), Style::default().fg(Color::Yellow));
            }
            UiEvent::Error(text) => {
                self.busy = false;
                self.push_styled(format!("! {text}"), Style::default().fg(Color::Red));
            }
            UiEvent::Finished => {
                self.busy = false;
                self.thinking_since = None;
            }
        }
    }

    /// 按当前宽度把 items 扁平化成「一逻辑行 = 一屏行」
    fn rebuild(&mut self, width: usize) {
        let width = width.max(20);
        let mut rows = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            for seg in wrap(&item.head, width) {
                rows.push(Row {
                    text: seg,
                    owner: Some(index),
                    style: item.head_style,
                });
            }
            if item.foldable && item.collapsed {
                continue;
            }
            for line in &item.body {
                for seg in wrap(line, width) {
                    rows.push(Row {
                        text: seg,
                        owner: None,
                        style: item.body_style,
                    });
                }
            }
        }
        // 折叠状态变化会让行数变化，锚点行号需要跟着失效保护
        self.rows = rows;
        let max_offset = self.rows.len().saturating_sub(1);
        if self.offset > max_offset {
            self.offset = max_offset;
        }
    }

    /// 渲染一帧
    pub fn draw(&mut self, frame: &mut Frame, lang_zh: bool) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        let transcript = chunks[0];
        self.rebuild(transcript.width as usize);

        // 可视窗口（底部对齐 + offset）
        let height = transcript.height as usize;
        let total = self.rows.len();
        let end = total.saturating_sub(self.offset);
        let start = end.saturating_sub(height);
        let visible = &self.rows[start..end];

        let lines: Vec<Line> = visible
            .iter()
            .map(|row| Line::from(Span::styled(row.text.clone(), row.style)))
            .collect();
        let para = Paragraph::new(lines).block(
            RBlock::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(para, transcript);

        // 输入行
        let prompt = if self.input.starts_with('!') {
            "! "
        } else {
            "❯ "
        };
        let input_line = Line::from(vec![
            Span::styled(prompt, Style::default().fg(Color::Cyan)),
            Span::raw(self.input.clone()),
        ]);
        let input_area: Rect = chunks[1];
        frame.render_widget(Paragraph::new(input_line), input_area);
        // 光标定位
        let cursor_x = input_area.x + 2 + self.cursor as u16;
        frame.set_cursor_position((cursor_x.min(input_area.right().saturating_sub(1)), input_area.y));

        // 状态栏
        let status_text = if self.busy {
            let secs = self
                .thinking_since
                .map(|t| t.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            if self.thinking_since.is_some() {
                let frame_ch = SPINNER[self.spinner % SPINNER.len()];
                format!("{frame_ch} 思考 {secs:.1}s   {}", self.status)
            } else {
                format!("◆ {secs:.1}s   {}", self.status)
            }
        } else if self.offset > 0 {
            format!("↑{}   {}", self.offset, self.status)
        } else {
            self.status.clone()
        };
        let _ = lang_zh;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                status_text,
                Style::default().fg(Color::DarkGray),
            ))),
            chunks[2],
        );
    }

    /// 上下滚动
    pub fn scroll(&mut self, delta: isize) {
        let max = self.rows.len().saturating_sub(1) as isize;
        let next = (self.offset as isize + delta).clamp(0, max);
        self.offset = next as usize;
    }

    /// 折叠 / 展开指定行所属的块
    pub fn toggle_at_row(&mut self, screen_row: usize, transcript_height: usize) {
        let total = self.rows.len();
        let end = total.saturating_sub(self.offset);
        let start = end.saturating_sub(transcript_height);
        let Some(row) = self.rows.get(start + screen_row) else {
            return;
        };
        let Some(index) = row.owner else { return };
        if let Some(item) = self.items.get_mut(index) {
            if item.foldable {
                item.collapsed = !item.collapsed;
            }
        }
    }

    /// 折叠 / 展开最近一个可折叠块（Ctrl+O）
    pub fn toggle_last(&mut self) {
        if let Some(item) = self.items.iter_mut().rev().find(|i| i.foldable) {
            item.collapsed = !item.collapsed;
        }
    }

    /// 插入一个字符
    pub fn insert_char(&mut self, ch: char) {
        let byte = char_byte_index(&self.input, self.cursor);
        self.input.insert(byte, ch);
        self.cursor += 1;
    }

    /// 退格
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let byte = char_byte_index(&self.input, self.cursor - 1);
        self.input.remove(byte);
        self.cursor -= 1;
    }

    /// 取走待提交的输入
    pub fn take_input(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.input)
    }
}

/// 字符下标 → 字节下标
fn char_byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

/// 按「字符数」粗略硬折行。
/// 注：这里用字符数而非显示宽度（CJK 会占两列），
/// 因此含大量中文的长行折行点会略有偏差，不影响功能。
fn wrap(text: &str, width: usize) -> Vec<String> {
    let count = text.chars().count();
    if count <= width {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        if used >= width {
            out.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(ch);
        used += 1;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// 会话记录的样式辅助
pub fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// 加粗
pub fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}
