// 自研单行编辑器 —— 替代 node:readline
//
// 为什么要自己写：readline 在刷新输入行时会发出「擦除到屏幕末尾」(\x1b[0J)，
// 会把屏幕底部的固定状态栏一起擦掉，也无法实现自定义的命令补全面板。
// 自己实现后，我们完全掌控每一次写入，只清当前输入行（\x1b[2K），绝不越界。
//
// 特性：光标移动、Home/End、Ctrl+A/E/U/K/W、历史上下翻、CJK 宽度感知、
//       超长时按光标位置横向滚动、非 TTY 自动降级为逐行读取。
import { emitKeypressEvents } from "node:readline";
import { displayWidth } from "./text.js";

export interface EditorKey {
	name?: string;
	ctrl: boolean;
	meta: boolean;
	shift: boolean;
	sequence: string;
}

export interface LineEditorOptions {
	/** 提示符（可带 ANSI 颜色） */
	prompt: string;
	/** 输入中按键回调；返回 true 表示已消费，编辑器不再处理 */
	onKey?: (key: EditorKey, line: string) => boolean;
	/** 非输入状态（例如流式输出中）的按键回调 */
	onIdleKey?: (key: EditorKey) => void;
}

const MAX_HISTORY = 200;

export class LineEditor {
	private readonly tty = process.stdin.isTTY === true && process.stdout.isTTY === true;
	private readonly options: LineEditorOptions;

	private active = false;
	private buffer = "";
	private cursor = 0;
	private resolver?: (value: string | null) => void;
	private history: string[] = [];
	private historyIndex = -1;
	private draft = "";
	private pipeBuffer = "";
	private disposed = false;

	constructor(options: LineEditorOptions) {
		this.options = options;
		if (this.tty) {
			emitKeypressEvents(process.stdin);
			process.stdin.setRawMode?.(true);
			process.stdin.resume();
			process.stdin.on("keypress", this.handleKeypress);
		} else {
			process.stdin.resume();
		}
	}

	/** 是否处于交互输入状态 */
	get prompting(): boolean {
		return this.active;
	}

	/** 当前输入内容（未提交） */
	get line(): string {
		return this.buffer;
	}

	/** 读取一行；返回 null 表示 EOF 或用户请求退出 */
	readLine(): Promise<string | null> {
		if (this.disposed) return Promise.resolve(null);
		if (!this.tty) return this.readLinePiped();
		return new Promise<string | null>((resolve) => {
			this.resolver = resolve;
			this.active = true;
			this.buffer = "";
			this.cursor = 0;
			this.historyIndex = -1;
			this.draft = "";
			this.render();
		});
	}

	/** 清掉当前输入行（主输出写入前调用） */
	erase(): void {
		if (this.tty && this.active) process.stdout.write("\r\u001b[2K");
	}

	/** 重绘输入行（主输出写入后调用） */
	redraw(): void {
		if (this.tty && this.active) this.render();
	}

	/** 退出前清理 */
	dispose(): void {
		this.disposed = true;
		if (this.tty) {
			process.stdin.off("keypress", this.handleKeypress);
			process.stdin.setRawMode?.(false);
			process.stdout.write("\u001b[r");
		}
	}

	// ── 内部实现 ─────────────────────────────────────────────

	private cols(): number {
		return Math.max(40, process.stdout.columns ?? 80);
	}

	/** 单行渲染：仅清当前行，按光标位置做横向窗口 */
	private render(): void {
		const promptWidth = displayWidth(this.options.prompt);
		const avail = Math.max(10, this.cols() - promptWidth - 1);

		const before = this.buffer.slice(0, this.cursor);
		let shownBefore = before;
		while (shownBefore.length > 0 && displayWidth(shownBefore) > avail - 1) {
			shownBefore = shownBefore.slice(1);
		}
		let shownAfter = this.buffer.slice(this.cursor);
		while (shownAfter.length > 0 && displayWidth(shownBefore) + displayWidth(shownAfter) > avail) {
			shownAfter = shownAfter.slice(0, -1);
		}

		const text = `${this.options.prompt}${shownBefore}${shownAfter}`;
		const cursorCol = promptWidth + displayWidth(shownBefore) + 1;
		process.stdout.write(`\r\u001b[2K${text}\u001b[${cursorCol}G`);
	}

	private insert(text: string): void {
		this.buffer = this.buffer.slice(0, this.cursor) + text + this.buffer.slice(this.cursor);
		this.cursor += text.length;
		this.render();
	}

	private submit(line: string): void {
		process.stdout.write("\n");
		this.active = false;
		if (line.trim()) {
			this.history.push(line);
			if (this.history.length > MAX_HISTORY) this.history.shift();
		}
		this.historyIndex = -1;
		const resolve = this.resolver;
		this.resolver = undefined;
		resolve?.(line);
	}

	private finishWithNull(): void {
		process.stdout.write("\n");
		this.active = false;
		const resolve = this.resolver;
		this.resolver = undefined;
		resolve?.(null);
	}

	private handleKeypress = (str: string, key: EditorKey): void => {
		if (!key) return;

		// 非输入状态（流式输出中）：交给外部处理中断/开关
		if (!this.active) {
			this.options.onIdleKey?.(key);
			return;
		}

		// 先给外部一次拦截机会（命令面板、快捷键等）
		if (this.options.onKey?.(key, this.buffer)) return;

		if (key.ctrl) {
			switch (key.name) {
				case "c":
					// 有内容先清空（shell 习惯），空行才请求退出
					if (this.buffer.length > 0) {
						this.buffer = "";
						this.cursor = 0;
						this.render();
					} else {
						this.finishWithNull();
					}
					return;
				case "d":
					if (this.buffer.length === 0) this.finishWithNull();
					return;
				case "a":
					this.cursor = 0;
					this.render();
					return;
				case "e":
					this.cursor = this.buffer.length;
					this.render();
					return;
				case "u":
					this.buffer = this.buffer.slice(this.cursor);
					this.cursor = 0;
					this.render();
					return;
				case "k":
					this.buffer = this.buffer.slice(0, this.cursor);
					this.render();
					return;
				case "w": {
					const head = this.buffer.slice(0, this.cursor).replace(/\s*\S*$/, "");
					this.buffer = head + this.buffer.slice(this.cursor);
					this.cursor = head.length;
					this.render();
					return;
				}
				default:
					return;
			}
		}

		switch (key.name) {
			case "return":
			case "enter":
				this.submit(this.buffer);
				return;
			case "backspace":
				if (this.cursor > 0) {
					this.buffer = this.buffer.slice(0, this.cursor - 1) + this.buffer.slice(this.cursor);
					this.cursor -= 1;
					this.render();
				}
				return;
			case "delete":
				if (this.cursor < this.buffer.length) {
					this.buffer = this.buffer.slice(0, this.cursor) + this.buffer.slice(this.cursor + 1);
					this.render();
				}
				return;
			case "left":
				if (this.cursor > 0) {
					this.cursor -= 1;
					this.render();
				}
				return;
			case "right":
				if (this.cursor < this.buffer.length) {
					this.cursor += 1;
					this.render();
				}
				return;
			case "home":
				this.cursor = 0;
				this.render();
				return;
			case "end":
				this.cursor = this.buffer.length;
				this.render();
				return;
			case "up":
				this.recallHistory(1);
				return;
			case "down":
				this.recallHistory(-1);
				return;
			default:
				break;
		}

		// 可打印字符（含中文等多字节序列）
		if (str && str >= " " && !key.meta) this.insert(str);
	};

	private recallHistory(direction: number): void {
		if (this.history.length === 0) return;
		if (this.historyIndex === -1) {
			if (direction < 0) return;
			this.draft = this.buffer;
			this.historyIndex = this.history.length - 1;
		} else {
			const next = this.historyIndex - direction;
			if (next < 0) {
				this.historyIndex = -1;
				this.buffer = this.draft;
				this.cursor = this.buffer.length;
				this.render();
				return;
			}
			if (next >= this.history.length) return;
			this.historyIndex = next;
		}
		this.buffer = this.history[this.historyIndex] ?? "";
		this.cursor = this.buffer.length;
		this.render();
	}

	/** 非 TTY：按行读取 */
	private readLinePiped(): Promise<string | null> {
		return new Promise<string | null>((resolve) => {
			const onData = (chunk: Buffer): void => {
				this.pipeBuffer += chunk.toString("utf8");
				const index = this.pipeBuffer.indexOf("\n");
				if (index < 0) return;
				const line = this.pipeBuffer.slice(0, index).replace(/\r$/, "");
				this.pipeBuffer = this.pipeBuffer.slice(index + 1);
				cleanup();
				process.stdout.write(`${this.options.prompt}${line}\n`);
				resolve(line);
			};
			const onEnd = (): void => {
				cleanup();
				const rest = this.pipeBuffer.trim();
				this.pipeBuffer = "";
				resolve(rest ? rest : null);
			};
			const cleanup = (): void => {
				process.stdin.off("data", onData);
				process.stdin.off("end", onEnd);
			};
			process.stdin.on("data", onData);
			process.stdin.on("end", onEnd);
		});
	}
}
