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
 * 打印 banner。
 * @param right 右栏文字（按行对齐，空字符串表示留白），由调用方按语言传入
 */
export function printBanner(right: string[] = []): void {
	info();
	ART.forEach((row, index) => {
		info(`  ${color.cyan(row)}   ${color.dim(right[index] ?? "")}`);
	});
	info();
}
