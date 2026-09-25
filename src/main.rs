mod agent;
mod auth;
mod browser;
mod clipboard;
mod config;
mod deepseek;
mod i18n;
mod prompt;
mod stream;
mod sysinfo;
mod tools;
mod ui;

use std::io::Stdout;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
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
  dsp [--config-dir <path>] [--resume] [--selftest] [--info]

选项
  -c, --config-dir <path>   配置目录（默认 ~/.pi/agent，也可用 PI_CONFIG_DIR）
      --resume              接着最近一次会话继续
      --selftest            只做自检：打印机器指纹并尝试解密已保存的凭证
      --info                只打印环境信息（同 /info）后退出，不进界面
      --grab                只扫描浏览器存储并报告诊断；找到凭证就顺手保存
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
  /help /login /logout /new /session /clear /goto /thinking /search
  /model /lang /status /info /open /system-prompt /quit

会话
  /new               新建会话（清空上下文）
  /session           列出所有历史会话
  /session <序号>    进入该会话并载入上下文

环境
  /info              系统、架构、构建号（当前那次 git 提交）、主机名；WSL 才显示虚拟机
  /open [url]        用自动识别的浏览器打开网页（默认 chat.deepseek.com）
  Ctrl+T / Ctrl+S    切换深度思考 / 智能搜索（状态栏显示当前值，不打断输出）

登录
  /login                先自动读浏览器里已登录的凭证（Edge 优先）；
                        没有就用 Edge 打开登录页，登录成功后自动读回凭证
  /login token          手动粘贴 userToken
  /login passwd         手机号 / 邮箱 + 密码
  /login wechatqr       微信扫码，二维码直接画在终端里
  DSP_TOKEN=<token> dsp 环境变量，/login 时优先采用
  userToken 存在浏览器 localStorage 里（key 为 userToken），
  上面第一条会自动去 Edge/Chrome 的 LevelDB 里把它抠出来。
  加密格式未变，磁盘上已有的凭证可以直接复用。";

/// 登录页地址（DeepSeek 网页端登录页）
const SIGN_IN_URL: &str = "https://chat.deepseek.com/sign_in";

/// 交互式登录的阶段。`App` 只会「要一次输入」，多步流程靠这个阶段机串起来。
const STAGE_NONE: u8 = 0;
/// 等粘贴 userToken
const STAGE_TOKEN: u8 = 1;
/// 等手机号 / 邮箱（密码登录第一步）
const STAGE_ACCOUNT: u8 = 2;
/// 等密码（密码登录第二步）
const STAGE_PASSWORD: u8 = 3;

const LOGIN_HELP: &str = "\
登录方式（/login 后接子命令）
  /login            打开登录页，登录后取 userToken 粘回来
  /login browser    同上（默认）
  /login token      手动粘贴 userToken
  /login passwd     手机号 / 邮箱 + 密码
  /login wechatqr   微信扫码（二维码直接画在终端里）

userToken 的取法：在浏览器里登录 chat.deepseek.com，
按 F12 打开控制台执行 localStorage.getItem('userToken')，把结果粘进来。";

#[derive(Default)]
struct Args {
    config_dir: Option<String>,
    command: Option<String>,
    resume: bool,
    selftest: bool,
    info: bool,
    /// 只扫描浏览器存储并报告诊断结果（找到凭证就顺手保存）
    grab: bool,
    help: bool,
}

fn parse_args(argv: &[String]) -> Args {
    let mut args = Args::default();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--help" | "-h" => args.help = true,
            "--selftest" => args.selftest = true,
            "--info" => args.info = true,
            "--grab" => args.grab = true,
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
    if args.grab {
        std::process::exit(grab(&paths));
    }
    if args.info {
        // 与 /info 同一份内容，但不需要进 TUI —— 也方便贴到 bug 报告里
        let config = config::load_config(&paths);
        let dir = paths.config_dir.display().to_string();
        for line in sysinfo::report(&dir, config.language) {
            println!("{line}");
        }
        return;
    }
    if args.selftest {
        std::process::exit(selftest(&paths));
    }

    if let Err(e) = run(paths, args.resume) {
        cleanup_terminal();
        eprintln!("启动失败：{e}");
        std::process::exit(1);
    }
}

/// `--grab`：扫描浏览器存储并如实汇报看到了什么；找到凭证就顺手保存。
///
/// 登录不成功时先跑它。输出里能直接区分三种原因：
/// 目录根本没找到（路径假设错了）、目录在但键名一次没出现（键名或 profile 不对）、
/// 键名出现了却提取不出值（记录被 snappy 压过 / 值布局与预期不同，看「键后字节」）。
fn grab(paths: &ConfigPaths) -> i32 {
    let hits = browser::scan();
    if hits.is_empty() {
        println!("没找到浏览器的 Local Storage 目录（浏览器访问过 chat.deepseek.com 吗？）");
        return 1;
    }

    let mut found: Option<(String, String)> = None;
    for hit in &hits {
        println!("{}  {}", hit.browser, hit.dir.display());
        println!(
            "  userToken {} 次 · 域名 {} · {}",
            hit.key_hits,
            if hit.origin { "在" } else { "不在" },
            match &hit.token {
                Some(t) => format!("凭证 {} 字符", t.chars().count()),
                None => "未取到凭证".to_string(),
            }
        );
        if found.is_none() {
            found = hit.token.as_ref().map(|t| (hit.browser.clone(), t.clone()));
        }
    }

    match found {
        Some((who, token)) => {
            sysinfo::close_browser(&who);
            let ua = config::load_config(paths).user_agent;
            match auth::save_token_from_input(paths, &token, &ua) {
                Ok(_) => {
                    println!("\n登录成功！（来自 {who}）");
                    0
                }
                Err(e) => {
                    println!("\n凭证已取到，保存失败：{e}");
                    1
                }
            }
        }
        None => {
            println!("\n没取到凭证。");
            2
        }
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
    /// 交互式登录的当前阶段（见 STAGE_*）
    login_stage: u8,
    /// 密码登录第一步填的账号
    login_account: String,
    /// 本会话累计的 token 用量（状态栏）
    total_tokens: u64,
    /// 最近一轮的生成速率（token/s），还没跑过就是 None
    last_rate: Option<f64>,
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

    /// 设备标识：密码登录要带上它。没有就随机生成一个并落盘，
    /// 保证同一台机器每次登录用的是同一个 id。
    fn device_id(&mut self) -> String {
        if self.config.device_id.is_empty() {
            use rand::Rng;
            let mut rng = rand::thread_rng();
            let mut id = String::with_capacity(32);
            for _ in 0..16 {
                id.push_str(&format!("{:02x}", rng.gen::<u8>()));
            }
            self.config.device_id = id;
            self.persist();
        }
        self.config.device_id.clone()
    }

    fn status_text(&self) -> String {
        // 最底下这一行同时承担「当前状态」和「两个开关的按键」：
        // 不用记 Ctrl+T / Ctrl+S 是什么，看一眼最底下就行。
        // 最底下这行承担三件事：当前状态、两个开关的按键、累计用量。
        // 本轮用量/耗时仍留在输出区（TurnDone 那一行），这里放累计值。
        let mut usage = String::new();
        if self.total_tokens > 0 {
            usage.push_str(&format!(" · Σ{} tok", self.total_tokens));
        }
        if let Some(rate) = self.last_rate {
            usage.push_str(&format!(" · {rate:.1} tok/s"));
        }
        format!(
            "◆ {} · {} · Ctrl+T {} {} · Ctrl+S {} {} · {}{usage}",
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
        login_stage: STAGE_NONE,
        login_account: String::new(),
        total_tokens: 0,
        last_rate: None,
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
                    // 扫码流程在后台拿到了凭证，这里落地
                    UiEvent::Token(token) => {
                        let token = token.clone();
                        do_login(core, app, &token);
                    }
                    // 累计用量：本轮的 token 数与生成速率汇总到状态栏
                    UiEvent::TurnDone { usage, gen_ms, .. } => {
                        if let Some(u) = usage {
                            core.total_tokens += u;
                            core.last_rate = if *gen_ms > 0 {
                                Some(*u as f64 * 1000.0 / *gen_ms as f64)
                            } else {
                                None
                            };
                            app.set_status(core.status_text());
                        }
                    }
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
            // 静默复制：选区本来就还高亮着，不必再往输出区插一行
            clipboard::copy(&text);
        }

        // 交互式登录：App 只负责「要一次输入」，多步流程靠 login_stage 串起来
        if let Some(text) = app.take_login_input() {
            match core.login_stage {
                STAGE_TOKEN => {
                    core.login_stage = STAGE_NONE;
                    do_login(core, app, &text);
                }
                STAGE_ACCOUNT => {
                    core.login_account = text;
                    core.login_stage = STAGE_PASSWORD;
                    app.start_login(true);
                    app.line_styled("请输入密码，回车登录（Esc 取消）", ui::dim());
                }
                STAGE_PASSWORD => {
                    core.login_stage = STAGE_NONE;
                    let account = core.login_account.clone();
                    do_password_login(core, app, &account, &text);
                }
                _ => {}
            }
        }

        // Ctrl+T / Ctrl+S 的开关请求（界面层拿不到 core.config，只能这样回传）
        if let Some(toggle) = app.take_toggle() {
            match toggle {
                ui::Toggle::Thinking => {
                    core.config.thinking = !core.config.thinking;
                    after_toggle(core, app, "toggle.thinking", core.config.thinking, true);
                }
                ui::Toggle::Search => {
                    core.config.search = !core.config.search;
                    after_toggle(core, app, "toggle.search", core.config.search, true);
                }
            }
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
                // Windows 的控制台后端对一次按键会同时上报 Press 与 Release，
                // 不过滤就会「敲一个字符出两个」。Linux/pty 只报 Press，此过滤无副作用。
                if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    continue;
                }
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
        command(trimmed, core, app, rx);
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

/// 会话标题：把提示词压平成一行，最多留 28 字
fn title_of(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() > 28 {
        format!("{}…", chars[..28].iter().collect::<String>())
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
    // 首条提示词就是这次会话的标题：回车之后，右下角的「新会话」立刻换成它
    {
        let mut session = core.session.lock().unwrap();
        if session.messages.is_empty() {
            session.title = title_of(&input);
        }
    }
    core.session_title = core.session.lock().unwrap().title.clone();
    app.set_status(core.status_text());
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

fn command(
    input: &str,
    core: &mut Core,
    app: &mut App,
    rx: &mut Option<Receiver<UiEvent>>,
) {
    let mut it = input.splitn(2, char::is_whitespace);
    let cmd = it.next().unwrap_or("").to_ascii_lowercase();
    let arg = it.next().unwrap_or("").trim().to_string();
    let lang = core.lang;

    match cmd.as_str() {
        "/help" => {
            // 只讲界面里的命令；命令行参数（--config-dir 那些）用 `dsp --help` 看
            for line in HELP.lines().skip_while(|l| !l.starts_with("快捷键")) {
                app.line_styled(line.to_string(), help_style(line));
            }
        }
        "/info" => {
            let dir = core.paths.config_dir.display().to_string();
            for line in sysinfo::report(&dir, lang) {
                app.line(line);
            }
        }
        "/open" => {
            let url = if arg.is_empty() {
                "https://chat.deepseek.com".to_string()
            } else {
                arg.clone()
            };
            let list = sysinfo::detect_browsers();
            let in_wsl = sysinfo::wsl_version().is_some();
            match sysinfo::resolve(&list, &core.config.browser) {
                Some(i) => match sysinfo::open_url(&list[i], &url, false) {
                    Ok(()) => app.line_styled(
                        format!(
                            "已用 {} 打开 {url}",
                            sysinfo::describe(&list[i], in_wsl, lang)
                        ),
                        ui::ok(),
                    ),
                    Err(e) => app.line_styled(format!("打开浏览器失败：{e}"), ui::err()),
                },
                None => app.line_styled(
                    if lang == Lang::Zh {
                        "没有可用浏览器，输入 /info 看看环境。"
                    } else {
                        "No browser available; run /info."
                    },
                    ui::warn(),
                ),
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
            after_toggle(core, app, "toggle.thinking", core.config.thinking, false);
        }
        "/search" => {
            core.config.search = toggle(&arg).unwrap_or(!core.config.search);
            after_toggle(core, app, "toggle.search", core.config.search, false);
        }
        // /thinking-view 已取消，用户路径改走 Ctrl+O。
        // 这条留作内部实现，免得 show_thinking / push_thinking 变成死代码。
        "/__thinking-view" => {
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
        // /session 不带参数列会话，带编号进入该会话并载入上下文
        "/session" => {
            let all = Session::list(&core.paths.sessions_dir);
            if arg.is_empty() {
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
                    app.line_styled("用 /session <序号> 进入该会话并载入上下文。", ui::dim());
                }
            } else {
                let index: usize = match arg.trim_start_matches('#').parse::<usize>() {
                    Ok(n) if n >= 1 && n <= all.len() => n - 1,
                    _ => {
                        app.line_styled("序号无效，输入 /session 看列表。", ui::warn());
                        return;
                    }
                };
                let picked = all[index].clone();
                let count = picked.messages.len();
                core.session_title = picked.title.clone();
                *core.session.lock().unwrap() = picked;
                core.total_tokens = 0;
                core.last_rate = None;
                app.set_status(core.status_text());
                app.line_styled(
                    format!("已进入 {}（载入 {count} 条上下文）", core.session_title),
                    ui::ok(),
                );
            }
        }
        "/new" => {
            let cwd = core.session.lock().unwrap().cwd.clone();
            *core.session.lock().unwrap() = Session::new(cwd, "新会话");
            core.session_title = "新会话".to_string();
            core.total_tokens = 0;
            core.last_rate = None;
            app.clear_all();
            app.set_status(core.status_text());
            app.line_styled("已新建会话。", ui::ok());
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
        // /login 后接子命令：browser（默认）/ token / passwd / wechatqr
        "/login" => {
            let what = if arg.is_empty() { "browser" } else { arg.as_str() };
            match what {
                "browser" | "web" => open_login_page(core, app, rx),
                "token" | "paste" => {
                    core.login_stage = STAGE_TOKEN;
                    app.start_login(true);
                    app.line_styled("粘贴 userToken 后回车（Esc 取消）", ui::dim());
                }
                "passwd" | "password" => {
                    core.login_stage = STAGE_ACCOUNT;
                    app.start_login(false);
                    app.line_styled("输入手机号或邮箱，回车继续（Esc 取消）", ui::dim());
                }
                "wechatqr" | "wechat" | "qr" => login_with_wechat_qr(core, app, rx),
                // 兼容旧写法 /login <userToken>
                other if other.len() >= 16 && !other.contains(' ') => do_login(core, app, other),
                _ => {
                    for line in LOGIN_HELP.lines() {
                        app.line(line);
                    }
                }
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

/// /login browser：先把浏览器里已有的登录态读出来，没有就打开登录页等着，
/// 登录成功后再自动把 userToken 读回来 —— 全程不用手动复制粘贴。
fn open_login_page(core: &mut Core, app: &mut App, rx: &mut Option<Receiver<UiEvent>>) {
    if let Ok(token) = std::env::var("DSP_TOKEN") {
        if !token.trim().is_empty() {
            let token = token.trim().to_string();
            do_login(core, app, &token);
            return;
        }
    }

    // 1) 浏览器里已经登录过：直接拿来用，页面都不用开。
    //    这里不额外打字，成功提示统一由 do_login 给。
    if let Some((who, token)) = browser::extract_user_token() {
        do_login(core, app, &token);
        // 凭证到手，浏览器不用留着了
        sysinfo::close_browser(&who);
        return;
    }

    // 2) 没有就打开登录页（默认 Edge）
    let list = sysinfo::detect_browsers();
    let in_wsl = sysinfo::wsl_version().is_some();
    match sysinfo::resolve(&list, &core.config.browser) {
        Some(i) => {
            let shown = sysinfo::describe(&list[i], in_wsl, core.lang);
            match sysinfo::open_url(&list[i], SIGN_IN_URL, list[i].label.contains("Edge")) {
                // Edge 用 --app 起独立窗口，登录页看起来就是个登录框
                Ok(()) => {
                    app.line_styled(format!("已用 {shown} 打开登录页"), ui::ok());
                }
                Err(e) => {
                    app.line_styled(format!("打开浏览器失败：{e}"), ui::err());
                    return;
                }
            }
        }
        None => {
            app.line_styled(
                format!("没检测到浏览器，手动打开 {SIGN_IN_URL} 登录。"),
                ui::warn(),
            );
            app.line_styled("登录完成后仍会自动读取凭证。", ui::dim());
        }
    }

    // 3) 后台盯着浏览器存储，登录一完成就把 token 读回来
    let (tx, receiver): (Sender<UiEvent>, Receiver<UiEvent>) = mpsc::channel();
    *rx = Some(receiver);
    std::thread::spawn(move || {
        // 每 2 秒扫一次，最多等 5 分钟
        for _ in 0..150 {
            std::thread::sleep(Duration::from_secs(2));
            if let Some((who, token)) = browser::extract_user_token() {
                sysinfo::close_browser(&who);
                let _ = tx.send(UiEvent::Token(token));
                let _ = tx.send(UiEvent::Finished);
                return;
            }
        }
        let _ = tx.send(UiEvent::Finished);
    });
}

/// 密码登录第二步：拿账号 + 密码去换 token
fn do_password_login(core: &mut Core, app: &mut App, account: &str, password: &str) {
    let client = match core.client() {
        Ok(c) => c,
        Err(e) => {
            app.line_styled(format!("! {e}"), ui::err());
            return;
        }
    };
    let device_id = core.device_id();
    app.line_styled("正在登录…", ui::dim());
    match client.login_with_password(account, password, &device_id) {
        Ok(token) => do_login(core, app, &token),
        Err(e) => {
            app.line_styled(format!("登录失败：{e}"), ui::err());
            app.line_styled("若提示字段不符，把上面这段原文发我就能改对。", ui::dim());
        }
    }
}

/// 微信扫码登录。
///
/// 整个流程丢到后台线程做（取编号 → 下载图片 → 轮询扫码状态 → 换 token），
/// 结果用 UiEvent 回传，界面照常刷新，不会在等扫码的时候卡死。
fn login_with_wechat_qr(core: &mut Core, app: &mut App, rx: &mut Option<Receiver<UiEvent>>) {
    let client = match core.client() {
        Ok(c) => c,
        Err(e) => {
            app.line_styled(format!("! {e}"), ui::err());
            return;
        }
    };
    let device_id = core.device_id();
    let (tx, receiver): (Sender<UiEvent>, Receiver<UiEvent>) = mpsc::channel();
    *rx = Some(receiver);
    app.line_styled("正在获取微信二维码…", ui::dim());
    std::thread::spawn(move || {
        wechat_login_worker(&client, &device_id, &tx);
        let _ = tx.send(UiEvent::Finished);
    });
}

/// 后台线程：取码 → 画出来 → 轮询 → 过期就换一张 → 拿到 code 换 token
fn wechat_login_worker(client: &DeepSeekClient, device_id: &str, tx: &Sender<UiEvent>) {
    // 一张码大约两分钟，最多换 5 张
    for round in 1..=5 {
        let uuid = match client.wechat_qr_uuid() {
            Ok(u) => u,
            Err(e) => {
                let _ = tx.send(UiEvent::Error(format!("获取二维码编号失败：{e}")));
                return;
            }
        };
        let png = match client.wechat_qr_png(&uuid) {
            Ok(p) => p,
            Err(e) => {
                let _ = tx.send(UiEvent::Error(format!("下载二维码图片失败：{e}")));
                return;
            }
        };

        let _ = tx.send(UiEvent::Line(String::new()));
        match qr_art_from_png(&png) {
            Ok(art) => {
                for line in art {
                    let _ = tx.send(UiEvent::Line(line));
                }
            }
            Err(e) => {
                let _ = tx.send(UiEvent::Error(format!("二维码渲染失败：{e}")));
                return;
            }
        }
        let _ = tx.send(UiEvent::Line(String::new()));
        let _ = tx.send(UiEvent::Notice(format!(
            "第 {round} 张码：用微信扫上面这个二维码，并在手机上确认"
        )));

        // 每 2 秒问一次，单张码最多等 90 秒
        let mut scanned = false;
        for _ in 0..45 {
            std::thread::sleep(Duration::from_secs(2));
            match client.wechat_scan_state(&uuid) {
                // 405 = 已确认，带 wx_code
                Ok((405, code)) if !code.is_empty() => {
                    let _ = tx.send(UiEvent::Notice("已确认，正在换取凭证…".to_string()));
                    match client.login_by_wechat(&code, device_id) {
                        Ok(token) => {
                            let _ = tx.send(UiEvent::Token(token));
                            return;
                        }
                        Err(e) => {
                            let _ = tx.send(UiEvent::Error(format!("换取凭证失败：{e}")));
                            let _ = tx.send(UiEvent::Line(
                                "这一步的接口路径我查不到，上面是服务端原文，发我即可修正。"
                                    .to_string(),
                            ));
                            return;
                        }
                    }
                }
                // 403 = 过期或被取消
                Ok((403, _)) => break,
                // 404 = 已扫待确认
                Ok((404, _)) => {
                    if !scanned {
                        scanned = true;
                        let _ = tx.send(UiEvent::Notice("已扫码，请在手机上确认登录".to_string()));
                    }
                }
                // 408 = 还没扫，继续等
                Ok(_) => {}
                Err(e) => {
                    let _ = tx.send(UiEvent::Error(format!("轮询扫码状态失败：{e}")));
                    return;
                }
            }
        }
        let _ = tx.send(UiEvent::Notice("二维码已过期，重新获取…".to_string()));
    }
    let _ = tx.send(UiEvent::Error(
        "连着几张二维码都过期了，稍后再试 /login wechatqr".to_string(),
    ));
}

/// 把二维码图片解码出内容，再用 qrcode 重画成终端字符画。
///
/// 不直接缩放像素：图片里有多少像素、留了多宽的白边都不确定，
/// 缩放比例一旦和模块数对不上，画出来的二维码就扫不出来了。
/// 解出内容重画才能保证每个模块正好一个格。
fn qr_art_from_png(png: &[u8]) -> Result<Vec<String>, String> {
    let img = image::load_from_memory(png)
        .map_err(|e| e.to_string())?
        .to_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(img);
    let grids = prepared.detect_grids();
    let grid = grids.first().ok_or_else(|| "图片里没找到二维码".to_string())?;
    let (_meta, content) = grid.decode().map_err(|e| format!("解码二维码失败：{e}"))?;
    render_qr(&content)
}

/// 把二维码内容画成终端字符画（Dense1x2：一个字符高塞两行，手机扫得动）
fn render_qr(content: &str) -> Result<Vec<String>, String> {
    let code = qrcode::QrCode::new(content.as_bytes()).map_err(|e| e.to_string())?;
    let art = code.render::<qrcode::render::unicode::Dense1x2>().build();
    Ok(art.lines().map(|l| l.to_string()).collect())
}

/// 保存一份 token。本程序不做浏览器自动化，token 由用户提供
/// （粘贴 / 环境变量 / 直接复用磁盘上已有的凭证，加密格式未变）。
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
            app.line_styled("登录成功！", ui::ok());
        }
        Err(e) => app.line_styled(format!("{}：{e}", core.t("login.failed")), ui::err()),
    }
}

/// /help 每一行的配色：段落标题加粗黄色，命令行青色，按键说明品红，其余灰。
/// 输出区是按行着色的（一行一个样式），所以这里只能按整行来判断。
fn help_style(line: &str) -> ratatui::style::Style {
    use ratatui::style::{Color, Modifier, Style};
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Style::default();
    }
    // 段落标题：顶格、不以 / 开头
    if line == trimmed && !trimmed.starts_with('/') {
        return Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
    }
    if trimmed.starts_with('/') || trimmed.starts_with('!') {
        return Style::default().fg(Color::Cyan);
    }
    if trimmed.contains("Ctrl+") || trimmed.contains("Enter") || trimmed.contains("Esc") {
        return Style::default().fg(Color::Magenta);
    }
    Style::default().fg(Color::Gray)
}

/// 落地一个开关。
///
/// `quiet` 给 Ctrl+T / Ctrl+S 用：状态栏那一行本来就写着「Ctrl+T 深度思考 开」，
/// 再往输出区插一行只会把正文冲散，所以快捷键路径不打印，命令路径保留回显。
fn after_toggle(core: &mut Core, app: &mut App, key: &str, value: bool, quiet: bool) {
    core.persist();
    app.set_status(core.status_text());
    if quiet {
        return;
    }
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


