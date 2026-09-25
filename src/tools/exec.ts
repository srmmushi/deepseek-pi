// exec 工具 —— 在当前工作目录执行 shell 命令并回收输出
//
// 安全提示：该工具会以当前用户身份执行任意命令，仅应在可信目录中使用。
import { exec } from "node:child_process";
import type { ToolContext, ToolResult } from "./types.js";

/** 命令超时（毫秒） */
const EXEC_TIMEOUT_MS = 120_000;
/** 输出上限（字符），超出部分截断 */
const MAX_OUTPUT_CHARS = 40_000;

/**
 * 解码子进程输出。
 * 子进程（尤其 Windows 的 cmd）常按本地代码页输出，中文环境是 GBK，
 * 直接按 UTF-8 解码会变成乱码。这里先按 UTF-8 解，出现替换字符时回退 GBK。
 */
function decodeOutput(buffer: Buffer): string {
	const utf8 = buffer.toString("utf8");
	if (process.platform !== "win32" || !utf8.includes("\uFFFD")) return utf8;
	try {
		return new TextDecoder("gbk").decode(buffer);
	} catch {
		// 运行时未内置 gbk 编码表时保持 UTF-8 结果
		return utf8;
	}
}

/** 原始 shell 执行结果（`!命令` 交互模式复用，不做任何包装） */
export interface ShellResult {
	ok: boolean;
	/** 退出码 */
	code: number;
	/** stdout + stderr（已 trim，超限时截断） */
	output: string;
	truncated: boolean;
	durationMs: number;
}

/** 在指定目录执行 shell 命令，返回原始结果 */
export function runShell(command: string, cwd: string): Promise<ShellResult> {
	const startedAt = Date.now();
	return new Promise<ShellResult>((resolvePromise) => {
		exec(
			command,
			{
				cwd,
				timeout: EXEC_TIMEOUT_MS,
				maxBuffer: 16 * 1024 * 1024,
				windowsHide: true,
				// 用 latin1 逐字节保留原始输出，稍后自行按正确编码解码
				encoding: "latin1",
				shell: process.platform === "win32" ? undefined : "/bin/sh",
			},
			(error, stdout, stderr) => {
				const code = error ? ((error as { code?: number }).code ?? 1) : 0;
				const out = decodeOutput(Buffer.from(stdout, "latin1"));
				const err = decodeOutput(Buffer.from(stderr, "latin1"));
				let combined = `${out}${err}`.trim();
				let truncated = false;
				if (combined.length > MAX_OUTPUT_CHARS) {
					combined = combined.slice(0, MAX_OUTPUT_CHARS);
					truncated = true;
				}
				resolvePromise({
					ok: code === 0,
					code,
					output: combined,
					truncated,
					durationMs: Date.now() - startedAt,
				});
			},
		);
	});
}

/** 执行命令（供 Agent 调用：输出带 [exec] 头部，便于模型理解） */
export async function runExec(args: { command: string }, ctx: ToolContext): Promise<ToolResult> {
	const result = await runShell(args.command, ctx.cwd);
	const tail = result.truncated
		? `\n${ctx.i18n.t("tool.maxOutput", { limit: MAX_OUTPUT_CHARS })}`
		: "";
	const header = `[exec] ${args.command}\n${ctx.i18n.t("tool.execDone", { code: result.code })}\n`;
	const body = result.output || "(无输出)";
	return {
		ok: result.ok,
		output: `${header}${body}${tail}`,
		summary: `exit ${result.code}`,
	};
}
