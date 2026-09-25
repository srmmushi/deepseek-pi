// 中英双语文案模块
//
// 设计：扁平的 key → 模板字符串映射，`t(key, vars)` 做 {name} 占位替换。
// 语言默认跟随系统（LANG / LC_ALL / Intl），可通过配置项 language 覆盖。
export type Lang = "zh" | "en";

export type MessageKey = keyof typeof zhMessages;

const zhMessages = {
	// ── 应用 ──────────────────────────────────────────────
	"app.tagline": "终端编程助手 · 仅 DeepSeek 网页版",
	"app.welcome": "欢迎使用 {name}，模型供应商已锁定为 DeepSeek 网页版。",
	"app.needLogin": "尚未检测到登录凭证，请先执行 /login。",
	"app.bye": "再见。",

	// ── 状态栏 ────────────────────────────────────────────
	"status.model": "模型",
	"status.thinking": "深度思考",
	"status.search": "智能搜索",
	"status.lang": "语言",
	"status.on": "开",
	"status.off": "关",
	"status.tokens": "tokens",
	"status.busy": "生成中",
	"status.hint": "输入 /help 查看命令，Ctrl+T 切思考，Ctrl+S 切搜索",

	// ── 交互 ──────────────────────────────────────────────
	"repl.thinkingLabel": "思考",
	"repl.answerLabel": "回答",
	"repl.stopped": "（已中断）",
	"repl.working": "正在处理…",
	"repl.maxSteps": "已达到最大工具调用轮数（{max}），停止继续执行。",
	"repl.emptyInput": "输入为空。",

	// ── 登录 / 登出 ───────────────────────────────────────
	"login.starting": "正在启动浏览器，请稍候…",
	"login.waiting": "请在浏览器窗口中完成登录（扫码或账号密码），登录成功后会自动继续…",
	"login.detected": "已检测到登录凭证，正在保存…",
	"login.success": "登录成功，token 已加密保存到 {path}",
	"login.timeout": "登录超时（{minutes} 分钟未检测到凭证），请重试 /login。",
	"login.failed": "登录失败：{error}",
	"login.already": "已存在登录凭证，将重新登录并覆盖。",
	"login.needPlaywright":
		"未找到 Playwright 依赖。请先执行：npm install && npx playwright install chromium（或本机安装 Chrome/Edge 后重试）。",
	"login.ua": "已记录浏览器 User-Agent：{ua}",
	"login.verifying": "已检测到 userToken，正在向 DeepSeek 校验…",
	"login.invalid": "检测到的凭证无效（{error}），继续等待登录完成…",
	"login.lastReject": "最后一次检测到的凭证无效：{error}",
	"logout.done": "已清除本地登录凭证。",
	"logout.none": "本地没有可清除的登录凭证。",

	// ── 开关 ──────────────────────────────────────────────
	"toggle.thinking": "深度思考已{state}。",
	"toggle.search": "智能搜索已{state}。",
	"toggle.model": "已切换模型：{model}",
	"toggle.unknownArg": "参数无效，可用：on | off",

	// ── 工具 ──────────────────────────────────────────────
	"tool.running": "→ 执行 {name}：{detail}",
	"tool.ok": "✓ {name} 完成",
	"tool.fail": "✗ {name} 失败：{error}",
	"tool.unknown": "未知工具：{name}",
	"tool.maxOutput": "（输出已截断，仅显示前 {limit} 字符）",
	"tool.writeDone": "已写入 {path}（{bytes} 字节）",
	"tool.readDone": "已读取 {path}",
	"tool.listDone": "已列出 {path}",
	"tool.execDone": "命令退出码 {code}",

	// ── 系统提示词 ────────────────────────────────────────
	"sp.current": "当前系统提示词（{path}）：",
	"sp.edited": "已用编辑器打开 {path}，保存后下次对话生效。",
	"sp.reset": "系统提示词已恢复默认。",
	"sp.notFound": "系统提示词文件不存在，已生成默认值：{path}",

	// ── 命令 ──────────────────────────────────────────────
	"cmd.helpTitle": "可用命令：",
	"cmd.unknown": "未知命令：{name}。输入 /help 查看全部命令。",
	"cmd.modelList": "可选模型：{models}（当前：{current}）",
	"cmd.langSet": "界面语言已切换为：{lang}",
	"cmd.clear": "已清空会话上下文。",
	"cmd.status": "配置目录：{dir}\n模型：{model}｜思考：{thinking}｜搜索：{search}｜语言：{lang}｜上下文：{mode}",

	// ── 会话 ──────────────────────────────────────────────
	"status.session": "会话",
	"session.current": "当前会话：{title}",
	"session.meta": "工作目录 {cwd} ｜ 网页会话 {web} ｜ 上下文模式 {mode}",
	"session.webUnbound": "未创建（首次发送提示词时创建）",
	"session.new": "已新建会话：{title}（网页会话将在首次发送时创建）",
	"session.listTitle": "会话列表（共 {count} 个，按最近使用排序）：",
	"session.empty": "暂无会话，输入 /new 新建。",
	"session.row": "{index}. {title}  [{id}]",
	"session.rowMeta": "     目录 {cwd} ｜ 网页会话 {web} ｜ 更新于 {updated}",
	"session.switched": "已切换到会话「{title}」，工作目录 {cwd}，网页会话 {web}",
	"session.notFound": "未找到会话：{name}",
	"session.cwdMissing": "会话的工作目录不存在：{cwd}（保持当前目录）",
	"session.hint": "用 /session <序号|id前缀> 切换，/new 新建，/session all 查看全部。",

	// ── 凭证状态 ──────────────────────────────────────────
	"auth.loaded": "已加载登录凭证（token {len} 字符）｜ 执行 /status 可校验有效性",
	"auth.missing": "尚未检测到登录凭证，请执行 /login（也可直接输入 login）",
	"auth.filePresent": "凭证文件：{path}",
	"auth.fileMissing": "凭证文件：不存在（请执行 /login）",
	"auth.tokenInfo": "token 长度 {len}，指纹 {fp}",
	"auth.verifying": "正在向 DeepSeek 校验凭证…",
	"auth.verified": "凭证校验：✔ 有效",
	"auth.verifyFailed": "凭证校验：✘ {error}",
	"auth.reloginHint": "请执行 /login 重新登录（当前凭证已失效，建议先 /logout 清除）。",

	// ── 界面 ──────────────────────────────────────────────
	"ui.dir": "配置目录",
	"ui.cred": "凭证",
	"ui.session": "会话",
	"ui.credLoaded": "已加载（token {len} 字符）· /status 可校验",
	"ui.credMissing": "未登录 · 输入 login 或 /login",
	"ui.webShort": "网页会话 {web}",
	"ui.tips": "直接输入即可对话 · 输入 / 查看全部命令 · Ctrl+T 思考 · Ctrl+S 搜索 · Ctrl+C 中断",
	"bar.hint": "Ctrl+T 思考  ·  Ctrl+S 搜索  ·  /help 命令  ·  Ctrl+C 中断",

	// ── 错误 ──────────────────────────────────────────────
	"error.noToken": "尚未登录或凭证已失效，请执行 /login 重新获取。",
	"error.http": "网络请求失败：{error}",
	"error.api": "DeepSeek 接口返回错误：{error}",
	"error.pow": "PoW 计算失败：{error}。请尝试更新 config.json 中的 wasmUrl。",
	"error.powWasm": "WASM PoW 模块加载失败：{error}",
	"error.rateLimit": "触发上游限流，请稍后重试（已做退避重试）。",
	"error.sessionExpired": "会话已过期（code={code}），请重新执行 /login。",
	"error.waf": "请求被 CloudFront WAF 拦截（通常是美国 IP）。可在 config.json 中配置 proxy 后重试。",
	"error.hint": "上游提示：{msg}",
	"error.unsupported": "当前环境不支持：{error}",
	"error.apiChanged":
		"接口似乎已变更（{detail}）。请检查 DeepSeek 网页版是否可用；如为 PoW 失败，请更新 config.json 的 wasmUrl。",
} as const;

const enMessages: Record<MessageKey, string> = {
	"app.tagline": "Terminal coding agent · DeepSeek Web only",
	"app.welcome": "Welcome to {name}. The only provider is DeepSeek Web.",
	"app.needLogin": "No credentials found. Please run /login first.",
	"app.bye": "Bye.",

	"status.model": "Model",
	"status.thinking": "Thinking",
	"status.search": "Search",
	"status.lang": "Lang",
	"status.on": "on",
	"status.off": "off",
	"status.tokens": "tokens",
	"status.busy": "working",
	"status.hint": "Type /help for commands · Ctrl+T toggles thinking · Ctrl+S toggles search",

	"repl.thinkingLabel": "Thinking",
	"repl.answerLabel": "Answer",
	"repl.stopped": "(interrupted)",
	"repl.working": "Working…",
	"repl.maxSteps": "Reached the maximum number of tool rounds ({max}); stopping.",
	"repl.emptyInput": "Empty input.",

	"login.starting": "Launching browser, please wait…",
	"login.waiting": "Please finish signing in inside the browser window; we will continue automatically.",
	"login.detected": "Credentials detected, saving…",
	"login.success": "Login succeeded. Token encrypted and saved to {path}",
	"login.timeout": "Login timed out (no credentials within {minutes} minutes). Please retry /login.",
	"login.failed": "Login failed: {error}",
	"login.already": "Credentials already exist; signing in again will overwrite them.",
	"login.needPlaywright":
		"Playwright is not installed. Run: npm install && npx playwright install chromium (or install Chrome/Edge locally and retry).",
	"login.ua": "Captured browser User-Agent: {ua}",
	"login.verifying": "userToken detected, verifying against DeepSeek…",
	"login.invalid": "Captured credential is invalid ({error}); still waiting for login to finish…",
	"login.lastReject": "Last captured credential was invalid: {error}",
	"logout.done": "Local credentials cleared.",
	"logout.none": "Nothing to clear.",

	"toggle.thinking": "Deep thinking is now {state}.",
	"toggle.search": "Smart search is now {state}.",
	"toggle.model": "Model switched to: {model}",
	"toggle.unknownArg": "Invalid argument. Use: on | off",

	"tool.running": "→ running {name}: {detail}",
	"tool.ok": "✓ {name} done",
	"tool.fail": "✗ {name} failed: {error}",
	"tool.unknown": "Unknown tool: {name}",
	"tool.maxOutput": "(output truncated, showing first {limit} chars)",
	"tool.writeDone": "Wrote {path} ({bytes} bytes)",
	"tool.readDone": "Read {path}",
	"tool.listDone": "Listed {path}",
	"tool.execDone": "Command exited with code {code}",

	"sp.current": "Current system prompt ({path}):",
	"sp.edited": "Opened {path} in your editor; changes apply on the next turn.",
	"sp.reset": "System prompt reset to default.",
	"sp.notFound": "System prompt file missing; default written to {path}",

	"cmd.helpTitle": "Available commands:",
	"cmd.unknown": "Unknown command: {name}. Type /help to list all commands.",
	"cmd.modelList": "Models: {models} (current: {current})",
	"cmd.langSet": "UI language switched to: {lang}",
	"cmd.clear": "Conversation context cleared.",
	"cmd.status": "Config dir: {dir}\nModel: {model} | Thinking: {thinking} | Search: {search} | Lang: {lang} | Context: {mode}",

	"status.session": "Session",
	"session.current": "Current session: {title}",
	"session.meta": "cwd {cwd} | web session {web} | context mode {mode}",
	"session.webUnbound": "not created yet (created on first prompt)",
	"session.new": "New session created: {title} (web session is created on first prompt)",
	"session.listTitle": "Sessions ({count} total, most recent first):",
	"session.empty": "No sessions yet. Use /new to create one.",
	"session.row": "{index}. {title}  [{id}]",
	"session.rowMeta": "     cwd {cwd} | web {web} | updated {updated}",
	"session.switched": "Switched to \"{title}\", cwd {cwd}, web session {web}",
	"session.notFound": "Session not found: {name}",
	"session.cwdMissing": "Session cwd does not exist: {cwd} (kept current directory)",
	"session.hint": "Use /session <index|id-prefix> to switch, /new to create, /session all to list.",

	"auth.loaded": "Credential loaded (token {len} chars) | run /status to verify it",
	"auth.missing": "No credential found. Run /login (or just type login)",
	"auth.filePresent": "Credential file: {path}",
	"auth.fileMissing": "Credential file: not found (run /login)",
	"auth.tokenInfo": "token length {len}, fingerprint {fp}",
	"auth.verifying": "Verifying credential against DeepSeek…",
	"auth.verified": "Credential check: ✔ valid",
	"auth.verifyFailed": "Credential check: ✘ {error}",
	"auth.reloginHint": "Run /login again (the credential is invalid; /logout first to clear it).",

	"ui.dir": "Config dir",
	"ui.cred": "Credential",
	"ui.session": "Session",
	"ui.credLoaded": "loaded (token {len} chars) · /status to verify",
	"ui.credMissing": "not logged in · type login or /login",
	"ui.webShort": "web {web}",
	"ui.tips": "Type to chat · / lists all commands · Ctrl+T thinking · Ctrl+S search · Ctrl+C cancel",
	"bar.hint": "Ctrl+T thinking  ·  Ctrl+S search  ·  /help commands  ·  Ctrl+C cancel",

	"error.noToken": "Not logged in or credentials expired. Please run /login.",
	"error.http": "Network request failed: {error}",
	"error.api": "DeepSeek API error: {error}",
	"error.pow": "Proof-of-work failed: {error}. Try updating wasmUrl in config.json.",
	"error.powWasm": "Failed to load the WASM PoW module: {error}",
	"error.rateLimit": "Upstream rate limit hit, retrying with backoff.",
	"error.sessionExpired": "Session expired (code={code}). Please run /login again.",
	"error.waf": "Blocked by CloudFront WAF (usually a US IP). Configure proxy in config.json and retry.",
	"error.hint": "Upstream hint: {msg}",
	"error.unsupported": "Unsupported environment: {error}",
	"error.apiChanged":
		"The API seems to have changed ({detail}). Verify chat.deepseek.com still works; if PoW fails, update wasmUrl in config.json.",
};

const TABLES: Record<Lang, Record<string, string>> = {
	zh: zhMessages,
	en: enMessages,
};

/** 跟随系统推断语言，默认中文 */
export function detectLang(): Lang {
	const candidates = [process.env.LC_ALL, process.env.LC_MESSAGES, process.env.LANG].filter(
		Boolean,
	) as string[];
	for (const c of candidates) {
		if (/^zh/i.test(c)) return "zh";
		if (/^en/i.test(c)) return "en";
	}
	try {
		const locale = Intl.DateTimeFormat().resolvedOptions().locale;
		if (locale) return /^zh/i.test(locale) ? "zh" : "en";
	} catch {
		// 忽略：部分环境无 Intl
	}
	return "zh";
}

/** 将语言字符串规整为受支持的值 */
export function normalizeLang(value: unknown): Lang | undefined {
	if (value === "zh" || value === "en") return value;
	return undefined;
}

export interface I18n {
	lang: Lang;
	/** 取文案并做变量替换 */
	t: (key: MessageKey, vars?: Record<string, string | number>) => string;
	/** 切换语言（返回新的实例） */
	withLang: (lang: Lang) => I18n;
}

export function createI18n(lang: Lang): I18n {
	const table = TABLES[lang];
	const t = (key: MessageKey, vars?: Record<string, string | number>): string => {
		let text = table[key] ?? String(key);
		if (vars) {
			for (const [k, v] of Object.entries(vars)) {
				text = text.split(`{${k}}`).join(String(v));
			}
		}
		return text;
	};
	return { lang, t, withLang: (next: Lang) => createI18n(next) };
}
