// 工具公共类型定义
import type { I18n } from "../i18n/index.js";

/** 支持的五个核心工具 */
export type ToolName = "write" | "read" | "list" | "exec" | "search";

/** 解析后的工具调用（判别联合） */
export type ToolCall =
	| { name: "write"; content: string; path: string }
	| { name: "read"; path: string }
	| { name: "list"; path: string }
	| { name: "exec"; command: string }
	| { name: "search"; query: string };

/** 工具执行上下文 */
export interface ToolContext {
	/** 工作目录：相对路径均以此为基准 */
	cwd: string;
	i18n: I18n;
}

/** 工具执行结果 */
export interface ToolResult {
	/** 是否成功（用于 UI 着色与模型判断） */
	ok: boolean;
	/** 回传给模型的文本 */
	output: string;
	/** 展示给用户的一行摘要 */
	summary: string;
}
