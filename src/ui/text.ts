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
