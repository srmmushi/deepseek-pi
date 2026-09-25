//! Agent 层：一轮对话的完整流程（流式 → 解析工具调用 → 并行执行 → 回灌 → 继续）
//!
//! 跑在后台线程里，通过 mpsc 通道把界面事件推给 UI 线程；
//! 用户按 Esc 时置 `aborted`，流式回调返回 false 提前收手。
//!
//! 注意累积状态的传递方式：`stream_chat_with_retry` 只接受回调，
//! 而回调结束后调用方还要读回「本轮正文 / 思考全文」，
//! 所以用一个 `Arc<Mutex<Assembled>>` 在两边共享。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use crate::config::{AppConfig, ContextMode, Lang};
use crate::deepseek::{
    load_pow_solver, stream_chat_with_retry, DeepSeekClient, DsError, PowSolver, WebSessionHandle,
};
use crate::i18n::tr;
use crate::prompt::{build_tool_doc, tool_result_prefix};
use crate::stream::StreamEvent;
use crate::tools::{describe_call, execute_tool, parse_tool_calls, ToolCall, ToolResult};

/// 后台线程 → UI 的事件
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// 追加一行普通文本（不可折叠）
    Line(String),
    /// 思考开始（UI 起 spinner 计时）
    ThinkStart,
    /// 思考进度（只传字数，spinner 由 UI 绘制）
    ThinkProgress { chars: usize },
    /// 思考结束（带上全文，供展开回放）
    ThinkEnd { text: String, ms: u128 },
    /// 工具批次开始：调用一次性列出
    ToolBatchStart(Vec<String>),
    /// 单个工具结束
    ToolEnd {
        tool: String,
        result: ToolResult,
        ms: u128,
        parallel: bool,
    },
    /// 一轮结束。`gen_ms` 只统计流式生成耗时（不含工具执行），用于算 tok/s
    TurnDone {
        usage: Option<u64>,
        ms: u128,
        gen_ms: u128,
    },
    /// 非致命提示
    Notice(String),
    /// 登录流程在后台拿到了 userToken，交给主循环保存
    Token(String),
    /// 致命错误（已翻译成可读文案）
    Error(String),
    /// 凭证失效，界面应清掉本地凭证
    AuthFailed,
    /// 本轮请求处理完毕（无论成败）
    Finished,
}

/// 会话状态，落盘在 <config>/sessions/<id>.json
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub cwd: PathBuf,
    pub handle: WebSessionHandle,
    /// (role, content)，role 取 user / assistant / tool
    pub messages: Vec<(String, String)>,
    pub updated_at: u64,
}

impl Session {
    pub fn new(cwd: PathBuf, title: &str) -> Self {
        let now = crate::auth::now_ms();
        Self {
            id: format!("{now:x}"),
            title: title.to_string(),
            cwd,
            updated_at: now,
            ..Default::default()
        }
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        std::fs::write(dir.join(format!("{}.json", self.id)), format!("{text}\n"))
    }

    /// 按最近使用排序
    pub fn list(dir: &Path) -> Vec<Session> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out: Vec<Session> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|t| serde_json::from_str(&t).ok())
            .collect();
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        out
    }

    /// 取最近一次会话
    pub fn latest(dir: &Path) -> Option<Session> {
        Session::list(dir).into_iter().next()
    }
}

/// 共享给后台线程的运行环境
pub struct AgentRuntime {
    pub client: Arc<DeepSeekClient>,
    pub solver: Arc<Mutex<Option<PowSolver>>>,
    pub token: String,
    pub config: AppConfig,
    pub lang: Lang,
    /// 中断标志
    pub aborted: Arc<Mutex<bool>>,
}

/// 一轮流式过程中累积的状态
#[derive(Default)]
struct Assembled {
    /// 正文全文
    assistant: String,
    /// 思考全文
    think: String,
    /// 已收到的思考字符数（UI 只用来画进度）
    think_chars: usize,
    /// 已按行切分并上报的正文（避免重复上报）
    emitted_len: usize,
    /// 用于按行切分的残留
    pending: String,
    usage: Option<u64>,
    finish: Option<String>,
}

/// 把底层错误翻译成可读提示；第二个返回值表示「凭证失效，需要重新登录」
pub fn describe_error(lang: Lang, e: &DsError) -> (String, bool) {
    if e.is_auth() {
        return (tr(lang, "error.sessionExpired").to_string(), true);
    }
    if e.is_rate_limit() {
        return (tr(lang, "error.rateLimit").to_string(), false);
    }
    if matches!(e, DsError::Waf) {
        return (tr(lang, "error.waf").to_string(), false);
    }
    let msg = e.to_string();
    let lower = msg.to_lowercase();
    if lower.contains("pow") || lower.contains("wasm") {
        return (format!("{}：{msg}", tr(lang, "error.pow")), false);
    }
    if lower.contains("请求失败") || lower.contains("connect") || lower.contains("timed out") {
        return (format!("{}：{msg}", tr(lang, "error.http")), false);
    }
    (format!("{}：{msg}", tr(lang, "error.apiChanged")), false)
}

/// PoW WASM 下载/实例化失败（不是接口错误，单独措辞）
fn describe_pow(lang: Lang, msg: &str) -> String {
    format!("{}：{msg}", tr(lang, "error.pow"))
}

/// 构建本轮要发送的内容
fn build_outgoing(
    config: &AppConfig,
    session: &Session,
    system_text: &str,
    user_input: &str,
) -> String {
    match config.context_mode {
        ContextMode::Reuse => {
            // 网页会话还没跑过任何一轮时，把系统提示词 + 工具说明随本轮下发
            if session.handle.session_id.is_none() && session.handle.parent_message_id.is_none() {
                format!("{system_text}\n\n{user_input}")
            } else {
                user_input.to_string()
            }
        }
        ContextMode::Replay => {
            let mut parts = vec![format!("<｜System｜>{system_text}\n")];
            for (role, content) in &session.messages {
                let tag = if role == "user" { "User" } else { "Assistant" };
                parts.push(format!("<｜{tag}｜>{content}"));
            }
            parts.push(format!("<｜User｜>{user_input}"));
            parts.join("")
        }
    }
}

/// 确保 PoW 求解器已就绪（懒加载 + 进程内缓存）
fn ensure_solver(runtime: &AgentRuntime, tx: &Sender<UiEvent>) -> bool {
    let mut guard = runtime.solver.lock().unwrap();
    if guard.is_some() {
        return true;
    }
    // 首次要下载并实例化官方 sha3 WASM，会停顿一两秒。
    // 这里刻意不往输出区打字：那行提示会打断排版，而状态栏的计时已经能说明在忙。
    match load_pow_solver(
        &runtime.config.wasm_url,
        &runtime.config.user_agent,
        &runtime.config.proxy,
    ) {
        Ok(solver) => {
            *guard = Some(solver);
            true
        }
        Err(e) => {
            let _ = tx.send(UiEvent::Error(describe_pow(runtime.lang, &e.to_string())));
            false
        }
    }
}

/// 工具调用的一行展示（`▌ read  package.json`）
pub fn tool_call_line(call: &ToolCall) -> String {
    format!("▌ {:<6}{}", call.name().as_str(), describe_call(call))
}

/// 毫秒格式化为紧凑时长
pub fn format_ms(ms: u128) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let total = (ms as f64 / 1000.0).round() as u64;
        format!("{}m{:02}s", total / 60, total % 60)
    }
}

/// 执行一轮对话（阻塞，跑在后台线程）
pub fn run_turn(
    runtime: &AgentRuntime,
    session: &mut Session,
    user_input: &str,
    system_text: &str,
    tx: &Sender<UiEvent>,
) {
    let lang = runtime.lang;
    let turn_started = std::time::Instant::now();
    let mut total_usage: u64 = 0;
    let mut saw_usage = false;
    // 累计的「纯生成」耗时，用于 token/s（工具执行的时间不算在内）
    let mut gen_ms: u128 = 0;

    session
        .messages
        .push(("user".to_string(), user_input.to_string()));

    let max_steps = runtime.config.max_tool_steps.max(1);
    let result_prefix = tool_result_prefix(lang);
    let mut outgoing = build_outgoing(&runtime.config, session, system_text, user_input);
    let mut steps = 0usize;
    // 网页会话失效只自动重开一次，否则会死循环
    let mut handle_reset = false;

    loop {
        if *runtime.aborted.lock().unwrap() {
            let _ = tx.send(UiEvent::Notice(tr(lang, "repl.stopped").to_string()));
            break;
        }

        if !ensure_solver(runtime, tx) {
            break;
        }

        let assembled = Arc::new(Mutex::new(Assembled::default()));
        // 思考块必须先于正文上屏。正文是按行实时推给 UI 的，
        // 若等整个流结束再发 ThinkEnd，思考块就会排到正文下面 —— 顺序就反了。
        // 所以「正文一开始」就把思考收尾；sent 用于防止重复发送。
        let think_sent = Arc::new(AtomicBool::new(false));
        let stream_started = std::time::Instant::now();
        let stream_result = {
            let mut guard = runtime.solver.lock().unwrap();
            let Some(solver) = guard.as_mut() else {
                break;
            };
            let tx_cb = tx.clone();
            let aborted = runtime.aborted.clone();
            let state = assembled.clone();
            let sent_cb = think_sent.clone();
            let mut think_started = std::time::Instant::now();
            let mut on_event = move |evt: StreamEvent| -> bool {
                if *aborted.lock().unwrap() {
                    return false;
                }
                let mut acc = state.lock().unwrap();
                match evt {
                    StreamEvent::ThinkStart => {
                        think_started = std::time::Instant::now();
                        sent_cb.store(false, Ordering::Relaxed);
                        let _ = tx_cb.send(UiEvent::ThinkStart);
                    }
                    StreamEvent::ThinkDelta(text) => {
                        acc.think.push_str(&text);
                        acc.think_chars += text.chars().count();
                        let chars = acc.think_chars;
                        drop(acc);
                        let _ = tx_cb.send(UiEvent::ThinkProgress { chars });
                    }
                    StreamEvent::ContentStart => {
                        // 正文要开始了：先把思考块交出去，保证它排在正文上面
                        if !sent_cb.swap(true, Ordering::Relaxed) {
                            let text = std::mem::take(&mut acc.think);
                            let ms = think_started.elapsed().as_millis();
                            drop(acc);
                            let _ = tx_cb.send(UiEvent::ThinkEnd { text, ms });
                        }
                    }
                    StreamEvent::ContentDelta(text) => {
                        acc.assistant.push_str(&text);
                        acc.pending.push_str(&text);
                        // 按整行上报，便于 UI 逐行排版
                        let mut lines: Vec<String> = Vec::new();
                        while let Some(idx) = acc.pending.find('\n') {
                            lines.push(acc.pending[..idx].to_string());
                            acc.pending = acc.pending[idx + 1..].to_string();
                        }
                        acc.emitted_len += lines.len();
                        drop(acc);
                        for line in lines {
                            let _ = tx_cb.send(UiEvent::Line(line));
                        }
                    }
                    StreamEvent::Done {
                        finish_reason,
                        usage,
                    } => {
                        acc.finish = finish_reason;
                        if usage.is_some() {
                            acc.usage = usage;
                        }
                        let pending = std::mem::take(&mut acc.pending);
                        // 只有思考、没有正文的回复（或始终没收到 ContentStart）也在这里收尾
                        let closing = if sent_cb.swap(true, Ordering::Relaxed) {
                            None
                        } else {
                            Some((
                                std::mem::take(&mut acc.think),
                                think_started.elapsed().as_millis(),
                            ))
                        };
                        drop(acc);
                        if !pending.is_empty() {
                            let _ = tx_cb.send(UiEvent::Line(pending));
                        }
                        if let Some((text, ms)) = closing {
                            let _ = tx_cb.send(UiEvent::ThinkEnd { text, ms });
                        }
                    }
                }
                true
            };

            stream_chat_with_retry(
                runtime.client.as_ref(),
                solver,
                &runtime.token,
                &outgoing,
                "default",
                runtime.config.thinking,
                runtime.config.search,
                Some(&mut session.handle),
                &mut on_event,
                3,
            )
        };

        let stream_ms = stream_started.elapsed().as_millis();
        gen_ms += stream_ms;

        let snapshot = {
            let acc = assembled.lock().unwrap();
            (acc.assistant.clone(), acc.usage)
        };
        let (assistant_text, usage) = snapshot;

        // 兜底：流式过程中途出错时既没走到 ContentStart 也没走到 Done，
        // 思考态会一直挂在状态栏上，这里补一次收尾。
        if !think_sent.swap(true, Ordering::Relaxed) {
            let text = {
                let mut acc = assembled.lock().unwrap();
                std::mem::take(&mut acc.think)
            };
            let _ = tx.send(UiEvent::ThinkEnd {
                text,
                ms: stream_ms,
            });
        }
        if let Some(u) = usage {
            total_usage += u;
            saw_usage = true;
        }

        if let Err(e) = stream_result {
            // 服务端常见的一种错误码：HTTP 200、code=0，但
            // biz_code=1 + biz_msg="invalid chat session id" ——
            // 说明绑定的那个网页会话已经没了（换过会话、或在网页端删了）。
            // 这种情况丢掉绑定重开一次，比把错误甩给用户有用得多。
            let text = e.to_string();
            if !handle_reset
                && (text.contains("invalid chat session")
                    || text.contains("invalid_chat_session")
                    || text.contains("chat_session_id"))
            {
                handle_reset = true;
                session.handle.session_id = None;
                session.handle.parent_message_id = None;
                outgoing = build_outgoing(&runtime.config, session, system_text, user_input);
                let _ = tx.send(UiEvent::Notice(
                    "网页会话已失效，已重开会话并重试。".to_string(),
                ));
                continue;
            }
            let (msg, auth) = describe_error(lang, &e);
            let _ = tx.send(UiEvent::Error(msg));
            if auth {
                // 凭证失效：让界面提示并清掉本地凭证
                let _ = tx.send(UiEvent::AuthFailed);
            }
            break;
        }

        session
            .messages
            .push(("assistant".to_string(), assistant_text.clone()));

        let parsed = parse_tool_calls(&assistant_text);

        // 解析出错：回灌错误让模型自我修正
        if parsed.calls.is_empty() && !parsed.errors.is_empty() {
            let error_text = format!(
                "{result_prefix}\n[tool-call parse error]\n{}",
                parsed.errors.join("\n")
            );
            session
                .messages
                .push(("tool".to_string(), error_text.clone()));
            outgoing = error_text;
            steps += 1;
            if steps >= max_steps {
                let _ = tx.send(UiEvent::Notice(
                    format!("已达到最大工具调用轮数（{max_steps}）"),
                ));
                break;
            }
            continue;
        }

        if parsed.calls.is_empty() {
            break;
        }

        // 并行执行本批工具
        let calls = parsed.calls.clone();
        let _ = tx.send(UiEvent::ToolBatchStart(
            calls.iter().map(tool_call_line).collect(),
        ));
        let parallel = calls.len() > 1;
        let cwd = session.cwd.clone();
        let results = execute_parallel(calls.clone(), cwd, lang);

        let mut blocks: Vec<String> = Vec::new();
        let batch_started = std::time::Instant::now();
        for (call, result, ms) in results {
            let _ = tx.send(UiEvent::ToolEnd {
                tool: call.name().as_str().to_string(),
                result: result.clone(),
                ms,
                parallel,
            });
            let body = if result.ok {
                result.output.clone()
            } else {
                format!("[error] {}", result.output)
            };
            session
                .messages
                .push(("tool".to_string(), body.clone()));
            blocks.push(format!("{result_prefix}\n{body}"));
        }
        if parallel {
            let _ = tx.send(UiEvent::Line(format!(
                "· {} 个工具并行  ·  {}",
                calls.len(),
                format_ms(batch_started.elapsed().as_millis())
            )));
        }

        outgoing = blocks.join("\n\n");
        steps += 1;
        if steps >= max_steps {
            let _ = tx.send(UiEvent::Notice(format!(
                "已达到最大工具调用轮数（{max_steps}）"
            )));
            break;
        }
    }

    // 会话名由服务端生成（网页端就是这么做的），本地不用提示词顶替。
    // 首轮它可能还没起好名，所以只要标题还是默认值就每轮再问一次。
    if session.title == "新会话" {
        if let Some(sid) = session.handle.session_id.clone() {
            if let Some(title) = runtime.client.session_title(&runtime.token, &sid) {
                session.title = title;
            }
        }
    }

    let _ = tx.send(UiEvent::TurnDone {
        usage: if saw_usage { Some(total_usage) } else { None },
        ms: turn_started.elapsed().as_millis(),
        gen_ms,
    });
    let _ = tx.send(UiEvent::Finished);
}

/// 并行执行一批工具，返回 (调用, 结果, 耗时毫秒)，顺序与传入一致
fn execute_parallel(
    calls: Vec<ToolCall>,
    cwd: PathBuf,
    lang: Lang,
) -> Vec<(ToolCall, ToolResult, u128)> {
    let mut handles = Vec::new();
    for call in calls {
        let cwd = cwd.clone();
        handles.push(std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                execute_tool(&call, &cwd, lang)
            }))
            .unwrap_or_else(|_| ToolResult {
                ok: false,
                output: tr(lang, "tool.crashed").to_string(),
                summary: describe_call(&call),
            });
            (call, result, started.elapsed().as_millis())
        }));
    }
    handles
        .into_iter()
        .filter_map(|h| h.join().ok())
        .collect()
}

/// 供 UI 拼接系统提示词文本
pub fn build_system_text(system_prompt: &str, lang: Lang) -> String {
    format!("{system_prompt}\n\n{}", build_tool_doc(lang))
}
