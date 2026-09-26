//! SSE 流解析：把 DeepSeek 的 p/o/v patch 协议转换为结构化事件
//!
//! 协议要点：p / o 跨事件持久化、o 默认 SET、BATCH 递归分解（子路径前置父路径）、
//! 用 fragments 的 type 区分 THINK / RESPONSE、status=FINISHED/INCOMPLETE 视为终止。

use serde_json::Value;

/// 一条联网搜索来源
#[derive(Debug, Clone, PartialEq)]
pub struct SearchItem {
    pub url: String,
    pub title: String,
    /// 站点名（标题为空时用它兜底，比裸 URL 好看）
    pub site_name: String,
    /// 与正文里 `[citation:N]` 对应的编号
    pub cite_index: u32,
}

/// 精简后的流事件
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    ThinkStart,
    ThinkDelta(String),
    ContentStart,
    ContentDelta(String),
    /// 联网搜索的来源列表。
    ///
    /// 服务端会先下发一个 `type: SEARCH` 的片段，随后用
    /// `response/fragments/-1/results` 这个补丁把来源数组补上；
    /// 不是每次搜索都会有（`search_triggered` 为假时就没有）。
    SearchResults { items: Vec<SearchItem> },
    Done {
        finish_reason: Option<String>,
        usage: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Init,
    Thinking,
    Content,
    Done,
}

#[derive(Debug, Clone)]
struct Fragment {
    ty: String,
    content: String,
}

/// 上游 hint 事件（限流等）
#[derive(Debug, Clone)]
pub struct HintError {
    pub message: String,
    pub overloaded: bool,
}

impl std::fmt::Display for HintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for HintError {}

/// patch 状态机
#[derive(Debug, Default)]
struct PatchState {
    current_path: Option<String>,
    current_op: Option<String>,
    fragments: Vec<Fragment>,
    status: Option<String>,
    usage: Option<u64>,
    phase: Option<Phase>,
}

impl PatchState {
    fn phase(&self) -> Phase {
        self.phase.unwrap_or(Phase::Init)
    }

    /// 消费一帧 SSE 文本
    fn apply_frame(&mut self, frame: &str) -> Result<Vec<StreamEvent>, HintError> {
        if frame.trim().is_empty() {
            return Ok(vec![]);
        }
        let mut event_type = None;
        let mut data_line = None;
        for line in frame.split('\n') {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("event:") {
                event_type = Some(rest.trim().to_string());
            } else if let Some(rest) = trimmed.strip_prefix("data:") {
                data_line = Some(rest.trim().to_string());
            }
        }

        if event_type.as_deref() == Some("hint") {
            if let Some(data) = data_line {
                return Err(hint_to_error(&data));
            }
        }
        let Some(data) = data_line else {
            return Ok(vec![]);
        };
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            return Ok(vec![]);
        };

        let mut events = self.apply_patch(&value);
        events = self.finalize(events);
        Ok(events)
    }

    fn apply_patch(&mut self, val: &Value) -> Vec<StreamEvent> {
        if let Some(p) = val.get("p").and_then(|v| v.as_str()) {
            self.current_path = Some(p.to_string());
        }
        if let Some(o) = val.get("o").and_then(|v| v.as_str()) {
            self.current_op = Some(o.to_string());
        }
        let op = self.current_op.clone().unwrap_or_else(|| "SET".to_string());
        let path = self.current_path.clone().unwrap_or_default();

        let Some(v) = val.get("v") else {
            return vec![];
        };

        // 初始快照：无路径且含 response
        if self.current_path.is_none() {
            if let Some(response) = v.get("response").filter(|r| r.is_object()) {
                return self.apply_initial_snapshot(response);
            }
        }

        // 联网搜索的来源列表。放在 BATCH 判断之前：它通常是单独一条下发，
        // 但若哪天带上 BATCH 标记，走 BATCH 分支会被当成普通补丁数组漏掉。
        // 判据是「路径以 /results 结尾」+「确实是含 url 的对象数组」。
        if path.ends_with("/results") {
            if let Some(items) = parse_search_items(v) {
                return vec![StreamEvent::SearchResults { items }];
            }
        }

        if op == "BATCH" {
            if let Some(arr) = v.as_array() {
                return self.apply_batch(&path, arr);
            }
        }

        self.apply_path(&path, &op, v)
    }

    fn apply_batch(&mut self, parent_path: &str, arr: &[Value]) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        let mut sub_path = String::new();
        let mut sub_op = "SET".to_string();

        for item in arr {
            if !item.is_object() {
                continue;
            }
            if let Some(p) = item.get("p").and_then(|v| v.as_str()) {
                sub_path = p.to_string();
            }
            if let Some(o) = item.get("o").and_then(|v| v.as_str()) {
                sub_op = o.to_string();
            }
            let Some(v) = item.get("v") else { continue };

            let full_path = if parent_path.is_empty() {
                sub_path.clone()
            } else if sub_path.is_empty() {
                parent_path.to_string()
            } else {
                format!("{parent_path}/{sub_path}")
            };

            if sub_op == "BATCH" {
                let nested = v.as_array().cloned().unwrap_or_default();
                events.extend(self.apply_batch(&full_path, &nested));
            } else {
                events.extend(self.apply_path(&full_path, &sub_op, v));
            }
        }
        events
    }

    fn apply_initial_snapshot(&mut self, response: &Value) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if let Some(status) = response.get("status").and_then(|v| v.as_str()) {
            self.status = Some(status.to_string());
        }
        if let Some(usage) = response.get("accumulated_token_usage").and_then(|v| v.as_u64()) {
            self.usage = Some(usage);
        }
        if let Some(frags) = response.get("fragments").and_then(|v| v.as_array()) {
            self.fragments.clear();
            for frag in frags {
                let Some(ty) = frag.get("type").and_then(|v| v.as_str()) else {
                    continue;
                };
                let content = frag
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                self.fragments.push(Fragment {
                    ty: ty.to_string(),
                    content: content.clone(),
                });
                events.extend(delta_for(ty, &content));
            }
        }
        events
    }

    fn apply_path(&mut self, path: &str, op: &str, val: &Value) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        let normalized = path.strip_prefix('/').unwrap_or(path);

        match normalized {
            "response/status" => {
                if let Some(status) = val.as_str() {
                    self.status = Some(status.to_string());
                }
            }
            "response/accumulated_token_usage" => {
                if let Some(usage) = val.as_u64() {
                    self.usage = Some(usage);
                }
            }
            "response/fragments/-1/content" => {
                if let Some(text) = val.as_str() {
                    if let Some(last) = self.fragments.last_mut() {
                        last.content.push_str(text);
                        let ty = last.ty.clone();
                        events.extend(delta_for(&ty, text));
                    }
                }
            }
            "response/fragments" if op == "APPEND" => {
                if let Some(arr) = val.as_array() {
                    for item in arr {
                        let Some(ty) = item.get("type").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        let content = item
                            .get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        self.fragments.push(Fragment {
                            ty: ty.to_string(),
                            content: content.clone(),
                        });
                        events.extend(delta_for(ty, &content));
                    }
                }
            }
            _ => {}
        }
        events
    }

    /// 插入阶段切换信号，并在状态终止时追加 done
    fn finalize(&mut self, events: Vec<StreamEvent>) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        for evt in events {
            match &evt {
                StreamEvent::ThinkDelta(_)
                    if self.phase() == Phase::Init || self.phase() == Phase::Content =>
                {
                    self.phase = Some(Phase::Thinking);
                    out.push(StreamEvent::ThinkStart);
                }
                StreamEvent::ContentDelta(_)
                    if self.phase() == Phase::Init || self.phase() == Phase::Thinking =>
                {
                    self.phase = Some(Phase::Content);
                    out.push(StreamEvent::ContentStart);
                }
                _ => {}
            }
            out.push(evt);
        }

        if matches!(self.status.as_deref(), Some("FINISHED") | Some("INCOMPLETE"))
            && self.phase() != Phase::Done
        {
            self.phase = Some(Phase::Done);
            out.push(StreamEvent::Done {
                finish_reason: if self.status.as_deref() == Some("FINISHED") {
                    Some("stop".to_string())
                } else {
                    None
                },
                usage: self.usage,
            });
        }
        out
    }

    /// 流结束兜底
    fn flush(&mut self) -> Vec<StreamEvent> {
        self.finalize(vec![])
    }
}

/// 从补丁里取出搜索来源。形状不认识就返回 None ——
/// 宁可少显示，也不要把无关的数组当成来源列出来。
fn parse_search_items(val: &Value) -> Option<Vec<SearchItem>> {
    let items: Vec<SearchItem> = val
        .as_array()?
        .iter()
        .filter_map(|it| {
            let url = it.get("url").and_then(|v| v.as_str())?.to_string();
            Some(SearchItem {
                url,
                title: it
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                site_name: it
                    .get("site_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                cite_index: it
                    .get("cite_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32,
            })
        })
        .collect();
    (!items.is_empty()).then_some(items)
}

fn delta_for(frag_type: &str, content: &str) -> Vec<StreamEvent> {
    if content.is_empty() {
        return vec![];
    }
    match frag_type {
        "THINK" => vec![StreamEvent::ThinkDelta(content.to_string())],
        "RESPONSE" => vec![StreamEvent::ContentDelta(content.to_string())],
        _ => vec![],
    }
}

fn hint_to_error(data: &str) -> HintError {
    let mut content = "(unknown)".to_string();
    if let Ok(val) = serde_json::from_str::<Value>(data) {
        let raw = val
            .get("content")
            .or_else(|| val.get("finish_reason"))
            .and_then(|v| v.as_str());
        if let Some(text) = raw {
            content = text.to_string();
        }
    }
    HintError {
        overloaded: content.contains("rate_limit"),
        message: content,
    }
}

/// SSE 解析器：喂入文本块，产出结构化事件
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: String,
    state: PatchState,
    finished: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否已收到终止状态
    pub fn done(&self) -> bool {
        self.finished || self.state.phase() == Phase::Done
    }

    /// 推送一段文本
    pub fn push(&mut self, chunk: &str) -> Result<Vec<StreamEvent>, HintError> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();
        while let Some(idx) = self.buffer.find("\n\n") {
            let frame = self.buffer[..idx].to_string();
            self.buffer = self.buffer[idx + 2..].to_string();
            events.extend(self.state.apply_frame(&frame)?);
        }
        if self.state.phase() == Phase::Done {
            self.finished = true;
        }
        Ok(events)
    }

    /// 流结束，冲刷缓冲区
    pub fn flush(&mut self) -> Result<Vec<StreamEvent>, HintError> {
        let mut events = Vec::new();
        if !self.buffer.trim().is_empty() {
            let frame = std::mem::take(&mut self.buffer);
            events.extend(self.state.apply_frame(&frame)?);
        }
        events.extend(self.state.flush());
        if self.state.phase() == Phase::Done {
            self.finished = true;
        }
        Ok(events)
    }
}

/// 判断错误是否可重试（限流 / 瞬时故障）
pub fn is_retryable(error: &crate::deepseek::DsError) -> bool {
    use crate::deepseek::DsError;
    match error {
        DsError::Hint(_, overloaded) => *overloaded,
        DsError::Api { code, .. } => *code == 1001 || *code == 1201,
        // WAF 不可重试（重试也没用，需要换代理）
        DsError::Waf => false,
        _ => false,
    }
}

/// 从原始片段里用正则近似提取 response_message_id
pub fn extract_message_id(raw: &str) -> Option<u64> {
    let key = "response_message_id";
    let mut search_from = 0;
    while let Some(pos) = raw[search_from..].find(key) {
        let start = search_from + pos + key.len();
        let rest = &raw[start..];
        let rest = rest.trim_start();
        if let Some(after_quote) = rest.strip_prefix('"') {
            let rest = after_quote.trim_start();
            if let Some(after_colon) = rest.strip_prefix(':') {
                let digits: String = after_colon
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if !digits.is_empty() {
                    return digits.parse().ok();
                }
            }
        }
        search_from = start;
    }
    None
}
