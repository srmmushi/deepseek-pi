// 项目 ASCII banner —— DSP (deepseek-pi)
import { color, info } from "./output.js";

/** 品牌短名 */
export const APP_NAME = "DSP";

/** 品牌全名 */
export const APP_FULL_NAME = "deepseek-pi";

/** 左侧的 ASCII 字形（D / S / P，每行等宽 26 字符） */
const ART = [
	"██████╗ ███████╗ ██████╗ ",
	"██╔══██╗██╔════╝ ██╔══██╗",
	"██║  ██║███████╗ ██████╔╝",
	"██║  ██║╚════██║ ██╔═══╝ ",
	"██████╔╝███████║ ██║     ",
	"╚═════╝ ╚══════╝ ╚═╝     ",
];

/**
 * 生成 banner 的每一行。
 * 交给块渲染器逐行纳入缓冲区（否则整体重绘时会把 banner 擦掉）。
 * @param right 右栏文字（按行对齐，空字符串表示留白），由调用方按语言传入
 */
export function bannerLines(right: string[] = []): string[] {
	const lines = [""];
	ART.forEach((row, index) => {
		lines.push(`  ${color.cyan(row)}   ${color.dim(right[index] ?? "")}`);
	});
	lines.push("");
	return lines;
}

/** 直接打印 banner（非块渲染路径使用） */
export function printBanner(right: string[] = []): void {
	for (const line of bannerLines(right)) info(line);
}
