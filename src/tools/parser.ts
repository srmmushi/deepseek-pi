// 文本工具调用解析器
//
// 约定格式（每行一个调用，独占一行，一次性完整写入）：
//   write:"文件内容",文件路径
//   read:文件路径
//   list:目录路径
//   exec:命令
//   search:关键词
//
// 解析器对模型输出保持宽容：允许全角冒号、允许参数被引号包裹、
// 允许 write 的内容使用 \n \t \" \\ 转义。解析失败的行会记录为错误，交由 Agent 回灌给模型。
import type { ToolCall } from "./types.js";

export interface ParseResult {
	calls: ToolCall[];
	/** 形如 "第 3 行：write 缺少路径" 的错误描述 */
	errors: string[];
}

const LINE_PATTERN = /^(write|read|list|exec|search)\s*[:：]\s*([\s\S]+)$/i;

/** 去掉成对引号并还原常见转义 */
function unquote(input: string): string {
	const s = input.trim();
	if (s.length >= 2) {
		const first = s[0];
		const last = s[s.length - 1];
		if ((first === '"' && last === '"') || (first === "'" && last === "'")) {
			const inner = s.slice(1, -1);
			return inner.replace(/\\(.)/g, (_, ch: string) => {
				if (ch === "n") return "\n";
				if (ch === "t") return "\t";
				if (ch === "r") return "\r";
				return ch;
			});
		}
	}
	return s;
}

/**
 * 解析 write 的参数：`"内容",路径`。
 * 兼容两种写法：
 *   1. write:"内容",路径   —— 推荐（内容可含逗号）
 *   2. write:内容,路径     —— 内容中不能含逗号
 */
function parseWriteArgs(rest: string): { content: string; path: string } {
	const text = rest;
	if (text[0] === '"' || text[0] === "'") {
		const quote = text[0];
		let i = 1;
		let content = "";
		let closed = false;
		while (i < text.length) {
			const ch = text[i];
			if (ch === "\\" && i + 1 < text.length) {
				const next = text[i + 1];
				content += next === "n" ? "\n" : next === "t" ? "\t" : next === "r" ? "\r" : next;
				i += 2;
				continue;
			}
			if (ch === quote) {
				closed = true;
				i += 1;
				break;
			}
			content += ch;
			i += 1;
		}
		if (!closed) throw new Error("write 内容缺少结束引号");
		// 跳过逗号与空白
		let j = i;
		while (j < text.length && (text[j] === "," || text[j] === " " || text[j] === "\t")) j += 1;
		const path = unquote(text.slice(j));
		if (!path) throw new Error("write 缺少文件路径");
		return { content, path };
	}

	const idx = text.indexOf(",");
	if (idx === -1) throw new Error("write 格式应为 write:\"内容\",路径");
	return { content: text.slice(0, idx).trim(), path: unquote(text.slice(idx + 1)) };
}

function buildCall(name: string, rest: string): ToolCall {
	switch (name) {
		case "write": {
			const { content, path } = parseWriteArgs(rest);
			if (!path) throw new Error("write 缺少文件路径");
			return { name: "write", content, path };
		}
		case "read": {
			const path = unquote(rest);
			if (!path) throw new Error("read 缺少文件路径");
			return { name: "read", path };
		}
		case "list": {
			const path = unquote(rest) || ".";
			return { name: "list", path };
		}
		case "exec": {
			const command = unquote(rest);
			if (!command) throw new Error("exec 缺少命令");
			return { name: "exec", command };
		}
		case "search": {
			const query = unquote(rest);
			if (!query) throw new Error("search 缺少关键词");
			return { name: "search", query };
		}
		default:
			throw new Error(`未知工具：${name}`);
	}
}

/**
 * 从 assistant 文本中解析全部工具调用。
 * 只识别「单独成行」的调用，忽略代码围栏行。
 */
export function parseToolCalls(text: string): ParseResult {
	const calls: ToolCall[] = [];
	const errors: string[] = [];
	const lines = text.split(/\r?\n/);

	lines.forEach((rawLine, index) => {
		const line = rawLine.trim();
		if (!line || line.startsWith("```")) return;
		const match = LINE_PATTERN.exec(line);
		if (!match) return;
		const name = match[1].toLowerCase();
		try {
			calls.push(buildCall(name, match[2]));
		} catch (e) {
			errors.push(`第 ${index + 1} 行：[${name}] ${(e as Error).message}`);
		}
	});

	return { calls, errors };
}

/** 生成工具调用的参数摘要（用于界面展示） */
export function describeCall(call: ToolCall): string {
	switch (call.name) {
		case "write":
			return `${call.path}（${Buffer.byteLength(call.content, "utf8")} 字节）`;
		case "read":
			return call.path;
		case "list":
			return call.path;
		case "exec":
			return call.command;
		case "search":
			return call.query;
	}
}
