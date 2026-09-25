// Agent 主循环
//
// 单轮 turn 的流程：
//   构建内容 → 流式请求 DeepSeek → 解析文本工具调用 → 执行工具 → 回灌结果 → 继续
// 直到模型不再调用工具，或达到最大轮数。
//
// 上下文策略由 config.contextMode 决定：
//   reuse  —— 复用当前会话绑定的网页会话，只发增量（系统提示词仅在会话首条消息下发一次）
//   replay —— 每轮把完整历史打包成一个一次性 prompt（一次性会话，用完即删）
import type { App } from "../app.js";
import { buildPrompt } from "../deepseek/prompt.js";
import {
	streamChatWithRetry,
	isAuthError,
	isRateLimitError,
	type WebSessionHandle,
} from "../deepseek/provider.js";
import { ApiError, HttpError, WafError } from "../deepseek/client.js";
import { HintError } from "../deepseek/stream.js";
import {
	describeCall,
	executeTool,
	parseToolCalls,
	toolResultPrefix,
	type ToolCall,
	type ToolResult,
} from "../tools/index.js";

/** 单轮交互的输出回调（由 REPL 实现） */
export interface TurnIO {
	onThinkStart(): void;
	onThinkDelta(text: string): void;
	/** 思考结束；fullText 为本次思考全文，供折叠后按需展开回放 */
	onThinkEnd(fullText: string, elapsedMs: number): void;
	onContentStart(): void;
	onContentDelta(text: string): void;
	/** 一批工具即将并行执行（calls.length >= 1） */
	onToolBatchStart(calls: ToolCall[]): void;
	/** 单个工具执行结束；按完成顺序回调，index 为它在批次中的原始序号 */
	onToolEnd(call: ToolCall, result: ToolResult, elapsedMs: number, index: number): void;
	onDone(finishReason: string | null, usage: number | null): void;
	onNotice(text: string): void;
}

/** 把底层异常翻译为用户可读的提示 */
export function describeError(app: App, error: unknown): string {
	const { t } = app.i18n;
	if (error instanceof WafError) return t("error.waf");
	if (error instanceof HintError) return t("error.hint", { msg: error.message });
	if (isAuthError(error)) return t("error.sessionExpired", { code: (error as ApiError).code });
	if (isRateLimitError(error)) return t("error.rateLimit");
	if (error instanceof ApiError) return t("error.api", { error: `${error.code}: ${error.message}` });
	if (error instanceof HttpError) return t("error.api", { error: `HTTP ${error.status}` });

	const message = error instanceof Error ? error.message : String(error);
	if (/WASM|wasm_solve|PoW|pow/i.test(message)) return t("error.pow", { error: message });
	if (/proxy|ECONN|ENOTFOUND|fetch failed|socket/i.test(message)) {
		return t("error.http", { error: message });
	}
	return t("error.apiChanged", { detail: message });
}

/**
 * 执行一轮对话（可能包含多轮工具调用）。
 * @param signal 用于中断生成（Ctrl+C）
 */
export async function runTurn(
	app: App,
	userInput: string,
	io: TurnIO,
	signal: AbortSignal,
): Promise<void> {
	const session = app.activeSession;
	// 标题取首条用户输入
	app.maybeSetTitle(userInput);
	session.messages.push({ role: "user", content: userInput });
	app.saveSession();

	const toolCtx = { cwd: app.cwd, i18n: app.i18n };
	const maxSteps = Math.max(1, app.config.maxToolSteps);
	const resultPrefix = toolResultPrefix(app.i18n.lang);
	const reuseSession = app.config.contextMode === "reuse";

	/** 本轮要发送给网页端的内容（复用模式下即为「增量」） */
	let outgoing = userInput;
	let steps = 0;
	/** 累计 token 用量与结束原因（所有轮次结束后统一上报） */
	let totalUsage = 0;
	let sawUsage = false;
	let lastFinishReason: string | null = null;

	for (;;) {
		if (signal.aborted) throw new Error("aborted");

		const client = await app.getClient();
		const solver = await app.getSolver();
		const token = app.getToken();
		if (!token) throw new ApiError(40003, app.i18n.t("error.noToken"));

		let prompt: string;
		let handle: WebSessionHandle | undefined;

		if (reuseSession) {
			// 网页会话尚未成功完成过一轮时，把系统提示词 + 工具说明随本轮内容一起下发；
			// 之后依赖服务端上下文，不再重复发送。
			const needsSystem = session.parentMessageId === null;
			prompt = needsSystem ? `${app.buildSystemText()}\n\n${outgoing}` : outgoing;
			handle = {
				sessionId: session.deepseekSessionId,
				parentMessageId: session.parentMessageId,
			};
		} else {
			// 重放模式：每轮用一次性会话发送完整历史
			prompt = buildPrompt(session.messages, app.buildSystemText());
		}

		let assistantText = "";
		let pendingLine = "";
		let thinkOpen = false;
		let contentStarted = false;
		/** 本次思考的全文与起始时间（用于折叠摘要） */
		let thinkText = "";
		let thinkStartedAt = Date.now();

		/** 结束思考段（内容开始或本轮结束时都会调用） */
		const closeThink = (): void => {
			if (!thinkOpen) return;
			thinkOpen = false;
			io.onThinkEnd(thinkText, Date.now() - thinkStartedAt);
		};

		/** 输出一行正文；工具调用行由界面用 ⏺ 单独渲染，这里吞掉避免重复 */
		const emitLine = (line: string): void => {
			if (parseToolCalls(line).calls.length > 0) return;
			if (!contentStarted) {
				contentStarted = true;
				io.onContentStart();
			}
			io.onContentDelta(`${line}\n`);
		};

		const stream = streamChatWithRetry({
			client,
			solver,
			token,
			prompt,
			modelType: "default",
			thinkingEnabled: app.config.thinking,
			searchEnabled: app.config.search,
			signal,
			handle,
		});

		try {
			for await (const evt of stream) {
				switch (evt.type) {
					case "think_start":
						thinkOpen = true;
						thinkText = "";
						thinkStartedAt = Date.now();
						io.onThinkStart();
						break;
					case "think_delta":
						thinkText += evt.content;
						io.onThinkDelta(evt.content);
						break;
					case "content_start":
						closeThink();
						break;
					case "content_delta": {
						assistantText += evt.content;
						pendingLine += evt.content;
						// 按整行输出，便于识别并隐藏工具调用行
						let index = pendingLine.indexOf("\n");
						while (index !== -1) {
							emitLine(pendingLine.slice(0, index));
							pendingLine = pendingLine.slice(index + 1);
							index = pendingLine.indexOf("\n");
						}
						break;
					}
					case "done":
						if (pendingLine) {
							emitLine(pendingLine);
							pendingLine = "";
						}
						closeThink();
						lastFinishReason = evt.finishReason;
						if (evt.usage != null) {
							totalUsage += evt.usage;
							sawUsage = true;
						}
						break;
				}
			}
		} finally {
			// 无论成功、失败还是被中断，都必须把网页会话绑定落盘。
			// 否则下一轮会误以为「还没建会话」而重复创建，
			// 表现为「同一个 pi 会话在网页端出现多个会话」。
			if (handle) {
				session.deepseekSessionId = handle.sessionId;
				session.parentMessageId = handle.parentMessageId;
				app.saveSession();
			}
		}

		session.messages.push({ role: "assistant", content: assistantText });
		app.saveSession();

		const parsed = parseToolCalls(assistantText);

		// 解析出错：把错误回灌给模型，让它修正格式
		if (parsed.calls.length === 0 && parsed.errors.length > 0) {
			const errorText = `${resultPrefix}\n[tool-call parse error]\n${parsed.errors.join("\n")}`;
			session.messages.push({ role: "tool", content: errorText });
			app.saveSession();
			outgoing = errorText;
			io.onNotice(app.i18n.t("tool.fail", { name: "parse", error: parsed.errors.join("; ") }));
			steps += 1;
			if (steps >= maxSteps) break;
			continue;
		}

		// 没有工具调用 → 本轮结束
		if (parsed.calls.length === 0) break;

		// 并行执行本批工具：互不依赖的调用同时跑，总耗时取决于最慢的那个。
		// 回调顺序 = 完成顺序（体现真实并发）；回灌顺序 = 调用顺序（与模型的意图对齐）。
		io.onToolBatchStart(parsed.calls);
		const results = await Promise.all(
			parsed.calls.map(async (call, index) => {
				const startedAt = Date.now();
				let result: ToolResult;
				try {
					result = await executeTool(call, toolCtx);
				} catch (e) {
					result = {
						ok: false,
						output: app.i18n.t("tool.crashed", { error: (e as Error).message }),
						summary: describeCall(call),
					};
				}
				io.onToolEnd(call, result, Date.now() - startedAt, index);
				return result;
			}),
		);

		const resultBlocks: string[] = [];
		for (const result of results) {
			const body = result.ok ? result.output : `[error] ${result.output}`;
			resultBlocks.push(`${resultPrefix}\n${body}`);
			session.messages.push({ role: "tool", content: body });
		}
		app.saveSession();

		// 下一轮把工具结果作为增量发回
		outgoing = resultBlocks.join("\n\n");

		steps += 1;
		if (steps >= maxSteps) {
			io.onNotice(app.i18n.t("repl.maxSteps", { max: maxSteps }));
			break;
		}
	}

	// 全部轮次结束：统一上报累计用量
	io.onDone(lastFinishReason, sawUsage ? totalUsage : null);
}
