// SSE 流解析 —— 把 DeepSeek 的 p/o/v patch 协议转换为结构化事件
//
// 该实现严格对齐 ds-free-api 的 ds_core/src/chat/response.rs：
//   - p / o 跨事件持久化；o 默认 SET；
//   - BATCH 递归分解，子路径前置父路径；
//   - 通过 fragments 的 type 区分 THINK 与 RESPONSE；
//   - status=FINISHED/INCOMPLETE 视为终止信号并产出 done 事件。
import { ApiError } from "./client.js";

/** 精简后的流事件协议 */
export type StreamEvent =
	| { type: "think_start" }
	| { type: "think_delta"; content: string }
	| { type: "content_start" }
	| { type: "content_delta"; content: string }
	| { type: "done"; finishReason: string | null; usage: number | null };

type Phase = "init" | "thinking" | "content" | "done";

const FRAG_THINK = "THINK";
const FRAG_RESPONSE = "RESPONSE";

interface Fragment {
	ty: string;
	content: string;
}

/** 上游 hint 事件抛出的错误 */
export class HintError extends Error {
	constructor(
		message: string,
		public readonly overloaded: boolean,
	) {
		super(message);
		this.name = "HintError";
	}
}

/** patch 状态机 */
class PatchState {
	private currentPath: string | undefined;
	private currentOp: string | undefined;
	private fragments: Fragment[] = [];
	private status: string | undefined;
	private usage: number | undefined;
	phase: Phase = "init";

	/** 消费一帧 SSE 文本，返回 0 个或多个事件 */
	applyFrame(frame: string): StreamEvent[] {
		if (!frame.trim()) return [];

		const lines = frame.split("\n");
		const eventType = lines
			.find((l) => l.trim().startsWith("event:"))
			?.trim()
			.slice("event:".length)
			.trim();
		const dataLine = lines
			.find((l) => l.trim().startsWith("data:"))
			?.trim()
			.slice("data:".length)
			.trim();

		if (eventType === "hint" && dataLine) {
			throw this.hintToError(dataLine);
		}
		if (!dataLine) return [];

		let value: unknown;
		try {
			value = JSON.parse(dataLine);
		} catch {
			return [];
		}

		const events = this.applyPatch(value as Record<string, unknown>);
		return this.finalize(events);
	}

	private hintToError(data: string): HintError {
		let content = "(unknown)";
		try {
			const val = JSON.parse(data) as Record<string, unknown>;
			const raw = val.content ?? val.finish_reason;
			if (typeof raw === "string") content = raw;
		} catch {
			// 保留默认值
		}
		if (content.includes("rate_limit")) {
			return new HintError("触发上游限流", true);
		}
		return new HintError(content, false);
	}

	private applyPatch(val: Record<string, unknown>): StreamEvent[] {
		if (typeof val.p === "string") this.currentPath = val.p;
		if (typeof val.o === "string") this.currentOp = val.o;

		const op = this.currentOp ?? "SET";
		const path = this.currentPath ?? "";

		if (!("v" in val)) return [];
		const v = val.v;

		// 初始快照：无路径且含 response
		if (this.currentPath === undefined && isRecord(v) && isRecord(v.response)) {
			return this.applyInitialSnapshot(v.response);
		}

		if (op === "BATCH" && Array.isArray(v)) {
			return this.applyBatch(path, v);
		}

		return this.applyPath(path, op, v);
	}

	private applyBatch(parentPath: string, arr: unknown[]): StreamEvent[] {
		const events: StreamEvent[] = [];
		let subPath = "";
		let subOp = "SET";

		for (const item of arr) {
			if (!isRecord(item)) continue;
			if (typeof item.p === "string") subPath = item.p;
			if (typeof item.o === "string") subOp = item.o;
			if (!("v" in item)) continue;
			const v = item.v;

			const fullPath = parentPath
				? subPath
					? `${parentPath}/${subPath}`
					: parentPath
				: subPath;

			if (subOp === "BATCH") {
				events.push(...this.applyBatch(fullPath, Array.isArray(v) ? v : []));
			} else {
				events.push(...this.applyPath(fullPath, subOp, v));
			}
		}
		return events;
	}

	private applyInitialSnapshot(response: Record<string, unknown>): StreamEvent[] {
		const events: StreamEvent[] = [];
		if (typeof response.status === "string") this.status = response.status;
		if (typeof response.accumulated_token_usage === "number") {
			this.usage = response.accumulated_token_usage;
		}
		const frags = response.fragments;
		if (Array.isArray(frags)) {
			this.fragments = [];
			for (const frag of frags) {
				if (!isRecord(frag) || typeof frag.type !== "string") continue;
				const content = typeof frag.content === "string" ? frag.content : "";
				this.fragments.push({ ty: frag.type, content });
				events.push(...this.deltaFor(frag.type, content));
			}
		}
		return events;
	}

	private applyPath(path: string, op: string, val: unknown): StreamEvent[] {
		const events: StreamEvent[] = [];
		const normalized = path.startsWith("/") ? path.slice(1) : path;

		if (normalized === "response/status") {
			if (typeof val === "string") this.status = val;
			return events;
		}
		if (normalized === "response/accumulated_token_usage") {
			if (typeof val === "number") this.usage = val;
			return events;
		}
		if (normalized === "response/fragments/-1/content") {
			if (typeof val === "string") {
				const last = this.fragments[this.fragments.length - 1];
				if (last) {
					last.content += val;
					events.push(...this.deltaFor(last.ty, val));
				}
			}
			return events;
		}
		if (normalized === "response/fragments" && op === "APPEND") {
			if (Array.isArray(val)) {
				for (const item of val) {
					if (!isRecord(item) || typeof item.type !== "string") continue;
					const content = typeof item.content === "string" ? item.content : "";
					this.fragments.push({ ty: item.type, content });
					events.push(...this.deltaFor(item.type, content));
				}
			}
		}
		return events;
	}

	private deltaFor(fragType: string, content: string): StreamEvent[] {
		if (!content) return [];
		if (fragType === FRAG_THINK) return [{ type: "think_delta", content }];
		if (fragType === FRAG_RESPONSE) return [{ type: "content_delta", content }];
		return [];
	}

	/** 插入阶段切换信号，并在状态终止时追加 done */
	private finalize(events: StreamEvent[]): StreamEvent[] {
		const out: StreamEvent[] = [];
		for (const evt of events) {
			if (evt.type === "think_delta" && (this.phase === "init" || this.phase === "content")) {
				this.phase = "thinking";
				out.push({ type: "think_start" });
			} else if (
				evt.type === "content_delta" &&
				(this.phase === "init" || this.phase === "thinking")
			) {
				this.phase = "content";
				out.push({ type: "content_start" });
			}
			out.push(evt);
		}

		if (
			(this.status === "FINISHED" || this.status === "INCOMPLETE") &&
			this.phase !== "done"
		) {
			this.phase = "done";
			out.push({
				type: "done",
				finishReason: this.status === "FINISHED" ? "stop" : null,
				usage: this.usage ?? null,
			});
		}
		return out;
	}

	/** 流结束时兜底：若已带终止状态则补发 done */
	flush(): StreamEvent[] {
		return this.finalize([]);
	}
}

function isRecord(v: unknown): v is Record<string, unknown> {
	return typeof v === "object" && v !== null;
}

/** SSE 解析器：喂入文本块，产出结构化事件 */
export class SseParser {
	private buffer = "";
	private state = new PatchState();
	private finished = false;

	/** 是否已收到终止状态 */
	get done(): boolean {
		return this.finished || this.state.phase === "done";
	}

	/** 推送一段文本，返回本次可产出的事件 */
	push(chunk: string): StreamEvent[] {
		this.buffer += chunk;
		const events: StreamEvent[] = [];
		let idx = this.buffer.indexOf("\n\n");
		while (idx !== -1) {
			const frame = this.buffer.slice(0, idx);
			this.buffer = this.buffer.slice(idx + 2);
			events.push(...this.state.applyFrame(frame));
			idx = this.buffer.indexOf("\n\n");
		}
		if (this.state.phase === "done") this.finished = true;
		return events;
	}

	/** 流结束，冲刷缓冲区 */
	flush(): StreamEvent[] {
		const events: StreamEvent[] = [];
		if (this.buffer.trim()) {
			events.push(...this.state.applyFrame(this.buffer));
			this.buffer = "";
		}
		events.push(...this.state.flush());
		if (this.state.phase === "done") this.finished = true;
		return events;
	}
}

/** 从 ready 事件中解析 response_message_id（用于后续 stop_stream） */
export function parseReadyMessageId(frame: string): number | undefined {
	for (const line of frame.split("\n")) {
		const trimmed = line.trim();
		if (!trimmed.startsWith("data:")) continue;
		try {
			const val = JSON.parse(trimmed.slice("data:".length).trim()) as Record<string, unknown>;
			if (typeof val.response_message_id === "number") return val.response_message_id;
		} catch {
			// 忽略非 JSON 行
		}
	}
	return undefined;
}

/** 判断错误是否可重试（限流 / 瞬时故障） */
export function isRetryableError(error: unknown): boolean {
	if (error instanceof HintError) return error.overloaded;
	if (error instanceof ApiError) return error.code === 1001 || error.code === 1201;
	return false;
}
