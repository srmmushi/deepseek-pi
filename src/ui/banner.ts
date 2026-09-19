// 项目 ASCII banner
import { color, info } from "./output.js";

/** 左侧的 ASCII 字形（每行等宽 11 字符） */
const ART = [
	"██████╗ ██╗",
	"██╔══██╗██║",
	"██████╔╝██║",
	"██╔═══╝ ██║",
	"██║     ██║",
	"╚═╝     ╚═╝",
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
