// 系统提示词加载 / 编辑 / 重置
//
// 系统提示词存放在配置目录下的 system-prompt.md，用户可直接编辑；
// 工具调用说明（buildToolDoc）不写入该文件，而是在请求时追加，避免被误删。
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import type { ConfigPaths } from "../config/dirs.js";
import type { Lang } from "../i18n/index.js";

/** 内置默认系统提示词（按语言） */
export const DEFAULT_SYSTEM_PROMPT: Record<Lang, string> = {
	zh: [
		"你是 Pi Agent，一个运行在终端中的编程助手，只通过 DeepSeek 网页版进行推理。",
		"",
		"工作方式：",
		"- 先理解目标，再决定是否需要调用工具；能直接回答就直接回答。",
		"- 需要读写文件、查看目录或执行命令时，严格使用约定的工具调用格式。",
		"- 调用工具后，根据返回结果继续推进，直到任务完成。",
		"- 回答保持简洁、准确，避免与任务无关的长篇解释。",
		"- 涉及覆盖、删除等破坏性操作前，先用一句话说明你的意图。",
	].join("\n"),
	en: [
		"You are Pi Agent, a terminal coding assistant that reasons only through DeepSeek Web.",
		"",
		"How you work:",
		"- Understand the goal first, then decide whether a tool call is needed; answer directly when possible.",
		"- Use the exact tool-call format when you need to read/write files, list directories, or run commands.",
		"- After a tool call, continue from its result until the task is done.",
		"- Keep answers concise and accurate; avoid long unrelated explanations.",
		"- Before destructive actions (overwrite, delete), state your intent in one sentence.",
	].join("\n"),
};

/** 若文件不存在则写入默认系统提示词 */
export function ensureSystemPromptFile(paths: ConfigPaths, lang: Lang): void {
	if (existsSync(paths.systemPromptFile)) return;
	const dir = dirname(paths.systemPromptFile);
	if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
	writeFileSync(paths.systemPromptFile, `${DEFAULT_SYSTEM_PROMPT[lang]}\n`, "utf8");
}

/** 读取系统提示词；文件缺失时回退到内置默认值 */
export function loadSystemPrompt(paths: ConfigPaths, lang: Lang): string {
	try {
		if (!existsSync(paths.systemPromptFile)) return DEFAULT_SYSTEM_PROMPT[lang];
		const text = readFileSync(paths.systemPromptFile, "utf8").trim();
		return text || DEFAULT_SYSTEM_PROMPT[lang];
	} catch {
		return DEFAULT_SYSTEM_PROMPT[lang];
	}
}

/** 恢复默认系统提示词 */
export function resetSystemPrompt(paths: ConfigPaths, lang: Lang): void {
	const dir = dirname(paths.systemPromptFile);
	if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
	writeFileSync(paths.systemPromptFile, `${DEFAULT_SYSTEM_PROMPT[lang]}\n`, "utf8");
}

/** 选择默认编辑器 */
function pickEditor(): string {
	if (process.env.VISUAL) return process.env.VISUAL;
	if (process.env.EDITOR) return process.env.EDITOR;
	return process.platform === "win32" ? "notepad" : "nano";
}

/**
 * 用默认编辑器打开系统提示词文件。
 * 编辑器不存在或启动失败时返回 false，由调用方提示用户手动编辑。
 */
export function openSystemPromptInEditor(paths: ConfigPaths): Promise<boolean> {
	return new Promise((resolvePromise) => {
		const editor = pickEditor();
		let settled = false;
		const finish = (ok: boolean): void => {
			if (settled) return;
			settled = true;
			resolvePromise(ok);
		};
		try {
			const child = spawn(editor, [paths.systemPromptFile], { stdio: "inherit" });
			child.on("error", () => finish(false));
			child.on("close", () => finish(true));
		} catch {
			finish(false);
		}
	});
}
