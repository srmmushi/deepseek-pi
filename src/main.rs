// 源码按职责分了四个目录（api / chat / infra / tui）。目录只负责归类，
// 下面这些别名把模块重新挂回 crate 根，于是 `crate::config::Lang`、
// `ui::App` 这类原有写法全部照旧，各个文件里的 use 一句都不用改。
pub(crate) mod api;
pub(crate) mod chat;
pub(crate) mod infra;
pub(crate) mod tui;

pub(crate) use api::{client as deepseek, stream};
pub(crate) use chat::{agent, prompt, tools};
pub(crate) use infra::{auth, browser, clipboard, config};
pub(crate) use tui::{app as ui, i18n, sysinfo};

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
      --dump-session        打印会话绑定诊断（网页会话 id、按 id 查标题的结果、
                            列表接口的原始返回、接口探针）后退出
      --dump-turn           发一轮（关深度思考、开联网搜索、新开会话）并打印原始 SSE，
                            用来确认响应里到底有什么（比如搜索结果块）
      --grab                只扫描浏览器存储并报告诊断；找到凭证就顺手保存
  -h, --help                显示本帮助

快捷键
  Enter 发送          Esc×2 停止本轮（单次 Esc 先清选区 / 取消登录）
  Ctrl+C×2 退出程序   有选区时 Ctrl+C 改为复制
  右键               复制选区
  Ctrl+T / Ctrl+S    深度思考 / 智能搜索
  Ctrl+O             展开或收起最近一个块
  Ctrl+↑ / Ctrl+↓    历史顶部 / 回到底部        滚轮 / PgUp / PgDn 翻页
  左键拖动           选择文本        左键单击块头 折叠

命令
  /help /login /logout /new /session /clear /goto /thinking /search
  /model /lang /status /info /open /export /system-prompt /quit

会话
  /new               新建会话（清空上下文）
  /session           列出当前目录的会话
  /session all       列出全部会话（附路径，按终端宽度收窄）
  /session <序号>    进入该会话：清屏并回放全部上下文（含思考）
  /session debug     诊断网页会话绑定（会话名取不回来时用它）
  /export            把本次会话导出成 Markdown（写在当前目录）

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
    /// --dump-session：打印会话绑定诊断后退出（不进界面，方便复制）
    dump_session: bool,
    /// --dump-turn：发一轮（关思考、开联网搜索）并打印原始 SSE 后退出
    dump_turn: bool,
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
            "--dump-session" => args.dump_session = true,
            "--dump-turn" => args.dump_turn = true,
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
        let login = auth::load_auth(&paths);
        for line in sysinfo::report(&dir, login.as_ref(), config.language) {
            println!("{line}");
        }
        return;
    }
    if args.dump_session {
        dump_session(&paths);
        return;
    }
    if args.dump_turn {
        dump_turn(&paths);
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
        println!("没找到浏览器的 Local Storage 目录。找过这些起点：");
        for root in browser::search_roots() {
            println!(
                "  [{}] {}",
                if root.is_dir() { "有" } else { "无" },
                root.display()
            );
        }
        return 1;
    }

    let mut found: Option<(String, String)> = None;
    for hit in &hits {
        println!(
            "{}  {}  {}",
            hit.browser,
            hit.dir.display(),
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
    /// /logout 丢掉的那份凭证。浏览器存储里可能还留着，重新登录时要忽略它
    discarded: Option<String>,
    /// 本会话用的系统提示词全文，连同对应的会话 id 一起存。
    ///
    /// 会话内**刻意冻结**：这段文本是请求的最前面，服务端的上下文缓存按前缀逐字节命中，
    /// 中途改动会让整段缓存作废（而且 Reuse 模式下首轮就定下来了，改了也不会重新下发）。
    /// 换会话时 id 变了，自动重建。
    system_text_cache: Option<(String, String)>,
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
        // 把中断标志换成客户端那一个：读流的地方要靠它轮询，
        // 否则服务端长时间不发数据时，停止要等到下一个数据块才生效。
        self.aborted = client.abort_flag();
        self.client = Some(Arc::new(client));
        Ok(self.client.clone().unwrap())
    }

    /// 系统提示词 + 工具说明。同一会话内保持不变（见字段注释）。
    fn system_text(&mut self) -> String {
        let session_id = self.session.lock().unwrap().id.clone();
        if let Some((cached, text)) = &self.system_text_cache {
            if *cached == session_id {
                return text.clone();
            }
        }
        let prompt = prompt::load_system_prompt(&self.paths, self.lang);
        let text = agent::build_system_text(&prompt, self.lang);
        self.system_text_cache = Some((session_id, text.clone()));
        text
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

/// `--dump-turn`：发一轮对话，把原始 SSE 原样打出来。
///
/// 关掉深度思考（输出短）、打开联网搜索（正是不确定形状的那部分），
/// 并且开一个**全新的网页会话**，不碰你正在用的那个。
fn dump_turn(paths: &ConfigPaths) {
    let Some(auth) = auth::load_auth(paths) else {
        println!("未登录，先跑 dsp 并执行 /login。");
        return;
    };
    let config = config::load_config(paths);
    let client = match DeepSeekClient::new(&config) {
        Ok(c) => c,
        Err(e) => {
            println!("客户端初始化失败：{e}");
            return;
        }
    };
    let prompt = "用一句话回答：今天是几号？请联网确认后再回答。";
    println!("提示词：{prompt}");
    println!("（新会话 · 深度思考 关 · 联网搜索 开）");
    println!("--- 原始 SSE 开始 ---");
    let mut solver =
        match deepseek::load_pow_solver(&config.wasm_url, &config.user_agent, &config.proxy) {
            Ok(s) => s,
            Err(e) => {
                println!("PoW 初始化失败：{e}");
                return;
            }
        };
    match deepseek::dump_turn(&client, &mut solver, &auth.token, prompt) {
        Ok(()) => println!("\n--- 原始 SSE 结束 ---"),
        Err(e) => println!("\n--- 出错：{e} ---"),
    }
}

/// `--dump-session`：把「本地会话 ↔ 网页会话」的绑定情况打到 stdout。
///
/// 专为排查「会话名取不回来」而设，它把三件事分开说清楚：
/// 1. 网页会话 id 到底绑上没有 —— 没绑的话，标题查询根本不会发出；
/// 2. 按 id 查标题的结果；
/// 3. 列表接口到底返回了什么（形状对不上时，一眼能看出来）。
/// 输出在终端里可直接复制，不必在界面里拖选。
fn dump_session(paths: &ConfigPaths) {
    let Some(session) = Session::latest(&paths.sessions_dir) else {
        println!("没有本地会话（先跑一轮对话）。");
        return;
    };
    println!("本地会话 id   {}", session.id);
    println!("本地标题      {}", session.title);
    println!(
        "网页会话 id   {}",
        session
            .handle
            .session_id
            .clone()
            .unwrap_or_else(|| "（空 —— 标题查询不会发出）".to_string())
    );
    println!(
        "父消息 id     {}",
        session
            .handle
            .parent_message_id
            .map(|v| v.to_string())
            .unwrap_or_else(|| "（空）".to_string())
    );
    if let Some(id) = &session.handle.session_id {
        println!("会话链接      https://chat.deepseek.com/a/chat/s/{id}");
    }

    let Some(auth) = auth::load_auth(paths) else {
        println!("\n未登录，无法查接口。");
        return;
    };
    let config = config::load_config(paths);
    let client = match DeepSeekClient::new(&config) {
        Ok(c) => c,
        Err(e) => {
            println!("\n客户端初始化失败：{e}");
            return;
        }
    };

    if let Some(id) = &session.handle.session_id {
        match client.session_title(&auth.token, id) {
            Some(t) => println!("\n按 id 查到的标题：{t}"),
            None => println!("\n按 id 查标题：没取到（见下面的原始返回）"),
        }
    }

    println!("\n接口根地址    {}", config.api_base);
    println!("POST {}/chat_session/fetch_page", config.api_base);
    println!("原始返回（含状态码）：");
    println!("{}", client.session_list_raw(&auth.token, 20000));
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
        system_text_cache: None,
        paths,
        login_stage: STAGE_NONE,
        login_account: String::new(),
        total_tokens: 0,
        last_rate: None,
        discarded: None,
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
                if ctrl_c {
                    // 有选区时 Ctrl+C 仍然是「复制」；没有选区则连按两下退出程序
                    if app.has_selection() {
                        app.on_key(key);
                    } else if app.double_pressed(ui::DoubleAction::Quit) {
                        app.quit = true;
                    }
                    continue;
                }
                if app.on_key(key) {
                    app.quit = true;
                }
                // Esc 连按两下 = 停止本轮（单次 Esc 只清选区 / 取消登录）
                if app.take_esc() && app.double_pressed(ui::DoubleAction::Stop) {
                    core.abort();
                    // 立刻给个回执：真正松手在读流那一侧（最迟约 1 秒）
                    app.set_status("正在停止…".to_string());
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
    // 会话名交给服务端生成（和网页端一样），本地不拿提示词顶替；
    // 首轮跑完会去取一次，取到再刷新状态栏
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
            let login = auth::load_auth(&core.paths);
            for line in sysinfo::report(&dir, login.as_ref(), lang) {
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
        // /session            只列当前目录的会话
        // /session all        列出全部会话（附路径，按终端宽度收窄）
        // /session <编号>     进入该会话并载入上下文
        "/session" => {
            let cwd = core.session.lock().unwrap().cwd.clone();
            let all = Session::list(&core.paths.sessions_dir);
            let mine: Vec<Session> = all
                .iter()
                .filter(|s| s.cwd == cwd)
                .cloned()
                .collect();
            let width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

            if arg.eq_ignore_ascii_case("all") {
                if all.is_empty() {
                    app.line_styled("暂无历史会话。", ui::dim());
                } else {
                    app.line_styled(
                        format!("全部会话（{}）· 当前目录 {} 条", all.len(), mine.len()),
                        ui::user_style(),
                    );
                    for (i, s) in all.iter().enumerate() {
                        let dir = ui::shorten_path(&s.cwd.display().to_string(), width / 2);
                        app.line(format!("  #{:<3}{:<22}{dir}", i + 1, truncate(&s.title, 20)));
                    }
                    app.line_styled("/session <序号> 进入（序号按当前目录的列表算）。", ui::dim());
                }
            } else if arg.is_empty() {
                if mine.is_empty() {
                    app.line_styled(
                        "当前目录暂无历史会话（/session all 看全部）。",
                        ui::dim(),
                    );
                } else {
                    app.line_styled(
                        format!("当前目录的会话（{}）", mine.len()),
                        ui::user_style(),
                    );
                    for (i, s) in mine.iter().enumerate() {
                        let count = s.messages.iter().filter(|(r, _)| r != "think").count();
                        app.line(format!(
                            "  #{:<3}{:<24}{} 条消息",
                            i + 1,
                            truncate(&s.title, 22),
                            count
                        ));
                    }
                    app.line_styled("用 /session <序号> 进入该会话并载入上下文。", ui::dim());
                }
            } else if arg.eq_ignore_ascii_case("debug") {
                // 网页端会话名取不回来时用它：把绑定状态与接口原始返回都摊开
                let s = core.session.lock().unwrap().clone();
                app.line_styled("会话绑定诊断", ui::user_style());
                app.line(format!("  本地会话 id   {}", s.id));
                app.line(format!("  当前标题      {}", s.title));
                app.line(format!(
                    "  网页会话 id   {}",
                    s.handle
                        .session_id
                        .clone()
                        .unwrap_or_else(|| "（空）".to_string())
                ));
                app.line(format!(
                    "  父消息 id     {}",
                    s.handle
                        .parent_message_id
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "（空）".to_string())
                ));
                app.line(format!(
                    "  会话链接      {}",
                    match &s.handle.session_id {
                        Some(id) => format!("https://chat.deepseek.com/a/chat/s/{id}"),
                        None => "（还没绑上网页会话）".to_string(),
                    }
                ));
                match core.token.clone() {
                    None => app.line_styled("  未登录，无法查询。", ui::warn()),
                    Some(token) => match core.client() {
                        Err(e) => app.line_styled(format!("  {e}"), ui::err()),
                        Ok(client) => {
                            app.line_styled(
                                "  /chat_session/fetch_page 原始返回（截断）：",
                                ui::dim(),
                            );
                            app.line(format!("  {}", client.session_list_raw(&token, 1500)));
                        }
                    },
                }
            } else {
                let index = match arg.trim_start_matches('#').parse::<usize>() {
                    Ok(n) if n >= 1 && n <= mine.len() => n - 1,
                    _ => {
                        app.line_styled("序号无效，输入 /session 看列表。", ui::warn());
                        return;
                    }
                };
                let picked = mine[index].clone();
                // 思考只用于回放，不算进「几条上下文」
                let count = picked
                    .messages
                    .iter()
                    .filter(|(role, _)| role != "think")
                    .count();
                core.session_title = picked.title.clone();
                *core.session.lock().unwrap() = picked.clone();
                core.total_tokens = 0;
                core.last_rate = None;
                // 清屏并完整回放：不再只给一行「载入 N 条上下文」
                app.clear_all();
                app.line_styled(
                    format!("❯ 已进入「{}」（{count} 条上下文）", core.session_title),
                    ui::user_style(),
                );
                for (role, content) in &picked.messages {
                    app.replay(role, content);
                }
                app.line("");
                app.set_status(core.status_text());
            }
        }
        "/export" => {
            let session = core.session.lock().unwrap().clone();
            let path = session.cwd.join(format!("dsp-{}.md", session.id));
            match export_markdown(&session, &path) {
                Ok(()) => app.line_styled(
                    format!("已导出 {}（{} 条消息）", path.display(), session.messages.len()),
                    ui::ok(),
                ),
                Err(e) => app.line_styled(format!("导出失败：{e}"), ui::err()),
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
                    Ok(()) => app.line_styled(
                        "已恢复默认系统提示词。当前会话的开头保持不变（这样能命中服务端上下文缓存），/new 开新会话后生效。",
                        ui::ok(),
                    ),
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
            // 已经登录就别再走一遍，免得把手头的凭证覆盖掉
            if core.token.is_some() {
                app.line_styled("已登录。要换账号请先 /logout 退出登录。", ui::warn());
                return;
            }
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
            // 记下丢掉的凭证：浏览器里可能还留着它，下次 /login 不能又读回来
            core.discarded = core.token.clone();
            let removed = auth::clear_auth(&core.paths);
            core.token = None;
            app.set_status(core.status_text());
            app.line_styled(
                if removed { "已退出登录。" } else { "本地没有凭证。" },
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
    // 刚 /logout 过就跳过这一步：浏览器里留着的正是刚丢弃的旧凭证，
    // 一读回来就等于压根没退出
    if core.discarded.is_none() {
        if let Some((who, token)) = browser::extract_user_token() {
            do_login(core, app, &token);
            // 凭证到手，浏览器不用留着了
            sysinfo::close_browser(&who);
            return;
        }
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
    let discarded = core.discarded.clone();
    std::thread::spawn(move || {
        // 每 2 秒扫一次，最多等 5 分钟
        for _ in 0..150 {
            std::thread::sleep(Duration::from_secs(2));
            if let Some((who, token)) = browser::extract_user_token() {
                // 忽略 /logout 丢掉的那份，等真正重新登录后的新凭证
                if discarded.as_deref() == Some(token.as_str()) {
                    continue;
                }
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
            core.discarded = None;
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

/// 按字符数截断，超出加省略号
fn truncate(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        text.to_string()
    } else {
        format!("{}…", chars[..max].iter().collect::<String>())
    }
}

/// /export：把这次会话的上下文导出成一份 Markdown
fn export_markdown(session: &Session, path: &std::path::Path) -> std::io::Result<()> {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", session.title));
    out.push_str(&format!("- 会话 ID：`{}`\n", session.id));
    out.push_str(&format!("- 工作目录：`{}`\n", session.cwd.display()));
    out.push_str(&format!("- 消息数：{}\n\n---\n\n", session.messages.len()));
    for (role, content) in &session.messages {
        let who = match role.as_str() {
            "user" => "用户",
            "assistant" => "助手",
            "think" => "思考",
            _ => "工具结果",
        };
        out.push_str(&format!("## {who}\n\n{content}\n\n"));
    }
    std::fs::write(path, out)
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


