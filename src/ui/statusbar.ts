// 底部固定状态栏 —— 通过滚动区域（DECSTBM）把最后两行钉在屏幕底部
//
// 布局：
//   第 1 行（提示行）：命令补全提示 / 快捷键提示
//   第 2 行（状态栏）：会话 · 模型 · 思考 · 搜索 · 语言
//
// 实现要点：
//   - 把滚动区域限制为 1..(rows-2)，主输出只在该区域内滚动，状态栏不会被顶走；
//   - 输入行不单独定位，而是「停在滚动区域底部」：输出换行只让区域内滚动，
//     光标因此始终停留在底部，输入行也就被钉住了；
//   - 绘制状态栏时用 \x1b7 / \x1b8 保存与恢复光标，不干扰行编辑器；
//   - ⚠ 终端尺寸变化时必须重建滚动区域**并把光标重新锚定到底部**：
//     全屏后光标会停在旧位置（屏幕中部甚至顶部），若只重建区域不动光标，
//     输出就会从光标处开始向下写，直接覆盖已有内容；
//   - 非 TTY 或 PI_UI=plain 时自动退化为「不渲染」，由调用方打印普通状态行。
import { truncateTo } from "./text.js";

const PANEL_ROWS = 2;

export interface StatusBar {
	/** 是否启用（非 TTY 时为 false，调用方需自行降级） */
	readonly enabled: boolean;
	/** 设置内容并重绘 */
	set(hint: string, status: string): void;
	/** 仅重绘 */
	refresh(): void;
	/** 终端尺寸变化后：重建滚动区域、重新锚定光标、重绘面板 */
	handleResize(): void;
	/** 退出前恢复终端（重置滚动区域并清空面板） */
	dispose(): void;
}

export function createStatusBar(): StatusBar {
	const out = process.stdout;
	const enabled =
		out.isTTY === true && process.env.PI_UI !== "plain" && (out.rows ?? 0) >= 12;

	let hint = "";
	let status = "";
	let started = false;
	/** 上一次使用的滚动区域底部行（用于清理旧位置的面板残留） */
	let lastBottom = 0;

	const rows = (): number => Math.max(12, out.rows ?? 24);
	const cols = (): number => Math.max(40, out.columns ?? 80);
	/** 面板首行（= rows-1） */
	const panelTop = (): number => rows() - PANEL_ROWS + 1;
	/** 滚动区域底部 = 输入行所在行 */
	const scrollBottom = (): number => rows() - PANEL_ROWS;

	/** 定位到若干行并清行（用 7/8 保存恢复光标，不留副作用） */
	const clearLines = (from: number, count: number): string => {
		let buf = "\u001b7";
		for (let i = 0; i < count; i += 1) {
			buf += `\u001b[${Math.max(1, from + i)};1H\u001b[2K`;
		}
		return `${buf}\u001b8`;
	};

	/**
	 * 设置滚动区域。
	 * @param placeCursor 是否把光标锚定到滚动区域底部。
	 *   启动时置 true 让输入行紧贴状态栏上方；
	 *   尺寸变化时也必须置 true，否则「光标停在底部」这一不变式被打破。
	 * @param clearLine 锚定光标时是否顺带清掉该行内容。
	 *   启动时不能清（该行可能还有刚打印的头部内容）；
	 *   尺寸变化后该行已是被终端重排过的残留，必须清。
	 */
	const init = (placeCursor: boolean, clearLine = false): void => {
		if (!enabled) return;
		const bottom = scrollBottom();
		out.write(`\u001b[1;${bottom}r`);
		if (placeCursor) {
			out.write(`\u001b[${bottom};1H${clearLine ? "\u001b[2K" : ""}`);
		}
		lastBottom = bottom;
		started = true;
	};

	/** 重绘面板（保存/恢复光标，不影响主输出与输入行） */
	const paint = (): void => {
		if (!enabled) return;
		const width = cols();
		const lines = [truncateTo(hint, width), truncateTo(status, width)];
		const top = panelTop();
		let buf = "\u001b7";
		for (let i = 0; i < PANEL_ROWS; i += 1) {
			buf += `\u001b[${top + i};1H\u001b[2K${lines[i] ?? ""}`;
		}
		buf += "\u001b8";
		out.write(buf);
	};

	return {
		enabled,
		set(nextHint: string, nextStatus: string): void {
			hint = nextHint;
			status = nextStatus;
			if (!started) init(true);
			paint();
		},
		refresh(): void {
			paint();
		},
		handleResize(): void {
			if (!enabled) return;
			// 1) 终端变高时旧面板会留在滚动区域里，先擦掉
			if (started && lastBottom > 0) {
				out.write(clearLines(lastBottom + 1, PANEL_ROWS));
			}
			// 2) 用新尺寸重建滚动区域，并把光标重新锚定到底部（关键步骤）
			init(true, true);
			// 3) 在新位置重绘面板
			paint();
		},
		dispose(): void {
			if (!enabled) return;
			out.write(`\u001b[r${clearLines(panelTop(), PANEL_ROWS)}`);
		},
	};
}
