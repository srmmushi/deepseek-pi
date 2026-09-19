// 底部固定状态栏 —— 通过滚动区域（DECSTBM）把最后两行钉在屏幕底部
//
// 布局：
//   第 1 行（提示行）：命令补全提示 / 快捷键提示
//   第 2 行（状态栏）：会话 · 模型 · 思考 · 搜索 · 语言
//
// 实现要点：
//   - 把滚动区域限制为 1..(rows-2)，主输出只在该区域内滚动，状态栏不会被顶走；
//   - 绘制状态栏时用 \x1b7 / \x1b8 保存与恢复光标，不干扰 readline 的行编辑；
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
	/** 终端尺寸变化后重新计算 */
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

	const rows = (): number => Math.max(12, out.rows ?? 24);
	const cols = (): number => Math.max(40, out.columns ?? 80);

	/**
	 * 设置滚动区域。
	 * @param placeCursor 是否把光标移到滚动区域底部 —— 首次启动时置 true，
	 *   让输入行紧贴底部状态栏，而不是停在屏幕顶部的头部信息下面。
	 */
	const init = (placeCursor: boolean): void => {
		if (!enabled) return;
		const bottom = rows() - PANEL_ROWS;
		out.write(`\u001b[1;${bottom}r`);
		if (placeCursor) out.write(`\u001b[${bottom};1H`);
		started = true;
	};

	/** 重绘面板（保存/恢复光标，不影响主输出与 readline） */
	const paint = (): void => {
		if (!enabled) return;
		const bottom = rows();
		const width = cols();
		const lines = [truncateTo(hint, width), truncateTo(status, width)];
		let buf = "\u001b7";
		for (let i = 0; i < PANEL_ROWS; i += 1) {
			buf += `\u001b[${bottom - PANEL_ROWS + 1 + i};1H\u001b[2K${lines[i] ?? ""}`;
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
			init(false);
			paint();
		},
		dispose(): void {
			if (!enabled) return;
			const bottom = rows();
			let buf = "\u001b[r\u001b7";
			for (let i = 0; i < PANEL_ROWS; i += 1) {
				buf += `\u001b[${bottom - PANEL_ROWS + 1 + i};1H\u001b[2K`;
			}
			buf += "\u001b8";
			out.write(buf);
		},
	};
}
