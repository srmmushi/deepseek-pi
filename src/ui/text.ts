// 终端文本宽度工具 —— 处理 ANSI 颜色与中日韩全角字符的对齐
const ANSI_RE = /\u001b\[[0-9;]*m/g;

/** 去掉 ANSI 颜色序列 */
export function stripAnsi(text: string): string {
	return text.replace(ANSI_RE, "");
}

/** 单个码点的显示宽度（CJK / 全角记 2） */
export function charWidth(code: number): number {
	if (code < 0x20 || (code >= 0x7f && code < 0xa0)) return 0;
	if (code >= 0x0300 && code <= 0x036f) return 0; // 组合附加符号
	const wide =
		(code >= 0x1100 && code <= 0x115f) ||
		(code >= 0x2e80 && code <= 0x303e) ||
		(code >= 0x3041 && code <= 0x33ff) ||
		(code >= 0x3400 && code <= 0x4dbf) ||
		(code >= 0x4e00 && code <= 0x9fff) ||
		(code >= 0xa000 && code <= 0xa4cf) ||
		(code >= 0xac00 && code <= 0xd7a3) ||
		(code >= 0xf900 && code <= 0xfaff) ||
		(code >= 0xfe30 && code <= 0xfe6f) ||
		(code >= 0xff00 && code <= 0xff60) ||
		(code >= 0xffe0 && code <= 0xffe6) ||
		(code >= 0x20000 && code <= 0x3fffd);
	return wide ? 2 : 1;
}

/** 计算显示宽度（忽略 ANSI） */
export function displayWidth(text: string): number {
	let width = 0;
	for (const ch of stripAnsi(text)) {
		width += charWidth(ch.codePointAt(0) ?? 0);
	}
	return width;
}

/** 右补空格到指定显示宽度（保留原有颜色） */
export function padTo(text: string, width: number): string {
	const pad = width - displayWidth(text);
	return pad > 0 ? text + " ".repeat(pad) : text;
}

/** 把字节数格式化为紧凑体积（512B / 1.2KB / 3.4MB） */
export function formatBytes(bytes: number): string {
	if (bytes < 1024) return `${bytes}B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
	return `${(bytes / 1024 / 1024).toFixed(1)}MB`;
}

/** 把「左内容 …… 右内容」拼成一行，TTY 下把右侧对齐到终端最右 */
export function alignRight(left: string, right: string): string {
	const width = process.stdout.isTTY ? (process.stdout.columns ?? 80) : 0;
	if (width <= 0) return `${left} ${right}`;
	const pad = width - displayWidth(left) - displayWidth(right) - 1;
	if (pad < 2) return `${left} ${right}`;
	return `${left}${" ".repeat(pad)}${right}`;
}

/** 把毫秒格式化为紧凑时长（7ms / 640ms / 3.4s / 1m02s） */
export function formatDuration(ms: number): string {
	const clamped = Math.max(0, ms);
	if (clamped < 1000) return `${Math.round(clamped)}ms`;
	if (clamped < 60_000) return `${(clamped / 1000).toFixed(1)}s`;
	const total = Math.round(clamped / 1000);
	return `${Math.floor(total / 60)}m${String(total % 60).padStart(2, "0")}s`;
}

/** 按显示宽度截断并追加省略号（保留 ANSI 颜色序列） */
export function truncateTo(text: string, width: number): string {
	if (displayWidth(text) <= width) return text;
	let visible = 0;
	let result = "";
	for (let i = 0; i < text.length; i += 1) {
		if (text[i] === "\u001b") {
			const match = /^\u001b\[[0-9;]*m/.exec(text.slice(i));
			if (match) {
				result += match[0];
				i += match[0].length - 1;
				continue;
			}
		}
		const w = charWidth(text.codePointAt(i) ?? 0);
		if (visible + w > width - 1) break;
		result += text[i];
		visible += w;
	}
	return `${result}…\u001b[0m`;
}
