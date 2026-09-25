// 终端输出与配色 —— 不引入第三方 UI 依赖，直接使用 ANSI 转义序列
const isTty = process.stdout.isTTY === true;
const useColor = isTty && process.env.NO_COLOR === undefined;

function wrap(open: number, close: number): (s: string) => string {
	return (s: string) => (useColor ? `\u001b[${open}m${s}\u001b[${close}m` : s);
}

export const color = {
	bold: wrap(1, 22),
	dim: wrap(2, 22),
	italic: wrap(3, 23),
	underline: wrap(4, 24),
	red: wrap(31, 39),
	green: wrap(32, 39),
	yellow: wrap(33, 39),
	blue: wrap(34, 39),
	magenta: wrap(35, 39),
	cyan: wrap(36, 39),
	gray: wrap(90, 39),
};

/**
 * 主输出前的光标归位回调（由状态栏注入）。
 *
 * 为什么必须归位：整套布局依赖「光标停在滚动区域底部、且位于第 1 列」。
 * 而 Node 进入 raw 模式后，`\n` 只做换行不回列
 * （Windows 上 libuv 会设置 DISABLE_NEWLINE_AUTO_RETURN，POSIX 上关掉 ONLCR），
 * 于是光标会带着上一行的列偏移进入下一行：输出整体右移，
 * 一旦右移到超过终端宽度就会折行越过滚动区域底边，之后整屏开始互相覆盖。
 */
let outputAnchor: (() => void) | null = null;

/** 注册 / 注销主输出归位回调（状态栏可用时注入，非 TTY 时为 null） */
export function setOutputAnchor(fn: (() => void) | null): void {
	outputAnchor = fn;
}

/** 普通信息输出（stdout）—— TTY 下写前归位光标，写后换行 */
export function info(message = ""): void {
	if (!isTty) {
		// 非 TTY（管道 / 重定向）：不写任何控制字符，保持日志干净
		process.stdout.write(`${message}\n`);
		return;
	}
	outputAnchor?.();
	process.stdout.write(`\r${message}\n`);
}

/** 错误输出（stderr） */
export function error(message: string): void {
	process.stderr.write(`${color.red(message)}\n`);
}

/** 警告输出（stderr） */
export function warn(message: string): void {
	process.stderr.write(`${color.yellow(message)}\n`);
}

/**
 * 原地刷新「活动行」：先归位行首、清行，再写入新内容（不加换行）。
 * 用于思考指示这类需要就地更新的单行。非 TTY 下无「活动行」概念，直接忽略。
 */
export function writeLiveLine(text: string): void {
	if (!isTty) return;
	outputAnchor?.();
	process.stdout.write(`\r\u001b[2K${text}`);
}

/** 结束「活动行」：清掉并写入终态（加换行）；非 TTY 下退化为普通一行 */
export function endLiveLine(text: string): void {
	if (!isTty) {
		info(text);
		return;
	}
	outputAnchor?.();
	process.stdout.write(`\r\u001b[2K${text}\n`);
}

/**
 * 流式输出原语：负责把增量片段立即写到终端。
 * 使用同步 write，保证顺序与实时性。
 */
export function writeChunk(text: string): void {
	process.stdout.write(text);
}

/** 在流式输出结束后补一个换行 */
export function endLine(): void {
	process.stdout.write("\r\n");
}

/**
 * 开启鼠标跟踪（SGR 扩展模式，能拿到精确的行列坐标）。
 * 注意：开启后终端会用鼠标事件代替「拖选文本」，退出前**必须**调用 disableMouse()。
 */
export function enableMouse(): void {
	if (isTty) process.stdout.write("\u001b[?1000h\u001b[?1006h");
}

/** 关闭鼠标跟踪（恢复终端的原生拖选行为） */
export function disableMouse(): void {
	if (isTty) process.stdout.write("\u001b[?1000l\u001b[?1006l");
}

/** 覆盖当前行（用于刷新状态栏） */
export function overwriteLine(text: string): void {
	process.stdout.write(`\r\u001b[2K${text}`);
}
