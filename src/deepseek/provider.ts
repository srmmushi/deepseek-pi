// DeepSeek 网页版对话编排 —— 一次 completion 的完整生命周期
//
// 两种会话策略：
//   A. 复用（handle 传入）：pi 会话与网页会话 1:1，只发增量，上下文由服务端维护。
//   B. 一次性（不传 handle）：每轮新建会话、用完即删，prompt 自带完整历史（上游 API 风格）。
import type { DeepSeekClient, CompletionPayload } from "./client.js";
import { POW_TARGET } from "./client.js";
import { encodePowHeader, type PowSolver } from "./pow.js";
import { ApiError, WafError } from "./client.js";
import { HintError, SseParser, isRetryableError, type StreamEvent } from "./stream.js";

/**
 * 网页会话句柄：调用方传入即表示「复用该会话」，
 * provider 会在会话尚未创建时创建它，并把 message id 回写。
 */
export interface WebSessionHandle {
	/** DeepSeek 网页会话 id；null 表示需要新建（懒创建） */
	sessionId: string | null;
	/** 下一轮 completion 的 parent_message_id */
	parentMessageId: number | null;
}

export interface StreamChatParams {
	client: DeepSeekClient;
	solver: PowSolver;
	token: string;
	/** 本轮要发送的 prompt（复用模式下为增量内容） */
	prompt: string;
	/** 模型类型：default | expert（当前仅 default 可用） */
	modelType: string;
	thinkingEnabled: boolean;
	searchEnabled: boolean;
	signal?: AbortSignal;
	/** 复用会话句柄；不传则使用一次性会话 */
	handle?: WebSessionHandle;
}

/** 从原始文本中提取 response_message_id（用于 stop_stream 与链式 parent） */
function extractMessageId(raw: string): number | undefined {
	const m = /response_message_id"?\s*:\s*(\d+)/.exec(raw);
	return m ? Number(m[1]) : undefined;
}

/** 尝试把非 SSE 的 JSON 错误响应解析为异常 */
function parseStreamError(raw: string): Error {
	const text = raw.trim();
	try {
		const val = JSON.parse(text) as Record<string, unknown>;
		const code = typeof val.code === "number" ? val.code : undefined;
		const data = val.data as Record<string, unknown> | null | undefined;
		if (data && typeof data.biz_code === "number" && data.biz_code !== 0) {
			return new ApiError(data.biz_code, String(data.biz_msg ?? "unknown biz error"));
		}
		if (code !== undefined && code !== 0) {
			return new ApiError(code, String(val.msg ?? "unknown error"));
		}
	} catch {
		// 不是 JSON
	}
	return new Error(`无法解析的响应：${text.slice(0, 200)}`);
}

/**
 * 单次流式对话（不带重试）。
 * 逐块产出 StreamEvent，保证 UI 可以边收边渲染。
 */
export async function* streamChat(params: StreamChatParams): AsyncGenerator<StreamEvent> {
	const { client, solver, token, handle } = params;

	// 复用模式下由句柄承载会话；否则本次调用自己创建并负责销毁
	const ownedSession = handle === undefined;
	const alreadyBound = handle?.sessionId != null;
	let sessionId = handle?.sessionId ?? (await client.createSession(token));
	if (handle) handle.sessionId = sessionId;

	let messageId: number | undefined;
	let finished = false;
	let sawEvent = false;
	let rawHead = "";
	const decoder = new TextDecoder();

	try {
		const challenge = await client.createPowChallenge(token, POW_TARGET.completion);
		const powHeader = encodePowHeader(solver.solveChallenge(challenge));

		const payload: CompletionPayload = {
			chat_session_id: sessionId,
			// 已存在的会话：链到上一条消息；新建会话：null
			parent_message_id: alreadyBound ? (handle?.parentMessageId ?? null) : null,
			model_type: params.modelType,
			prompt: params.prompt,
			ref_file_ids: [],
			thinking_enabled: params.thinkingEnabled,
			search_enabled: params.searchEnabled,
			preempt: false,
		};

		const res = await client.completion(token, powHeader, payload, params.signal);
		const body = res.body as unknown as AsyncIterable<Uint8Array> | null;
		if (!body) throw new Error("响应没有可读的 SSE 流");

		const parser = new SseParser();

		for await (const chunk of body) {
			const text = decoder.decode(chunk, { stream: true });
			if (messageId === undefined && rawHead.length < 4096) {
				rawHead += text;
				const id = extractMessageId(rawHead);
				if (id !== undefined) messageId = id;
			}
			const events = parser.push(text);
			if (events.length > 0) sawEvent = true;
			for (const evt of events) yield evt;
			if (parser.done) {
				finished = true;
				break;
			}
		}

		if (!finished) {
			const tail = decoder.decode();
			if (tail) {
				const events = parser.push(tail);
				if (events.length > 0) sawEvent = true;
				for (const evt of events) yield evt;
			}
			for (const evt of parser.flush()) {
				if (evt.type === "done") finished = true;
				sawEvent = true;
				yield evt;
			}
		}

		// 整段流没有产出任何事件：多半是被封装成 JSON 的业务错误
		if (!sawEvent) {
			throw parseStreamError(rawHead);
		}
	} finally {
		// 把本轮 response_message_id 回写，供下一轮链式追加
		if (handle && messageId !== undefined) {
			handle.parentMessageId = messageId;
		}
		if (!finished && messageId !== undefined) {
			await client.stopStream(token, sessionId, messageId);
		}
		if (ownedSession) {
			// 一次性会话：用完即删
			await client.deleteSession(token, sessionId);
		} else if (handle && !alreadyBound && messageId === undefined) {
			// 本轮由我们新建，但完全没跑起来（PoW 失败 / 网络异常 / 立刻被中断）：
			// 删掉它并解绑，避免网页端不断堆积「空会话」。
			await client.deleteSession(token, sessionId);
			handle.sessionId = null;
		}
		// 复用模式成功时保留会话，保证 pi 会话与网页会话 1:1
	}
}

/**
 * 带退避重试的流式对话。
 * 仅在「尚未产出任何事件」时重试，避免重复输出。
 */
export async function* streamChatWithRetry(
	params: StreamChatParams,
	maxAttempts = 3,
): AsyncGenerator<StreamEvent> {
	let attempt = 0;
	// eslint-disable-next-line no-constant-condition
	while (true) {
		attempt += 1;
		let yielded = false;
		try {
			for await (const evt of streamChat(params)) {
				yielded = true;
				yield evt;
			}
			return;
		} catch (error) {
			if (error instanceof WafError) throw error;
			if (yielded || !isRetryableError(error) || attempt >= maxAttempts) throw error;
			const backoff = 1000 * 2 ** (attempt - 1);
			await new Promise((r) => setTimeout(r, backoff));
		}
	}
}

/** 判断错误是否为「凭证失效」 */
export function isAuthError(error: unknown): boolean {
	return error instanceof ApiError && error.code === 40003;
}

/** 判断是否为限流错误 */
export function isRateLimitError(error: unknown): boolean {
	if (error instanceof HintError) return error.overloaded;
	return error instanceof ApiError && (error.code === 1001 || error.code === 1201);
}
