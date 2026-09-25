mod agent;
mod auth;
mod clipboard;
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

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use agent::{AgentRuntime, Session, UiEvent};
use config::{AppConfig, ConfigPaths, Lang};
use deepseek::{DeepSeekClient, PowSolver};
use i18n::{on_off, tr};
use ui::App;

const HELP: &str = "\
DSP (deepseek-pi) —— 终端编程助手，仅使用 DeepSeek 网页版

用法
  dsp [--config-dir <path>] [--resume] [--selftest]

选项
  -c, --config-dir <path>   配置目录（默认 ~/.pi/agent，也可用 PI_CONFIG_DIR）
      --resume              接着最近一次会话继续
      --selftest            只做自检：打印机器指纹并尝试解密已保存的凭证
  -h, --help                显示本帮助

快捷键
  Enter 发送          Esc 退出（有选区时先取消选区）
  Ctrl+C 中断生成     选区存在时改为复制
  右键 / Ctrl+C      复制选区
  Ctrl+T / Ctrl+S    深度思考 / 智能搜索
  Ctrl+O             展开或收起最近一个块
  Ctrl+↑ / Ctrl+↓    历史顶部 / 回到底部        滚轮 / PgUp / PgDn 翻页
  左键拖动           选择文本        左键单击块头 折叠

命令
  /help /login /logout /thinking /search /thinking-view /model /lang
  /status /sessions /session /clear /goto /quit

登录
  Rust 版不做浏览器自动化。三种方式任选：
    /login <userToken>     直接带上 token
    /login                 在界面里粘贴 token（回车确认，Esc 取消）
    DSP_TOKEN=<token> dsp  从环境变量注入
  token 取自 chat.deepseek.com 的 LocalStorage（key 为 userToken）。
  加密方案与 TS 版一致，所以 TS 版写下的凭证可以直接复用。";

#[derive(Default)]
struct Args {
    config_dir: Option<String>,
    command: Option<String>,
    resume: bool,
    selftest: bool,
    help: bool,
}

fn parse_args(argv: &[String]) -> Args {
    let mut args = Args::default();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--help" | "-h" => args.help = true,
            "--selftest" => args.selftest = true,
            "--resume" => args.resume = true,
            "--config-dir" | "-c" => {
                if let Some(v) = argv.get(i + 1) {
                    args.config_dir = Some(v.clone());
                    i += 1;
                }
            }
            a if a.starts_with("--config-dir=") => {
                args.config_dir = Some(a["--config-dir=".len()..].to_string());
            }
            a if a.starts_with('/') => {
                args.command = Some(argv[i..].join(" "));
                break;
            }
            _ => {}
        }
        i += 1;
    }
    args
}

fn main() {
    let args = parse_args(&std::env::args().skip(1).collect::<Vec<_>>());
    if args.help {
        println!("{HELP}");
        return;
    }

    let paths = config::resolve_paths(args.config_dir.as_deref());
    if args.selftest {
        std::process::exit(selftest(&paths));
    }

    if let Err(e) = run(paths, args.resume) {
        cleanup_terminal();
        eprintln!("启动失败：{e}");
        std::process::exit(1);
    }
}

/// 不联网的兼容性自检：验证加密方案能否解开已有凭证
fn selftest(paths: &ConfigPaths) -> i32 {
    println!("配置目录 : {}", paths.config_dir.display());
    println!("凭证文件 : {}", paths.auth_file.display());
    println!("机器指纹 : {}", auth::machine_fingerprint());
    match auth::load_auth(paths) {
        Some(data) => {
            println!("凭证解密 : 成功（{} 字符）", data.token.chars().count());
            println!("token 指纹: {}", auth::token_fingerprint(&data.token));
            0
        }
        None => {
            println!("凭证解密 : 失败或不存在");
            if paths.auth_file.exists() {
                println!("文件存在但解不开，通常是机器指纹不同（换机或换用户）。");
                1
            } else {
                2
            }
        }
    }
}

struct Core {
    paths: ConfigPaths,
    config: AppConfig,
    token: Option<String>,
    lang: Lang,
    session: Arc<Mutex<Session>>,
    session_title: String,
    client: Option<Arc<DeepSeekClient>>,
    solver: Arc<Mutex<Option<PowSolver>>>,
    aborted: Arc<Mutex<bool>>,
    /// /system-prompt edit：交给事件循环临时让出终端
    pending_editor: bool,
}

impl Core {
    fn t<'a>(&self, key: &'a str) -> &'a str {
        tr(self.lang, key)
    }

    fn abort(&self) {
        *self.aborted.lock().unwrap() = true;
    }

    fn client(&mut self) -> Result<Arc<DeepSeekClient>, String> {
        if let Some(c) = &self.client {
            return Ok(c.clone());
        }
        let client = DeepSeekClient::new(&self.config).map_err(|e| e.to_string())?;
        self.client = Some(Arc::new(client));
        Ok(self.client.clone().unwrap())
    }

    fn system_text(&self) -> String {
        let prompt = prompt::load_system_prompt(&self.paths, self.lang);
        agent::build_system_text(&prompt, self.lang)
    }

    fn save_session(&self) {
        let session = self.session.lock().unwrap().clone();
        let _ = session.save(&self.paths.sessions_dir);
    }

    fn persist(&mut self) {
        let _ = config::save_config(&self.paths, &self.config);
    }

    fn status_text(&self) -> String {
        format!(
            "◆ {} · {} · {} {} · {} {} · {}",
            self.session_title,
            self.config.model,
            self.t("status.thinking"),
            on_off(self.lang, self.config.thinking),
            self.t("status.search"),
            on_off(self.lang, self.config.search),
            self.lang.code(),
        )
    }
}

fn run(paths: ConfigPaths, resume: bool) -> anyhow::Result<()> {
    let mut config = config::load_config(&paths);
    let _ = prompt::ensure_system_prompt_file(&paths, config.language);
    let _ = config::ensure_models_file(&paths);

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let session = if resume {
        Session::latest(&paths.sessions_dir).unwrap_or_else(|| Session::new(cwd.clone(), "新会话"))
    } else {
        Session::new(cwd, "新会话")
    };
    let token = auth::load_auth(&paths).map(|d| d.token);

    // 配置里的 UA 为空时，用凭证里记录的那个（与登录时的浏览器一致）
    if config.user_agent.is_empty() {
        if let Some(auth) = auth::load_auth(&paths) {
            if !auth.user_agent.is_empty() {
                config.user_agent = auth.user_agent;
            }
        }
    }

    let mut core = Core {
        lang: config.language,
        session_title: session.title.clone(),
        session: Arc::new(Mutex::new(session)),
        token,
        config,
        client: None,
        solver: Arc::new(Mutex::new(None)),
        aborted: Arc::new(Mutex::new(false)),
        pending_editor: false,
        paths,
    };

    let mut app = App::default();
    app.banner(core.lang, core.token.as_ref().map(|t| t.chars().count()));
    app.set_hint(core.t("ui.hint").to_string());
    app.set_status(core.status_text());

    let mut terminal = setup_terminal()?;
    let outcome = event_loop(&mut terminal, &mut core, &mut app);
    cleanup_terminal();
    core.save_session();
    outcome
}

fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    // 崩了也要把终端还原，否则会留下一个乱掉的画面
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        cleanup_terminal();
        original(info);
    }));
    enter_terminal()
}

fn enter_terminal() -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn cleanup_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        std::io::stdout(),
        LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    );
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    core: &mut Core,
    app: &mut App,
) -> anyhow::Result<()> {
    let mut rx: Option<Receiver<UiEvent>> = None;

    loop {
        terminal.draw(|frame| app.draw(frame, core.lang))?;

        if let Some(receiver) = &rx {
            let mut done = false;
            while let Ok(ev) = receiver.try_recv() {
                match &ev {
                    UiEvent::Finished => done = true,
                    UiEvent::AuthFailed => {
                        // 界面已经提示过了，这里只负责清掉本地凭证
                        core.token = None;
                        let _ = auth::clear_auth(&core.paths);
                    }
                    _ => {}
                }
                app.apply(ev);
            }
            if done {
                rx = None;
                core.save_session();
                // 标题可能是本轮首次由用户输入确定的，同步回状态栏
                core.session_title = core.session.lock().unwrap().title.clone();
                app.set_status(core.status_text());
            }
        }

        if app.quit {
            return Ok(());
        }

        // 复制请求交给主线程做（会 fork 子进程，不适合放在渲染路径里）
        if let Some(text) = app.take_copied() {
            let len = text.chars().count();
            clipboard::copy(&text);
            app.notice_copied(len);
        }

        if let Some(token) = app.take_login_input() {
            do_login(core, app, &token);
        }

        if let Some(input) = app.take_pending() {
            submit(core, app, input, &mut rx);
        }

        if core.pending_editor {
            core.pending_editor = false;
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".to_string());
            let file = core.paths.system_prompt_file.clone();
            cleanup_terminal();
            let status = std::process::Command::new(editor).arg(&file).status();
            *terminal = enter_terminal()?;
            match status {
                Ok(s) if s.success() => app.line_styled("系统提示词已更新，下一轮生效。", ui::ok()),
                _ => app.line_styled("编辑器未正常退出，提示词未改动。", ui::warn()),
            }
        }

        if !event::poll(Duration::from_millis(40))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c'));
                if ctrl_c && !app.has_selection() {
                    core.abort();
                    continue;
                }
                if app.on_key(key) {
                    app.quit = true;
                }
            }
            Event::Mouse(ev) => app.on_mouse(ev),
            _ => {}
        }
    }
}

/// 一条输入：斜杠命令 / !shell / 普通对话
fn submit(core: &mut Core, app: &mut App, input: String, rx: &mut Option<Receiver<UiEvent>>) {
    let trimmed = input.trim();
    app.line_styled(format!("❯ {trimmed}"), ui::user_style());

    if trimmed.starts_with('/') {
        command(trimmed, core, app);
    } else if let Some(cmd) = trimmed.strip_prefix('!') {
        shell(cmd.trim(), core, app);
    } else {
        app.anchor(label_of(trimmed));
        start_turn(core, app, trimmed.to_string(), rx);
    }
}

fn label_of(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 64 {
        format!("{}…", flat.chars().take(63).collect::<String>())
    } else {
        flat
    }
}

fn start_turn(core: &mut Core, app: &mut App, input: String, rx: &mut Option<Receiver<UiEvent>>) {
    if core.token.is_none() {
        app.line_styled(format!("! {}", core.t("app.notLoggedIn")), ui::warn());
        return;
    }
    let client = match core.client() {
        Ok(c) => c,
        Err(e) => {
            app.line_styled(format!("! {e}"), ui::err());
            return;
        }
    };
    *core.aborted.lock().unwrap() = false;
    app.set_busy(true);

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
    let session = core.session.clone();

    std::thread::spawn(move || {
        let mut guard = session.lock().unwrap();
        agent::run_turn(&runtime, &mut guard, &input, &system_text, &tx);
    });
}

/// `!命令`：直接跑 shell，结果只打印，不进对话上下文
fn shell(command: &str, core: &Core, app: &mut App) {
    if command.is_empty() {
        app.line_styled(core.t("shell.usage"), ui::dim());
        return;
    }
    app.line_styled(format!("▌ {command}"), ui::warn());
    let cwd = core.session.lock().unwrap().cwd.clone();
    let out = tools::run_shell(command, &cwd);
    for line in out.output.lines() {
        app.line(format!("  {line}"));
    }
    let style = if out.ok { ui::ok() } else { ui::err() };
    app.line_styled(
        format!(
            "└ {} {}  {}",
            core.t("shell.exit"),
            out.code,
            agent::format_ms(out.duration_ms)
        ),
        style,
    );
}

fn command(input: &str, core: &mut Core, app: &mut App) {
    let mut it = input.splitn(2, char::is_whitespace);
    let cmd = it.next().unwrap_or("").to_ascii_lowercase();
    let arg = it.next().unwrap_or("").trim().to_string();
    let lang = core.lang;

    match cmd.as_str() {
        "/help" => {
            for line in HELP.lines() {
                app.line(line);
            }
        }
        "/quit" | "/exit" => app.quit = true,
        "/clear" => {
            let cwd = core.session.lock().unwrap().cwd.clone();
            *core.session.lock().unwrap() = Session::new(cwd, "新会话");
            core.session_title = "新会话".to_string();
            app.set_status(core.status_text());
            app.line_styled(core.t("cmd.clear"), ui::dim());
        }
        "/thinking" => {
            core.config.thinking = toggle(&arg).unwrap_or(!core.config.thinking);
            after_toggle(core, app, "toggle.thinking", core.config.thinking);
        }
        "/search" => {
            core.config.search = toggle(&arg).unwrap_or(!core.config.search);
            after_toggle(core, app, "toggle.search", core.config.search);
        }
        "/thinking-view" => {
            core.config.show_thinking = toggle(&arg).unwrap_or(!core.config.show_thinking);
            core.persist();
            let state = on_off(lang, core.config.show_thinking);
            app.line_styled(format!("思考显示 {state}"), ui::dim());
            if core.config.show_thinking {
                let text = app.last_thinking.clone();
                app.push_thinking(&text);
            }
        }
        "/model" => {
            if arg.is_empty() {
                let all: Vec<&str> = config::MODELS.iter().map(|(id, _, _)| *id).collect();
                app.line_styled(
                    format!("当前 {} · 可选 {}", core.config.model, all.join(", ")),
                    ui::dim(),
                );
            } else if config::MODELS.iter().any(|(id, _, _)| *id == arg) {
                core.config.model = arg.clone();
                core.persist();
                app.line_styled(format!("已切换到 {arg}"), ui::dim());
            } else {
                app.line_styled(format!("未知模型 {arg}"), ui::warn());
            }
        }
        "/lang" => {
            let next = match arg.as_str() {
                "" => core.lang.other(),
                other => Lang::parse(other).unwrap_or_else(|| core.lang.other()),
            };
            core.config.language = next;
            core.lang = next;
            core.persist();
            app.set_hint(tr(next, "ui.hint").to_string());
            app.set_status(core.status_text());
            app.line_styled(tr(next, "cmd.langSet"), ui::dim());
        }
        "/status" => {
            app.line_styled(format!("{}:", core.t("cmd.status")), ui::user_style());
            app.line(format!("  配置目录   {}", core.paths.config_dir.display()));
            app.line(format!("  系统提示词 {}", core.paths.system_prompt_file.display()));
            app.line(format!("  模型       {}", core.config.model));
            app.line(format!(
                "  深度思考 {}   智能搜索 {}",
                on_off(lang, core.config.thinking),
                on_off(lang, core.config.search)
            ));
            match &core.token {
                Some(t) => app.line(format!(
                    "  凭证       {} 字符 · 指纹 {}",
                    t.chars().count(),
                    auth::token_fingerprint(t)
                )),
                None => app.line_styled(format!("  {}", core.t("ui.loginMissing")), ui::warn()),
            }
        }
        "/sessions" => {
            let all = Session::list(&core.paths.sessions_dir);
            if all.is_empty() {
                app.line_styled("暂无历史会话。", ui::dim());
            } else {
                app.line_styled(format!("历史会话（{}）", all.len()), ui::user_style());
                for (i, s) in all.iter().enumerate() {
                    app.line(format!(
                        "  #{:<3}{:<24}{} 条消息",
                        i + 1,
                        s.title,
                        s.messages.len()
                    ));
                }
                app.line_styled("用 /session <序号> 切换。", ui::dim());
            }
        }
        "/session" => {
            let all = Session::list(&core.paths.sessions_dir);
            let index: usize = match arg.trim_start_matches('#').parse::<usize>() {
                Ok(n) if n >= 1 && n <= all.len() => n - 1,
                _ => {
                    app.line_styled("用法：/session <序号>（先 /sessions 查看）", ui::dim());
                    return;
                }
            };
            let picked = all[index].clone();
            core.session_title = picked.title.clone();
            *core.session.lock().unwrap() = picked;
            app.set_status(core.status_text());
            app.line_styled(format!("已切换到 {}", core.session_title), ui::ok());
        }
        "/goto" => match arg.trim_start_matches('#').parse::<usize>() {
            Ok(n) if n >= 1 => app.goto_index(n - 1),
            _ => app.open_goto(),
        },
        "/system-prompt" => match arg.as_str() {
            "reset" => {
                let lang = core.lang;
                match prompt::reset_system_prompt(&core.paths, lang) {
                    Ok(()) => app.line_styled("已恢复默认系统提示词。", ui::ok()),
                    Err(e) => app.line_styled(format!("重置失败：{e}"), ui::err()),
                }
            }
            "edit" => core.pending_editor = true,
            _ => {
                app.line_styled(
                    format!("{}（edit 用编辑器打开 / reset 恢复默认）", core.paths.system_prompt_file.display()),
                    ui::dim(),
                );
                for line in prompt::load_system_prompt(&core.paths, lang).lines() {
                    app.line(format!("  {line}"));
                }
            }
        },
        // 三条登录路径，按优先级：/login <token> > 环境变量 > 交互粘贴
        "/login" => {
            if !arg.is_empty() {
                do_login(core, app, &arg);
            } else if let Ok(token) = std::env::var("DSP_TOKEN") {
                if token.trim().is_empty() {
                    app.start_login();
                } else {
                    let token = token.trim().to_string();
                    do_login(core, app, &token);
                }
            } else {
                app.start_login();
            }
        }
        "/logout" => {
            let removed = auth::clear_auth(&core.paths);
            core.token = None;
            app.set_status(core.status_text());
            app.line_styled(
                if removed { "已清除本地凭证。" } else { "本地没有凭证。" },
                ui::dim(),
            );
        }
        other => app.line_styled(format!("未知命令 {other}（/help 查看全部）"), ui::warn()),
    }
}

/// 保存一份 token。Rust 版不做浏览器自动化，token 由用户提供
/// （粘贴 / 环境变量 / 复用 TS 版写下的凭证，两边加密格式一致）。
fn do_login(core: &mut Core, app: &mut App, token: &str) {
    if token.trim().is_empty() {
        app.line_styled("token 为空，未保存。", ui::warn());
        return;
    }
    let ua = core.config.user_agent.clone();
    match auth::save_token_from_input(&core.paths, token, &ua) {
        Ok(saved) => {
            core.token = Some(saved.token);
            app.set_status(core.status_text());
            app.line_styled(core.t("login.success"), ui::ok());
        }
        Err(e) => app.line_styled(format!("{}：{e}", core.t("login.failed")), ui::err()),
    }
}

fn after_toggle(core: &mut Core, app: &mut App, key: &str, value: bool) {
    core.persist();
    app.set_status(core.status_text());
    app.line_styled(
        format!("{} {}", core.t(key), on_off(core.lang, value)),
        ui::dim(),
    );
}

fn toggle(arg: &str) -> Option<bool> {
    match arg.trim().to_ascii_lowercase().as_str() {
        "on" | "1" | "true" | "yes" | "开" | "开启" => Some(true),
        "off" | "0" | "false" | "no" | "关" | "关闭" => Some(false),
        _ => None,
    }
}


