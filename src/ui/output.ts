// 终端输出与配色 —— 不引入第三方 UI 依赖，直接使用 ANSI 转义序列
const useColor = process.stdout.isTTY && process.env.NO_COLOR === undefined;

function wrap(open: number, close: number): (s: string) => string {
	return (s: string) => (useColor ? `\u001b[${open}m${s}\u001b[${close}m` : s);
}

export const color = {
	bold: wrap(1, 22),
	dim: wrap(2, 22),
	italic: wrap(3, 23),
	red: wrap(31, 39),
	green: wrap(32, 39),
	yellow: wrap(33, 39),
	blue: wrap(34, 39),
	magenta: wrap(35, 39),
	cyan: wrap(36, 39),
	gray: wrap(90, 39),
};

/** 普通信息输出（stdout） */
export function info(message = ""): void {
	process.stdout.write(`${message}\n`);
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
 * 流式输出原语：负责把增量片段立即写到终端。
 * 使用同步 write，保证顺序与实时性。
 */
export function writeChunk(text: string): void {
	process.stdout.write(text);
}

/** 在流式输出结束后补一个换行 */
export function endLine(): void {
	process.stdout.write("\n");
}

/** 覆盖当前行（用于刷新状态栏） */
export function overwriteLine(text: string): void {
	process.stdout.write(`\r\u001b[2K${text}`);
}
