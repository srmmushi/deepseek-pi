// 应用状态中枢 —— 集中持有配置、凭证、会话、HTTP 客户端与 PoW 求解器
//
// 会话模型：
//   一个 pi 会话 = 一个 SessionRecord = 一个 DeepSeek 网页会话（懒创建）
//   工作目录、历史、网页会话 id 都挂在会话上，切换会话即切换这三者。
import { existsSync } from "node:fs";
import { resolveConfigPaths, type ConfigPaths } from "./config/dirs.js";
import {
	DEFAULT_WASM_URL,
	ensureModelsFile,
	loadConfig,
	MODELS,
	saveConfig,
	type AppConfig,
	type ModelEntry,
} from "./config/store.js";
import { clearAuth, loadAuth, saveAuth } from "./auth/store.js";
import { createI18n, detectLang, type I18n, type Lang } from "./i18n/index.js";
import { DeepSeekClient } from "./deepseek/client.js";
import { loadPowSolver, type PowSolver } from "./deepseek/pow.js";
import { ensureSystemPromptFile, loadSystemPrompt } from "./prompt/system-prompt.js";
import { buildToolDoc } from "./tools/index.js";
import type { ChatMessage } from "./deepseek/prompt.js";
import { deriveTitle, SessionStore, type SessionRecord } from "./agent/session-store.js";

/** 切换会话的结果 */
export type SwitchResult =
	| { ok: true; record: SessionRecord }
	| { ok: false; reason: "notFound" | "cwdMissing" };

export class App {
	readonly paths: ConfigPaths;
	/** 会话存储 */
	readonly sessions: SessionStore;
	config: AppConfig;
	i18n: I18n;
	/** 当前活动会话 */
	activeSession: SessionRecord;

	private client?: DeepSeekClient;
	private solver?: PowSolver;
	private cachedToken?: string;

	private constructor(paths: ConfigPaths, config: AppConfig, session: SessionRecord) {
		this.paths = paths;
		this.config = config;
		this.i18n = createI18n(config.language);
		this.sessions = new SessionStore(paths.sessionsDir);
		this.activeSession = session;
	}

	/** 初始化：加载配置、补齐文件、恢复或新建会话 */
	static async create(cliConfigDir?: string): Promise<App> {
		const paths = resolveConfigPaths(cliConfigDir);
		const config = loadConfig(paths);
		ensureModelsFile(paths);
		ensureSystemPromptFile(paths, config.language);

		// 若已登录且配置里没有 UA，则沿用凭证中记录的 UA
		const auth = loadAuth(paths);
		if (auth?.userAgent && !config.userAgent) {
			config.userAgent = auth.userAgent;
			saveConfig(paths, config);
		}

		const store = new SessionStore(paths.sessionsDir);
		// 每次启动默认开一个新会话（历史会话仍可通过 /session all 切换）
		const session = store.create(process.cwd());

		return new App(paths, config, session);
	}

	// ── 会话 ────────────────────────────────────────────────

	/** 当前会话绑定的工作目录（工具的相对路径基准） */
	get cwd(): string {
		return this.activeSession.cwd;
	}

	/** 当前会话的消息历史 */
	get history(): ChatMessage[] {
		return this.activeSession.messages;
	}

	/** 持久化当前会话 */
	saveSession(): void {
		this.sessions.save(this.activeSession);
	}

	/** 列出全部会话 */
	listSessions(): SessionRecord[] {
		return this.sessions.list();
	}

	/** 新建会话（网页会话懒创建，首次发送提示词时才真正建立） */
	newSession(cwd = this.activeSession.cwd): SessionRecord {
		const record = this.sessions.create(cwd);
		this.activeSession = record;
		return record;
	}

	/** 切换会话：同时切换工作目录与网页会话绑定 */
	switchSession(name: string): SwitchResult {
		const all = this.listSessions();

		// 支持序号（/session all 展示的编号，从 1 开始）
		const index = Number.parseInt(name, 10);
		let record: SessionRecord | undefined;
		if (/^\d+$/.test(name) && index >= 1 && index <= all.length) {
			record = all[index - 1];
		} else {
			record = this.sessions.findByPrefix(name);
		}
		if (!record) return { ok: false, reason: "notFound" };

		if (record.cwd && existsSync(record.cwd)) {
			try {
				process.chdir(record.cwd);
			} catch {
				// chdir 失败不阻断切换，工具仍以 record.cwd 为基准
			}
		} else if (record.cwd) {
			// 目录已不存在：保持当前目录，但仍切换会话
			this.activeSession = record;
			this.saveSession();
			return { ok: false, reason: "cwdMissing" };
		}

		this.activeSession = record;
		this.saveSession();
		return { ok: true, record };
	}

	/**
	 * 重置当前会话的上下文：
	 * 清空本地消息，并解绑网页会话（下次发送会新建一个网页会话）。
	 */
	resetSessionContext(): void {
		this.activeSession.messages = [];
		this.activeSession.deepseekSessionId = null;
		this.activeSession.parentMessageId = null;
		this.saveSession();
	}

	/** 用首条用户输入更新会话标题 */
	maybeSetTitle(input: string): void {
		if (this.activeSession.messages.length === 0) {
			this.activeSession.title = deriveTitle(input);
		}
	}

	// ── 运行时对象 ──────────────────────────────────────────

	async getClient(): Promise<DeepSeekClient> {
		if (!this.client) this.client = await DeepSeekClient.create(this.config);
		return this.client;
	}

	async getSolver(): Promise<PowSolver> {
		if (!this.solver) {
			this.solver = await loadPowSolver(this.config.wasmUrl || DEFAULT_WASM_URL, this.config.userAgent);
		}
		return this.solver;
	}

	/** 配置变更（UA / 代理 / WASM）后需重建客户端与求解器 */
	invalidateRuntime(): void {
		this.client = undefined;
		this.solver = undefined;
	}

	// ── 凭证 ────────────────────────────────────────────────

	/** 读取 token（带进程内缓存） */
	getToken(): string | undefined {
		if (this.cachedToken) return this.cachedToken;
		const auth = loadAuth(this.paths);
		if (auth) this.cachedToken = auth.token;
		return this.cachedToken;
	}

	isLoggedIn(): boolean {
		return this.getToken() !== undefined;
	}

	/** 登录成功后保存凭证，并同步 UA */
	setAuth(token: string, userAgent: string): void {
		saveAuth(this.paths, {
			token,
			userAgent: userAgent || this.config.userAgent,
			capturedAt: Date.now(),
		});
		if (userAgent && userAgent !== this.config.userAgent) {
			this.config.userAgent = userAgent;
			saveConfig(this.paths, this.config);
		}
		this.cachedToken = token;
		this.invalidateRuntime();
	}

	/** 登出：清除凭证缓存与文件 */
	clearAuthStore(): boolean {
		this.cachedToken = undefined;
		return clearAuth(this.paths);
	}

	// ── 配置变更 ────────────────────────────────────────────

	save(): void {
		saveConfig(this.paths, this.config);
	}

	setLanguage(lang: Lang): void {
		this.config.language = lang;
		this.i18n = this.i18n.withLang(lang);
		this.save();
	}

	setThinking(value: boolean): void {
		this.config.thinking = value;
		this.save();
	}

	setSearch(value: boolean): void {
		this.config.search = value;
		this.save();
	}

	setShowThinking(value: boolean): void {
		this.config.showThinking = value;
		this.save();
	}

	/** 切换模型：同时把「默认思考状态」应用到当前开关 */
	setModel(modelId: string): boolean {
		const model = findModel(modelId);
		if (!model) return false;
		this.config.model = model.id;
		this.config.thinking = model.thinkingDefault;
		this.save();
		return true;
	}

	getModel(): ModelEntry {
		return findModel(this.config.model) ?? MODELS[0];
	}

	// ── 提示词 ──────────────────────────────────────────────

	/** 用户可编辑的基础系统提示词 */
	getSystemPrompt(): string {
		return loadSystemPrompt(this.paths, this.config.language);
	}

	/** 工具说明（跟随语言） */
	getToolDoc(): string {
		return buildToolDoc(this.config.language);
	}

	/** 真正注入请求的完整系统文本 = 基础提示词 + 工具说明 */
	buildSystemText(): string {
		return `${this.getSystemPrompt()}\n\n${this.getToolDoc()}`;
	}
}

/** 按 id 查找模型 */
export function findModel(modelId: string): ModelEntry | undefined {
	return MODELS.find((m) => m.id === modelId);
}

/** 供 UI 展示的模型列表 */
export function modelIds(): string[] {
	return MODELS.map((m) => m.id);
}

/** 语言探测（供 index 在无配置时使用） */
export { detectLang };
