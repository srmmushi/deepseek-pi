// exec 工具 —— 在当前工作目录执行 shell 命令并回收输出
//
// 安全提示：该工具会以当前用户身份执行任意命令，仅应在可信目录中使用。
import { exec } from "node:child_process";
import type { ToolContext, ToolResult } from "./types.js";

/** 命令超时（毫秒） */
const EXEC_TIMEOUT_MS = 120_000;
/** 输出上限（字符），超出部分截断 */
const MAX_OUTPUT_CHARS = 40_000;

/** 执行命令 */
export function runExec(args: { command: string }, ctx: ToolContext): Promise<ToolResult> {
	return new Promise((resolvePromise) => {
		exec(
			args.command,
			{
				cwd: ctx.cwd,
				timeout: EXEC_TIMEOUT_MS,
				maxBuffer: 16 * 1024 * 1024,
				windowsHide: true,
				shell: process.platform === "win32" ? undefined : "/bin/sh",
			},
			(error, stdout, stderr) => {
				const code = error ? ((error as { code?: number }).code ?? 1) : 0;
				let combined = `${stdout}${stderr}`.trim();
				let truncated = false;
				if (combined.length > MAX_OUTPUT_CHARS) {
					combined = combined.slice(0, MAX_OUTPUT_CHARS);
					truncated = true;
				}
				const tail = truncated
					? `\n${ctx.i18n.t("tool.maxOutput", { limit: MAX_OUTPUT_CHARS })}`
					: "";
				const header = `[exec] ${args.command}\n${ctx.i18n.t("tool.execDone", { code })}\n`;
				const body = combined || "(无输出)";
				return resolvePromise({
					ok: code === 0,
					output: `${header}${body}${tail}`,
					summary: `exit ${code}`,
				});
			},
		);
	});
}
