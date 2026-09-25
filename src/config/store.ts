// 运行时配置读写 —— config.json 与 models.json
//
// 配置项会被持久化，用于保存语言、思考/搜索开关、UA、PoW WASM 地址等。
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import type { ConfigPaths } from "./dirs.js";
import { detectLang, normalizeLang, type Lang } from "../i18n/index.js";

/** 运行时配置结构 */
export interface AppConfig {
	/** 界面与提示词语言 */
	language: Lang;
	/** 是否开启深度思考（对应 DeepSeek thinking_enabled） */
	thinking: boolean;
	/** 是否开启智能搜索（对应 DeepSeek search_enabled） */
	search: boolean;
	/**
	 * 是否展开显示思考全文。
	 * false（默认）= 折叠：思考期间只显示单行实时指示，结束后留一行摘要；
	 * true = 展开：思考内容逐字流式显示。
	 */
	showThinking: boolean;
	/** 当前模型 id：deepseek-chat | deepseek-reasoner */
	model: string;
	/** 登录时捕获的浏览器 User-Agent（所有请求复用） */
	userAgent: string;
	/** PoW WASM 地址（DeepSeek 更新后需手动替换） */
	wasmUrl: string;
	/** DeepSeek API 基础地址 */
	apiBase: string;
	/** 客户端版本头 x-client-version */
	clientVersion: string;
	/** 客户端平台头 x-client-platform */
	clientPlatform: string;
	/** 客户端区域头 x-client-locale */
	clientLocale: string;
	/** 可选代理（http:// 或 socks5://），用于绕过 WAF 区域限制 */
	proxy: string;
	/** 两次上游请求之间的最小间隔（毫秒），用于规避风控 */
	requestIntervalMs: number;
	/** 单轮最多工具调用轮数 */
	maxToolSteps: number;
	/**
	 * 上下文模式：
	 *   reuse  —— 复用同一个网页会话，每轮只发增量（会话 1:1，省 token）
	 *   replay —— 每轮把完整历史打包重发（一次性会话，最稳，token 消耗大）
	 */
	contextMode: "reuse" | "replay";
}

/** 精简后的模型清单（单一 provider） */
export interface ModelEntry {
	id: string;
	name: string;
	thinkingDefault: boolean;
}

export const PROVIDER_ID = "deepseek-web";

export const MODELS: ModelEntry[] = [
	{ id: "deepseek-chat", name: "DeepSeek Chat", thinkingDefault: false },
	{ id: "deepseek-reasoner", name: "DeepSeek Reasoner", thinkingDefault: true },
];

/** 默认 PoW WASM 地址，DeepSeek 更新静态资源后需替换 */
export const DEFAULT_WASM_URL =
	"https://fe-static.deepseek.com/chat/static/sha3_wasm_bg.7b9ca65ddd.wasm";

const DEFAULT_USER_AGENT =
	"Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36";

export function defaultConfig(): AppConfig {
	return {
		language: detectLang(),
		thinking: false,
		search: true,
		showThinking: false,
		model: MODELS[0].id,
		userAgent: DEFAULT_USER_AGENT,
		wasmUrl: DEFAULT_WASM_URL,
		apiBase: "https://chat.deepseek.com/api/v0",
		clientVersion: "2.0.0",
		clientPlatform: "web",
		clientLocale: "zh_CN",
		proxy: "",
		requestIntervalMs: 1200,
		maxToolSteps: 25,
		contextMode: "reuse",
	};
}

function ensureDirFor(file: string): void {
	const dir = dirname(file);
	if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
}

/** 读取 JSON 文件，失败时返回 undefined */
function readJson<T>(file: string): T | undefined {
	try {
		if (!existsSync(file)) return undefined;
		const raw = readFileSync(file, "utf8");
		return JSON.parse(raw) as T;
	} catch {
		return undefined;
	}
}

/** 原子化写入 JSON */
function writeJson(file: string, value: unknown): void {
	ensureDirFor(file);
	writeFileSync(file, `${JSON.stringify(value, null, "\t")}\n`, "utf8");
}

/** 加载配置；缺失字段用默认值补齐，并回写文件 */
export function loadConfig(paths: ConfigPaths): AppConfig {
	const defaults = defaultConfig();
	const raw = readJson<Partial<AppConfig>>(paths.configFile) ?? {};
	const merged: AppConfig = { ...defaults };
	for (const key of Object.keys(defaults) as (keyof AppConfig)[]) {
		const value = raw[key];
		if (value === undefined || value === null) continue;
		if (key === "language") {
			merged.language = normalizeLang(value) ?? defaults.language;
		} else if (typeof defaults[key] === typeof value) {
			// 仅接受类型一致的字段，避免脏数据污染
			(merged as unknown as Record<string, unknown>)[key] = value;
		}
	}
	if (!existsSync(paths.configFile)) {
		writeJson(paths.configFile, merged);
	}
	return merged;
}

/** 保存配置（全量写入） */
export function saveConfig(paths: ConfigPaths, config: AppConfig): void {
	writeJson(paths.configFile, config);
}

/** 生成 / 补全 models.json（仅一个 provider） */
export function ensureModelsFile(paths: ConfigPaths): void {
	if (existsSync(paths.modelsFile)) return;
	writeJson(paths.modelsFile, {
		providers: {
			[PROVIDER_ID]: {
				name: "DeepSeek Web",
				apiBase: "https://chat.deepseek.com/api/v0",
				notes: "DeepSeek 网页版逆向接口，仅此一个供应商。",
				models: MODELS.map((m) => ({
					id: m.id,
					name: m.name,
					reasoning: m.thinkingDefault,
				})),
			},
		},
	});
}
