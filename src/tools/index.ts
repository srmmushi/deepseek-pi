// 工具注册与分发
import type { Lang } from "../i18n/index.js";
import { runExec } from "./exec.js";
import { runList, runRead, runSearch, runWrite } from "./fs-tools.js";
import type { ToolCall, ToolContext, ToolName, ToolResult } from "./types.js";

export type { ToolCall, ToolContext, ToolName, ToolResult } from "./types.js";
export { describeCall, parseToolCalls } from "./parser.js";

/** 工具名集合 */
export const TOOL_NAMES: ToolName[] = ["write", "read", "list", "exec", "search"];

/** 执行一个工具调用 */
export async function executeTool(call: ToolCall, ctx: ToolContext): Promise<ToolResult> {
	switch (call.name) {
		case "write":
			return runWrite(call, ctx);
		case "read":
			return runRead(call, ctx);
		case "list":
			return runList(call, ctx);
		case "exec":
			return runExec(call, ctx);
		case "search":
			return runSearch(call, ctx);
	}
}

/** 每个工具的一句话说明（用于帮助信息） */
const TOOL_SUMMARY: Record<ToolName, Record<Lang, string>> = {
	write: { zh: "写入文件（一次性完整覆盖）", en: "Write a file (full overwrite)" },
	read: { zh: "读取文件", en: "Read a file" },
	list: { zh: "列出目录", en: "List a directory" },
	exec: { zh: "执行命令", en: "Run a shell command" },
	search: { zh: "搜索文件内容", en: "Search file contents" },
};

/**
 * 生成注入到系统提示词中的工具调用说明。
 * 要求「精简」：只描述格式与硬性规则，不做冗长解释。
 */
export function buildToolDoc(lang: Lang): string {
	if (lang === "en") {
		return [
			"You can call the following tools. Use EXACTLY this format:",
			"",
			'Write a file (a full overwrite, not append): write:"file content",path',
			'Example: write:"console.log(\'hello\')",src/index.js',
			"",
			"Read a file: read:path",
			"Example: read:src/index.js",
			"",
			"List a directory: list:path",
			"Example: list:src",
			"",
			"Run a command: exec:command",
			"Example: exec:npm install",
			"",
			"Search files: search:keyword",
			"Example: search:useState",
			"",
			"Rules:",
			"- Call only one tool at a time.",
			"- A tool call must occupy its own line.",
			"- Write is a full one-shot write, never an append.",
			"- Paths may be relative to the current working directory or absolute.",
			"- Tool results come back as a message starting with `[tool result]`.",
		].join("\n");
	}

	return [
		"你可以调用以下工具来完成任务。调用时严格使用如下格式：",
		"",
		"写入文件（一次完整的写入操作）：",
		'write:"文件内容",文件路径',
		"示例：write:\"console.log('hello')\",src/index.js",
		"",
		"读取文件：",
		"read:文件路径",
		"示例：read:src/index.js",
		"",
		"列出目录：",
		"list:目录路径",
		"示例：list:src",
		"",
		"执行命令：",
		"exec:命令",
		"示例：exec:npm install",
		"",
		"搜索文件：",
		"search:关键词",
		"示例：search:useState",
		"",
		"注意：",
		"- 每次只调用一个工具。",
		"- 工具调用必须独占一行。",
		"- 写入操作是一次性完整写入，不是追加。",
		"- 路径可以是相对路径（相对于当前工作目录）或绝对路径。",
		"- 工具执行结果会以 `[工具结果]` 开头的消息返回。",
	].join("\n");
}

/** 复用会话模式下，工具结果的回灌前缀（需与工具说明保持一致） */
export function toolResultPrefix(lang: Lang): string {
	return lang === "zh" ? "[工具结果]" : "[tool result]";
}

/** 列出工具说明（供 /help 展示） */
export function toolSummaries(lang: Lang): Array<{ name: ToolName; text: string }> {
	return TOOL_NAMES.map((name) => ({ name, text: TOOL_SUMMARY[name][lang] }));
}
