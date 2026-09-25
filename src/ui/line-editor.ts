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

/** 鼠标事件（SGR 扩展模式） */
export interface MouseEvent {
	/** 0 左键 · 1 中键 · 2 右键 · 64 上滚 · 65 下滚 */
	button: number;
	/** 列号（1 起） */
	x: number;
	/** 行号（1 起） */
	y: number;
	/** 是否为抬起事件 */
	release: boolean;
}

/** SGR 鼠标序列：\x1b[<b;x;yM（按下）或 m（抬起） */
const MOUSE_RE = /^\u001b\[<(\d+);(\d+);(\d+)([Mm])$/;

/** 解析鼠标序列；不是鼠标事件时返回 undefined */
export function parseMouse(sequence: string): MouseEvent | undefined {
	const m = MOUSE_RE.exec(sequence);
	if (!m) return undefined;
	return { button: Number(m[1]), x: Number(m[2]), y: Number(m[3]), release: m[4] === "m" };
}

export interface LineEditorOptions {
	/** 提示符（可带 ANSI 颜色） */
	prompt: string;
	/** 输入中按键回调；返回 true 表示已消费，编辑器不再处理 */
	onKey?: (key: EditorKey, line: string) => boolean;
	/** 非输入状态（例如流式输出中）的按键回调 */
	onIdleKey?: (key: EditorKey) => void;
	/** 鼠标事件回调（需调用方先开启鼠标跟踪）；输入中与输出中都会触发 */
	onMouse?: (event: MouseEvent) => void;
	/**
	 * 提交后是否由编辑器自行换行。
	 * 置 false 时编辑器不动这一行，交由调用方接管 —— 例如把它改写为
	 * 带样式的用户消息回显（同时进入块渲染器缓冲区，保证重绘保真）。
	 */
	keepInputLine?: boolean;
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
	private pipeEnded = false;
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

	/** 动态切换提示符（例如输入 `!` 进入命令模式）；正在输入时立即重绘 */
	setPrompt(prompt: string): void {
		if (this.options.prompt === prompt) return;
		this.options.prompt = prompt;
		if (this.tty && this.active) this.render();
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
		// 必须用 \r\n：raw 模式下 \n 不回列，直接用 \n 会把输入行的列偏移
		// 带进后续输出，导致输出整体右移并最终折行覆盖状态栏。
		if (!this.options.keepInputLine) process.stdout.write("\r\n");
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
		process.stdout.write("\r\n");
		this.active = false;
		const resolve = this.resolver;
		this.resolver = undefined;
		resolve?.(null);
	}

	private handleKeypress = (str: string, key: EditorKey): void => {
		if (!key) return;

		// 鼠标事件优先处理（输入中与输出中都要响应点击折叠）
		const mouse = parseMouse(key.sequence ?? str);
		if (mouse) {
			this.options.onMouse?.(mouse);
			return;
		}

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

	/**
	 * 非 TTY 下的输入回显。
	 * 刻意留空：回显统一由调用方负责（TTY 与管道两条路径保持一致，
	 * 且回显内容要进入块渲染器缓冲区，否则整体重绘会把它擦掉）。
	 */
	private emitPiped(_line: string): void {
		// no-op
	}

	/** 非 TTY：按行读取（支持一次投喂多行、也支持流提前结束） */
	private readLinePiped(): Promise<string | null> {
		return new Promise<string | null>((resolve) => {
			/** 缓冲区里有一整行就取出一行并 resolve；返回是否取到 */
			const takeLine = (): boolean => {
				const index = this.pipeBuffer.indexOf("\n");
				if (index < 0) return false;
				const line = this.pipeBuffer.slice(0, index).replace(/\r$/, "");
				this.pipeBuffer = this.pipeBuffer.slice(index + 1);
				cleanup();
				this.emitPiped(line);
				resolve(line);
				return true;
			};

			const onData = (chunk: Buffer): void => {
				this.pipeBuffer += chunk.toString("utf8");
				takeLine();
			};

			const onEnd = (): void => {
				this.pipeEnded = true;
				// 先看是否还有完整行；没有的话，把最后一段无换行的内容当一行
				this.pipeBuffer = this.pipeBuffer.replace(/\r?\n$/, "");
				if (takeLine()) return;
				const rest = this.pipeBuffer.trim();
				this.pipeBuffer = "";
				cleanup();
				if (rest) this.emitPiped(rest);
				resolve(rest ? rest : null);
			};

			const cleanup = (): void => {
				process.stdin.off("data", onData);
				process.stdin.off("end", onEnd);
			};

			// 流早已结束（上一轮读取期间）：直接消费缓冲区，避免再次挂起
			if (this.pipeEnded) {
				onEnd();
				return;
			}
			process.stdin.on("data", onData);
			process.stdin.on("end", onEnd);
			// 数据可能在上一次 readLine 期间就已进入缓冲区，先尝试消费
			takeLine();
		});
	}
}
