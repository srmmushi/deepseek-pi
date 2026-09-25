//! DSP (deepseek-pi) —— Rust + Ratatui 终端编程助手
//!
//! 入口与事件循环：
//!   - 解析 --config-dir / PI_CONFIG_DIR / ~/.pi/agent
//!   - 启动 Ratatui，主线程只负责渲染与按键
//!   - 一轮对话跑在后台线程，通过 mpsc 把事件推回主线程

mod agent;
mod auth;
mod config;
mod deepseek;
mod i18n;
mod prompt;
mod stream;
mod tools;
mod ui;

use std::io::Stdout;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::style::{Color, Style};
use ratatui::Terminal;

use agent::{AgentRuntime, Session, UiEvent};
use config::{AppConfig, ConfigPaths, Lang};
use deepseek::DeepSeekClient;
use i18n::{on_off, tr};
use ui::App;

/// 命令行参数
#[derive(Debug, Default)]
struct CliArgs {
    config_dir: Option<String>,
    help: bool,
    command: Option<String>,
    selftest: bool,
}

fn parse_args(argv: &[String]) -> CliArgs {
    let mut args = CliArgs::default();
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        match arg.as_str() {
            "--help" | "-h" => args.help = true,
            "--selftest" => args.selftest = true,
            "--config-dir" | "-c" => {
                if let Some(next) = argv.get(i + 1) {
                    args.config_dir = Some(next.clone());
                    i += 1;
                }
            }
            other if other.starts_with("--config-dir=") => {
                args.config_dir = Some(other["--config-dir=".len()..].to_string());
            }
            other if other.starts_with('/') => {
                args.command = Some(argv[i..].join(" "));
                break;
            }
            _ => {}
        }
        i += 1;
    }
    args
}

fn help_text() -> String {
    "DSP (deepseek-pi) —— Rust + Ratatui 终端编程助手，仅使用 DeepSeek 网页版\n\n\
用法：\n  \
dsp [--config-dir <path>] [--selftest]\n  \
dsp --help\n\n\
选项：\n  \
-c, --config-dir <path>   指定配置目录（默认 ~/.pi/agent，也可用环境变量 PI_CONFIG_DIR）\n  \
    --selftest            只做自检：打印机器指纹并尝试解密已保存的凭证\n  \
-h, --help                显示本帮助\n\n\
快捷键：\n  \
Enter 发送 · Esc 退出 · Ctrl+C 中断生成\n  \
Ctrl+T 深度思考 · Ctrl+S 智能搜索 · Ctrl+O 展开/折叠最近一个块\n  \
Ctrl+↑/↓ 或滚轮 浏览历史 · 左键点击块头 折叠/展开\n\n\
命令：/help /login /logout /thinking /search /thinking-view /model /lang /status /clear /quit"
        .to_string()
}

/// 自检：验证加密方案与已有凭证是否兼容（不联网）
fn selftest(paths: &ConfigPaths) -> i32 {
    println!("配置目录 : {}", paths.config_dir.display());
    println!("凭证文件 : {}", paths.auth_file.display());
    println!("机器指纹 : {}", auth::machine_fingerprint());
    match auth::load_auth(paths) {
        Some(data) => {
            println!("凭证解密 : 成功");
            println!("token 长度: {}", data.token.chars().count());
            println!("token 指纹: {}", auth::token_fingerprint(&data.token));
            println!("UA        : {}", data.user_agent);
            0
        }
        None => {
            println!("凭证解密 : 失败或不存在");
            if paths.auth_file.exists() {
                println!("提示：文件存在但无法解密——多半是机器指纹不一致（换机/换用户）。");
                1
            } else {
                2
            }
        }
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(&argv);

    if args.help {
        println!("{}", help_text());
        return;
    }

    let paths = config::resolve_paths(args.config_dir.as_deref());
    if args.selftest {
        std::process::exit(selftest(&paths));
    }

    if let Err(e) = run(paths) {
        eprintln!("启动失败：{e}");
        std::process::exit(1);
    }
}

/// 运行时的可变状态（config 需要能整体替换，如 /lang、/thinking）
struct Core {
    paths: ConfigPaths,
    config: AppConfig,
    token: Option<String>,
    session: Session,
    client: Option<Arc<DeepSeekClient>>,
    solver: Arc<Mutex<Option<deepseek::PowSolver>>>,
    aborted: Arc<Mutex<bool>>,
    lang: Lang,
}

impl Core {
    fn t<'a>(&self, key: &'a str) -> &'a str {
        tr(self.lang, key)
    }

    /// 保证客户端已就绪（配置变化后需要重建）
    fn ensure_client(&mut self) -> Result<Arc<DeepSeekClient>, String> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        match DeepSeekClient::new(&self.config) {
            Ok(client) => {
                let arc = Arc::new(client);
                self.client = Some(arc.clone());
                Ok(arc)
            }
            Err(e) => Err(format!("创建 HTTP 客户端失败：{e}")),
        }
    }

    /// 系统提示词 + 工具说明
    fn system_text(&self) -> String {
        let prompt = prompt::load_system_prompt(&self.paths, self.lang);
        agent::build_system_text(&prompt, self.lang)
    }

    /// 状态栏文本
    fn status_text(&self) -> String {
        let model = if self.config.model.is_empty() {
            "deepseek-chat".to_string()
        } else {
            self.config.model.clone()
        };
        let login = if self.token.is_some() {
            on_off(self.lang, true)
        } else {
            self.t("ui.loginMissing")
        };
        format!(
            "◆ {} · {model} · {} {} · {} {} · {} · {}",
            self.session.title,
            self.t("status.thinking"),
            on_off(self.lang, self.config.thinking),
            self.t("status.search"),
            on_off(self.lang, self.config.search),
            self.lang.code(),
            login
        )
    }

    /// 重新加载配置并同步界面语言
    fn reload_config(&mut self) {
        self.config = config::load_config(&self.paths);
        self.lang = self.config.language;
    }

    fn persist(&mut self) {
        let _ = config::save_config(&self.paths, &self.config);
    }
}

fn run(paths: ConfigPaths) -> anyhow::Result<()> {
    let mut core = Core {
        token: auth::load_auth(&paths).map(|d| d.token),
        config: config::load_config(&paths),
        lang: Lang::Zh,
        session: Session::new(
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            "新会话",
        ),
        client: None,
        solver: Arc::new(Mutex::new(None)),
        aborted: Arc::new(Mutex::new(false)),
        paths: paths.clone(),
    };
    core.lang = core.config.language;
    let _ = config::ensure_models_file(&paths);
    prompt::ensure_system_prompt_file(&paths, core.lang);

    let mut app = App::default();
    app.push_line("");
    app.push_styled("  DSP  (deepseek-pi)", ui::bold());
    let login_line = match &core.token {
        Some(token) => format!("  ✓ {} · token {} 字符", core.t("ui.loginOk"), token.chars().count()),
        None => format!("  ✗ {}", core.t("ui.loginMissing")),
    };
    app.push_styled(login_line, Style::default().fg(Color::DarkGray));
    app.push_line("");
    app.hint = core.t("ui.hint").to_string();
    app.status = core.status_text();

    // 终端初始化
    enable_raw_mode()?;
    let mut stdout: Stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, &mut core, &mut app);

    // 恢复终端（无论正常退出还是出错）
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    core: &mut Core,
    app: &mut App,
) -> anyhow::Result<()> {
    // 后台线程事件通道；每轮对话重建一个 worker
    let mut rx: Option<Receiver<UiEvent>> = None;
    let mut last_transcript_height: usize = 1;

    loop {
        terminal.draw(|frame| {
            last_transcript_height = frame.area().height.saturating_sub(2) as usize;
            app.draw(frame, core.lang == Lang::Zh);
        })?;

        // 消费后台事件
        if let Some(receiver) = &rx {
            let mut finished = false;
            while let Ok(event) = receiver.try_recv() {
                if matches!(event, UiEvent::Finished) {
                    finished = true;
                }
                app.apply(event);
            }
            if finished {
                rx = None;
            }
        }

        if app.should_quit {
            break;
        }

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if handle_key(key, core, app, &mut rx, &mut last_transcript_height)? {
                        break;
                    }
                }
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => app.scroll(3),
                    MouseEventKind::ScrollDown => app.scroll(-3),
                    MouseEventKind::Down(MouseButton::Left) => {
                        if mouse.row > 0 {
                            app.toggle_at_row(
                                mouse.row.saturating_sub(1) as usize,
                                last_transcript_height,
                            );
                        }
                    }
                    _ => {}
                },
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

/// 处理按键；返回 true 表示请求退出
fn handle_key(
    key: KeyEvent,
    core: &mut Core,
    app: &mut App,
    rx: &mut Option<Receiver<UiEvent>>,
    _transcript_height: &mut usize,
) -> anyhow::Result<bool> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if ctrl {
        match key.code {
            KeyCode::Char('c') => {
                // 有空选中就先复制（本版本尚未实现选择），否则中断生成
                *core.aborted.lock().unwrap() = true;
                app.busy = false;
                return Ok(false);
            }
            KeyCode::Char('t') => {
                core.config.thinking = !core.config.thinking;
                core.persist();
                app.status = core.status_text();
                app.push_styled(
                    format!(
                        "{} {}",
                        core.t("toggle.thinking"),
                        on_off(core.lang, core.config.thinking)
                    ),
                    ui::dim(),
                );
                return Ok(false);
            }
            KeyCode::Char('s') => {
                core.config.search = !core.config.search;
                core.persist();
                app.status = core.status_text();
                app.push_styled(
                    format!(
                        "{} {}",
                        core.t("toggle.search"),
                        on_off(core.lang, core.config.search)
                    ),
                    ui::dim(),
                );
                return Ok(false);
            }
            KeyCode::Char('o') => {
                app.toggle_last();
                return Ok(false);
            }
            KeyCode::Down => {
                app.offset = 0;
                return Ok(false);
            }
            KeyCode::Up => {
                app.scroll(1 << 20);
                return Ok(false);
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Esc => return Ok(true),
        KeyCode::Enter => {
            let input = app.take_input();
            let trimmed = input.trim().to_string();
            if trimmed.is_empty() {
                return Ok(false);
            }
            app.push_styled(format!("❯ {trimmed}"), ui::bold());
            app.anchor(label_of(&trimmed));
            if trimmed.starts_with('/') {
                handle_command(&trimmed, core, app);
            } else if trimmed.starts_with('!') {
                run_shell_command(&trimmed[1..].trim().to_string(), core, app);
            } else {
                start_turn(core, app, trimmed, rx);
            }
        }
        KeyCode::Backspace => app.backspace(),
        KeyCode::Delete => {
            if app.cursor < app.input.chars().count() {
                app.cursor += 1;
                app.backspace();
            }
        }
        KeyCode::Left => {
            app.cursor = app.cursor.saturating_sub(1);
        }
        KeyCode::Right => {
            let max = app.input.chars().count();
            if app.cursor < max {
                app.cursor += 1;
            }
        }
        KeyCode::Home => app.cursor = 0,
        KeyCode::End => app.cursor = app.input.chars().count(),
        KeyCode::Up => app.scroll(1),
        KeyCode::Down => app.scroll(-1),
        KeyCode::PageUp => app.scroll(10),
        KeyCode::PageDown => app.scroll(-10),
        KeyCode::Char(ch) => app.insert_char(ch),
        _ => {}
    }
    Ok(false)
}

/// 生成提示词锚点标签（截断）
fn label_of(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(60).collect()
}

/// 启动一轮对话（后台线程）
fn start_turn(core: &mut Core, app: &mut App, input: String, rx: &mut Option<Receiver<UiEvent>>) {
    if core.token.is_none() {
        app.push_styled(
            format!("! {}", core.t("app.notLoggedIn")),
            Style::default().fg(Color::Yellow),
        );
        return;
    }
    let client = match core.ensure_client() {
        Ok(c) => c,
        Err(e) => {
            app.push_styled(format!("! {e}"), Style::default().fg(Color::Red));
            return;
        }
    };
    *core.aborted.lock().unwrap() = false;
    app.busy = true;
    app.offset = 0;

    let (tx, receiver): (Sender<UiEvent>, Receiver<UiEvent>) = mpsc::channel();
    *rx = Some(receiver);

    let runtime = AgentRuntime {
        client,
        solver: core.solver.clone(),
        token: core.token.clone().unwrap_or_default(),
        config: core.config.clone(),
        lang: core.lang,
        aborted: core.aborted.clone(),
    };
    let system_text = core.system_text();
    let mut session = core.session.clone();
    let session_out = Arc::new(Mutex::new(core.session.clone()));

    std::thread::spawn(move || {
        agent::run_turn(&runtime, &mut session, &input, &system_text, &tx);
        *session_out.lock().unwrap() = session;
    });
}

/// `!命令`：直接执行 shell，结果只打印、不进对话上下文
fn run_shell_command(command: &str, core: &Core, app: &mut App) {
    if command.is_empty() {
        app.push_styled(core.t("shell.usage"), ui::dim());
        return;
    }
    app.push_styled(
        format!("▌ {command}"),
        Style::default().fg(Color::Yellow),
    );
    let cwd = core.session.cwd.clone();
    let result = tools::run_shell(command, &cwd);
    for line in result.output.lines() {
        app.push_line(format!("  {line}"));
    }
    let exit_style = if result.ok {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::Red)
    };
    app.push_styled(
        format!(
            "└ {} {}  {}",
            core.t("shell.exit"),
            result.code,
            agent::format_ms(result.duration_ms)
        ),
        exit_style,
    );
}

/// 斜杠命令
fn handle_command(input: &str, core: &mut Core, app: &mut App) {
    let mut parts = input.trim().splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("").to_ascii_lowercase();
    let args = parts.next().unwrap_or("").trim().to_string();
    let lang = core.lang;

    match cmd.as_str() {
        "/help" => {
            for line in help_text().lines() {
                app.push_line(line);
            }
        }
        "/quit" | "/exit" => app.should_quit = true,
        "/clear" => {
            core.session = Session::new(core.session.cwd.clone(), "新会话");
            app.push_styled(tr(lang, "cmd.clear"), ui::dim());
        }
        "/thinking" => {
            core.config.thinking = parse_toggle(&args).unwrap_or(!core.config.thinking);
            core.persist();
            app.status = core.status_text();
            app.push_styled(
                format!(
                    "{} {}",
                    tr(lang, "toggle.thinking"),
                    on_off(lang, core.config.thinking)
                ),
                ui::dim(),
            );
        }
        "/search" => {
            core.config.search = parse_toggle(&args).unwrap_or(!core.config.search);
            core.persist();
            app.status = core.status_text();
            app.push_styled(
                format!(
                    "{} {}",
                    tr(lang, "toggle.search"),
                    on_off(lang, core.config.search)
                ),
                ui::dim(),
            );
        }
        "/thinking-view" => {
            core.config.show_thinking = parse_toggle(&args).unwrap_or(!core.config.show_thinking);
            core.persist();
            let state = on_off(lang, core.config.show_thinking);
            app.push_styled(format!("{} {state}", tr(lang, "toggle.thinkingView")), ui::dim());
            let text = app.last_thinking.clone();
            if core.config.show_thinking && !text.is_empty() {
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    app.push_styled(format!("    {line}"), ui::dim());
                }
            }
        }
        "/model" => {
            if args.is_empty() {
                let list: Vec<&str> = config::MODELS.iter().map(|(id, _, _)| *id).collect();
                app.push_styled(
                    format!("{}: {}（当前 {}）", tr(lang, "toggle.model"), list.join(", "), core.config.model),
                    ui::dim(),
                );
            } else if config::MODELS.iter().any(|(id, _, _)| *id == args) {
                core.config.model = args.clone();
                core.persist();
                app.push_styled(format!("{} {args}", tr(lang, "toggle.model")), ui::dim());
            } else {
                app.push_styled(
                    format!("! {}: {args}", tr(lang, "cmd.unknown")),
                    Style::default().fg(Color::Yellow),
                );
            }
        }
        "/lang" => {
            let next = match args.as_str() {
                "" => core.lang.other(),
                other => Lang::parse(other).unwrap_or_else(|| core.lang.other()),
            };
            core.config.language = next;
            core.lang = next;
            core.persist();
            app.hint = tr(next, "ui.hint").to_string();
            app.status = core.status_text();
            app.push_styled(tr(next, "cmd.langSet"), ui::dim());
        }
        "/status" => {
            app.push_styled(format!("{}:", tr(lang, "cmd.status")), ui::bold());
            app.push_line(format!("  {}: {}", tr(lang, "ui.dir"), core.paths.config_dir.display()));
            app.push_line(format!("  model: {}", core.config.model));
            app.push_line(format!(
                "  {}: {}   {}: {}",
                tr(lang, "status.thinking"),
                on_off(lang, core.config.thinking),
                tr(lang, "status.search"),
                on_off(lang, core.config.search)
            ));
            app.push_line(format!(
                "  {}: {}",
                tr(lang, "ui.dir"),
                core.paths.system_prompt_file.display()
            ));
            match &core.token {
                Some(token) => app.push_line(format!(
                    "  {}: {} chars · {}",
                    tr(lang, "ui.loginOk"),
                    token.chars().count(),
                    auth::token_fingerprint(token)
                )),
                None => app.push_styled(
                    format!("  {}", tr(lang, "ui.loginMissing")),
                    Style::default().fg(Color::Yellow),
                ),
            }
        }
        "/login" => {
            // Rust 版暂不做浏览器自动化：直接给出获取方式并提示从环境变量注入
            app.push_styled(
                "Rust 版登录：请设置环境变量 DSP_TOKEN 后重启，或在 TS 版执行 /login 生成凭证（两者加密格式一致，可直接复用）。",
                Style::default().fg(Color::Yellow),
            );
            if let Ok(token) = std::env::var("DSP_TOKEN") {
                if !token.trim().is_empty() {
                    let data = auth::save_token_from_input(
                        &core.paths,
                        &token,
                        &core.config.user_agent,
                    );
                    match data {
                        Ok(saved) => {
                            core.token = Some(saved.token);
                            app.status = core.status_text();
                            app.push_styled(
                                tr(lang, "login.success"),
                                Style::default().fg(Color::Green),
                            );
                        }
                        Err(e) => app.push_styled(
                            format!("! {}: {e}", tr(lang, "login.failed")),
                            Style::default().fg(Color::Red),
                        ),
                    }
                }
            }
        }
        "/logout" => {
            let removed = auth::clear_auth(&core.paths);
            core.token = None;
            app.status = core.status_text();
            app.push_styled(
                if removed {
                    tr(lang, "logout.done")
                } else {
                    tr(lang, "logout.none")
                },
                ui::dim(),
            );
        }
        "/goto" => {
            let anchors = app.anchors.clone();
            if anchors.is_empty() {
                app.push_styled(tr(lang, "goto.empty"), ui::dim());
            } else {
                app.push_styled(format!("{}:", tr(lang, "goto.title")), ui::bold());
                for (i, (label, _)) in anchors.iter().enumerate() {
                    app.push_line(format!("  #{:<3}{label}", i + 1));
                }
                app.push_styled(tr(lang, "goto.hint"), ui::dim());
                if let Ok(index) = args.trim_start_matches('#').parse::<usize>() {
                    if index >= 1 && index <= anchors.len() {
                        let target_row = anchors[index - 1].1;
                        let total = app.rows.len();
                        app.offset = total.saturating_sub(target_row);
                    }
                }
            }
        }
        other => {
            app.push_styled(
                format!("! {}: {other}", tr(lang, "cmd.unknown")),
                Style::default().fg(Color::Yellow),
            );
        }
    }
}

/// 解析 on / off / 开 / 关
fn parse_toggle(arg: &str) -> Option<bool> {
    match arg.trim().to_ascii_lowercase().as_str() {
        "on" | "1" | "true" | "yes" | "开" | "开启" => Some(true),
        "off" | "0" | "false" | "no" | "关" | "关闭" => Some(false),
        _ => None,
    }
}
