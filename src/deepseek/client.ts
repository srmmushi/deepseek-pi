// DeepSeek 网页版 REST 客户端 —— 原始 API 调用层
//
// 对应上游参考实现的账户 / HTTP 客户端层，关键差异：
//   - 走 Node 内置 fetch，不再单独起后端服务；
//   - 所有请求都伪造浏览器头（UA / Origin / Referer），UA 来自登录会话；
//   - 可选用 undici ProxyAgent 走代理，绕过 CloudFront WAF 的地区限制。
import type { AppConfig } from "../config/store.js";
import type { Challenge } from "./pow.js";

const ENDPOINT_SESSION_CREATE = "/chat_session/create";
const ENDPOINT_SESSION_DELETE = "/chat_session/delete";
const ENDPOINT_POW_CHALLENGE = "/chat/create_pow_challenge";
const ENDPOINT_COMPLETION = "/chat/completion";
const ENDPOINT_EDIT_MESSAGE = "/chat/edit_message";
const ENDPOINT_STOP_STREAM = "/chat/stop_stream";

/** 原始接口地址（不带 /api/v0 前缀，供 UI 展示） */
export const ORIGIN = "https://chat.deepseek.com";

/** 业务 / 系统级错误 */
export class ApiError extends Error {
	constructor(
		public readonly code: number,
		message: string,
	) {
		super(message);
		this.name = "ApiError";
	}
}

/** HTTP 层错误 */
export class HttpError extends Error {
	constructor(
		public readonly status: number,
		message: string,
	) {
		super(message);
		this.name = "HttpError";
	}
}

/** CloudFront WAF 拦截（通常因美国 IP） */
export class WafError extends Error {
	constructor() {
		super("WAF challenge");
		this.name = "WafError";
	}
}

/**
 * 由 User-Agent 推导 sec-ch-ua 系列头，让请求更接近真实浏览器。
 * 现代 Chromium 系浏览器都会发送这些 Client Hints。
 */
function clientHints(ua: string): Record<string, string> {
	const chrome = /Chrome\/(\d+)/.exec(ua)?.[1];
	if (!chrome) return {};
	const edge = /Edg\/(\d+)/.exec(ua)?.[1];
	const brands = edge
		? [`"Microsoft Edge";v="${edge}"`, `"Chromium";v="${chrome}"`]
		: [`"Google Chrome";v="${chrome}"`, `"Chromium";v="${chrome}"`];
	brands.push(`"Not(A:Brand";v="24"`);
	const platform = /Windows/.test(ua) ? "Windows" : /Mac OS X/.test(ua) ? "macOS" : "Linux";
	return {
		"sec-ch-ua": brands.join(", "),
		"sec-ch-ua-mobile": "?0",
		"sec-ch-ua-platform": `"${platform}"`,
		"sec-fetch-dest": "empty",
		"sec-fetch-mode": "cors",
		"sec-fetch-site": "same-origin",
	};
}

/** 统一响应信封 */
interface Envelope<T> {
	code: number;
	msg: string;
	data: {
		biz_code: number;
		biz_msg: string;
		biz_data: T | null;
	} | null;
}

/** completion 请求体 */
export interface CompletionPayload {
	chat_session_id: string;
	parent_message_id?: number | null;
	model_type: string;
	prompt: string;
	ref_file_ids: string[];
	thinking_enabled: boolean;
	search_enabled: boolean;
	preempt: boolean;
}

/** 复用的请求选项 */
interface RequestOptions {
	method?: "GET" | "POST";
	token?: string;
	powHeader?: string;
	body?: unknown;
	query?: Record<string, string>;
}

export class DeepSeekClient {
	private lastRequestAt = 0;

	private constructor(
		private readonly config: AppConfig,
		private readonly dispatcher: unknown,
	) {}

	/** 创建客户端（按需初始化代理 dispatcher） */
	static async create(config: AppConfig): Promise<DeepSeekClient> {
		let dispatcher: unknown;
		if (config.proxy) {
			try {
				// 使用变量说明符，避免在未安装 undici 时影响类型检查
				const specifier = "undici";
				const undici = (await import(specifier)) as any;
				dispatcher = new undici.ProxyAgent(config.proxy);
			} catch (e) {
				throw new Error(`proxy 初始化失败（请确认已安装 undici）：${(e as Error).message}`);
			}
		}
		return new DeepSeekClient(config, dispatcher);
	}

	/** 构造伪造的浏览器请求头（对齐真实 Chrome/Edge 的请求形态） */
	private buildHeaders(options: RequestOptions): Record<string, string> {
		const headers: Record<string, string> = {
			"User-Agent": this.config.userAgent,
			Accept: "*/*",
			"Accept-Language": `${this.config.clientLocale.replace("_", "-")},en;q=0.9`,
			Origin: ORIGIN,
			Referer: `${ORIGIN}/`,
			...clientHints(this.config.userAgent),
		};
		if (this.config.clientVersion) headers["x-client-version"] = this.config.clientVersion;
		if (this.config.clientPlatform) headers["x-client-platform"] = this.config.clientPlatform;
		if (this.config.clientLocale) headers["x-client-locale"] = this.config.clientLocale;
		if (options.body !== undefined) headers["Content-Type"] = "application/json";
		if (options.token) headers.Authorization = `Bearer ${options.token}`;
		if (options.powHeader) headers["x-ds-pow-response"] = options.powHeader;
		return headers;
	}

	/** 保守限速：保证两次上游请求之间有最小间隔 */
	private async throttle(): Promise<void> {
		const gap = this.config.requestIntervalMs;
		if (gap <= 0) return;
		const now = Date.now();
		const wait = this.lastRequestAt + gap - now;
		if (wait > 0) await new Promise((r) => setTimeout(r, wait));
		this.lastRequestAt = Date.now();
	}

	private url(path: string, query?: Record<string, string>): string {
		const base = `${this.config.apiBase}${path}`;
		if (!query) return base;
		const qs = new URLSearchParams(query).toString();
		return `${base}?${qs}`;
	}

	/** 发起请求并做 WAF / 状态码检查，返回原始 Response */
	async raw(path: string, options: RequestOptions): Promise<Response> {
		await this.throttle();
		const init: Record<string, unknown> = {
			method: options.method ?? "POST",
			headers: this.buildHeaders(options),
		};
		if (options.body !== undefined) init.body = JSON.stringify(options.body);
		if (this.dispatcher) init.dispatcher = this.dispatcher;

		const res = await fetch(this.url(path, options.query), init as unknown as RequestInit);

		if (res.status === 202 && res.headers.get("x-amzn-waf-action")) {
			throw new WafError();
		}
		if (!res.ok) {
			const text = await res.text().catch(() => "");
			throw new HttpError(res.status, text.slice(0, 300));
		}
		return res;
	}

	/** 解析信封并解包 biz_data */
	private async parseEnvelope<T>(res: Response): Promise<T> {
		const env = (await res.json()) as Envelope<T>;
		if (env.code !== 0) {
			throw new ApiError(env.code, env.msg || "unknown error");
		}
		if (!env.data) {
			throw new ApiError(-1, "响应缺少 data 字段");
		}
		if (env.data.biz_code !== 0) {
			throw new ApiError(env.data.biz_code, env.data.biz_msg || "unknown biz error");
		}
		return (env.data.biz_data ?? null) as T;
	}

	private async postJson<T>(path: string, options: RequestOptions): Promise<T> {
		const res = await this.raw(path, options);
		return this.parseEnvelope<T>(res);
	}

	/** 创建对话会话，返回 session id */
	async createSession(token: string): Promise<string> {
		const data = await this.postJson<{ chat_session: { id: string } }>(ENDPOINT_SESSION_CREATE, {
			token,
			body: {},
		});
		const id = data?.chat_session?.id;
		if (!id) throw new ApiError(-1, "创建会话失败：缺少 chat_session.id");
		return id;
	}

	/** 删除对话会话（清理临时会话，避免孤儿会话堆积） */
	async deleteSession(token: string, sessionId: string): Promise<void> {
		try {
			await this.postJson<unknown>(ENDPOINT_SESSION_DELETE, {
				token,
				body: { chat_session_id: sessionId },
			});
		} catch {
			// 删除失败不影响主流程，静默忽略
		}
	}

	/** 请求 PoW 挑战（服务端返回 snake_case，这里统一映射为内部 camelCase） */
	async createPowChallenge(token: string, targetPath: string): Promise<Challenge> {
		const data = await this.postJson<{ challenge: Record<string, unknown> }>(
			ENDPOINT_POW_CHALLENGE,
			{ token, body: { target_path: targetPath } },
		);
		const raw = data?.challenge;
		if (!raw) throw new ApiError(-1, "获取 PoW 挑战失败");
		const challenge: Challenge = {
			algorithm: String(raw.algorithm ?? ""),
			challenge: String(raw.challenge ?? ""),
			salt: String(raw.salt ?? ""),
			signature: String(raw.signature ?? ""),
			difficulty: Number(raw.difficulty ?? 0),
			expireAfter: Number(raw.expire_after ?? 0),
			// 关键：服务端字段是 expire_at，而 PoW 的 prefix 依赖它，
			// 取不到会退化成 "salt_undefined_"，导致 WASM 永远求不出解。
			expireAt: Number(raw.expire_at ?? 0),
			targetPath: String(raw.target_path ?? targetPath),
		};
		if (!challenge.challenge || !challenge.salt || !Number.isFinite(challenge.expireAt)) {
			throw new ApiError(-1, `PoW 挑战字段异常：${JSON.stringify(raw).slice(0, 200)}`);
		}
		return challenge;
	}

	/** 发起 completion，返回 SSE 响应体（流式） */
	async completion(
		token: string,
		powHeader: string,
		payload: CompletionPayload,
		signal?: AbortSignal,
	): Promise<Response> {
		await this.throttle();
		const init: Record<string, unknown> = {
			method: "POST",
			headers: this.buildHeaders({ token, powHeader, body: payload }),
			body: JSON.stringify(payload),
			signal,
		};
		if (this.dispatcher) init.dispatcher = this.dispatcher;

		const res = await fetch(this.url(ENDPOINT_COMPLETION), init as unknown as RequestInit);
		if (res.status === 202 && res.headers.get("x-amzn-waf-action")) throw new WafError();
		if (!res.ok) {
			const text = await res.text().catch(() => "");
			throw new HttpError(res.status, text.slice(0, 300));
		}
		return res;
	}

	/** 中断正在进行的流式输出（不需要 PoW） */
	async stopStream(token: string, sessionId: string, messageId: number): Promise<void> {
		try {
			await this.postJson<unknown>(ENDPOINT_STOP_STREAM, {
				token,
				body: { chat_session_id: sessionId, message_id: messageId },
			});
		} catch {
			// 中断失败不致命
		}
	}

	/** 编辑已有消息并重新生成（保留接口，供多轮复用会话时使用） */
	async editMessage(
		token: string,
		powHeader: string,
		payload: {
			chat_session_id: string;
			message_id: number;
			prompt: string;
			search_enabled: boolean;
			thinking_enabled: boolean;
		},
		signal?: AbortSignal,
	): Promise<Response> {
		await this.throttle();
		const init: Record<string, unknown> = {
			method: "POST",
			headers: this.buildHeaders({ token, powHeader, body: payload }),
			body: JSON.stringify(payload),
			signal,
		};
		if (this.dispatcher) init.dispatcher = this.dispatcher;
		const res = await fetch(this.url(ENDPOINT_EDIT_MESSAGE), init as unknown as RequestInit);
		if (!res.ok) {
			const text = await res.text().catch(() => "");
			throw new HttpError(res.status, text.slice(0, 300));
		}
		return res;
	}
}

/** PoW target_path 常量 */
export const POW_TARGET = {
	completion: "/api/v0/chat/completion",
	editMessage: "/api/v0/chat/edit_message",
} as const;
