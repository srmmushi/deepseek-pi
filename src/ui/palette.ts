// 命令面板 / 输入提示的渲染
import { COMMAND_SPECS, describeOf, usageOf, type CommandSpec } from "../agent/command-specs.js";
import type { Lang } from "../i18n/index.js";
import { color, info } from "./output.js";
import { padTo } from "./text.js";

/** 命令名一列的宽度 */
const NAME_COLUMN = 32;

/** 打印完整命令面板（输入 `/` 时调用，写入主输出区） */
export function printCommandPalette(lang: Lang): void {
	info();
	info(`  ${color.bold(lang === "zh" ? "可用命令" : "Available commands")}`);
	for (const spec of COMMAND_SPECS) {
		info(`  ${color.cyan(padTo(usageOf(spec), NAME_COLUMN))}${color.dim(describeOf(spec, lang))}`);
	}
	info();
	// 非斜杠命令：! 前缀直接执行 shell
	const shellDesc =
		lang === "zh"
			? "直接执行 shell 命令，例如 !git status"
			: "Run a shell command, e.g. !git status";
	info(`  ${color.yellow(padTo("!<command>", NAME_COLUMN))}${color.dim(shellDesc)}`);
	info();
}

/** 生成底部提示行的补全文本（单行，超长由状态栏自行截断） */
export function formatHint(matches: CommandSpec[], lang: Lang): string {
	if (matches.length === 0) {
		return color.dim(lang === "zh" ? "无匹配命令 · /help 查看全部" : "no match · /help for all");
	}
	const names = matches.map((spec) => color.cyan(spec.name)).join(color.dim("  "));
	return `${color.dim("▸ ")}${names}`;
}
