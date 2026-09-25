// 交互式 REPL —— DSP 界面
//
// 屏幕布局（自上而下）：
//   1. 启动头部：DSP ASCII 字形 + 登录状态（保持极简）
//   2. 主输出区：对话流、思考、工具调用、! 命令结果（滚动区域 1..rows-2）
//   3. 输入行：❯ 对话 / ! 命令（滚动区域最后一行）
//   4. 提示行：分隔线 / 命令补全
//   5. 状态栏：会话 · 模型 · 思考 · 搜索 · 语言 · 滚动位置   ← 固定在屏幕最底部
//
// 输入约定：
//   普通文本 → 发送给 Agent
//   /xxx     → 斜杠命令（输入 / 查看全部）
//   !xxx     → 直接执行 shell 命令，结果只打印，不进入对话上下文
//
// 字形约定（一律不用 emoji）：▌ 工具调用/思考   └ 工具结果   · 元信息
//
// 折叠与滚动：思考、exec 输出都是可折叠块；主输出区支持向上滚动查看历史。
// 选择与复制：左键拖拽选择（自己渲染反显），右键或 Ctrl+C 复制到剪贴板；
//   单击（没有拖拽）则切换块折叠。终端原生选区读不到，所以选择必须自实现。
//   /goto 会列出本次会话发过的提示词，选中后滚动到它所在的位置。
//
// ⚠ 列偏移陷阱（曾导致输出覆盖已有内容）：
//   raw 模式下 \n 只换行、不回列（Windows 上 libuv 设 DISABLE_NEWLINE_AUTO_RETURN），
//   若某一行不先 \r 归位，列偏移会逐行累积，长行折行后越过滚动区域底边，
//   整屏就开始互相覆盖。因此主输出统一走 info()，并由状态栏把光标钉在区域底部。
import type { App } from "../app.js";
import { isAuthError } from "../deepseek/provider.js";
import { runShell } from "../tools/exec.js";
import { describeCall, type ToolCall, type ToolName, type ToolResult } from "../tools/index.js";
import { APP_FULL_NAME, APP_NAME, bannerLines } from "../ui/banner.js";
import { copyToClipboard } from "../ui/clipboard.js";
import { LineEditor, type EditorKey, type MouseEvent } from "../ui/line-editor.js";
import {
	color,
	disableMouse,
	enableMouse,
	error as printError,
	info,
	setOutputAnchor,
	writeLiveLine,
} from "../ui/output.js";
import { formatHint, printCommandPalette } from "../ui/palette.js";
import { createRenderer, type Anchor, type Block, type Renderer, type Selection } from "../ui/renderer.js";
import { createStatusBar } from "../ui/statusbar.js";
import { alignRight, displayWidth, formatDuration, truncateTo } from "../ui/text.js";
import { suggestCommands } from "./command-specs.js";
import { handleCommand } from "./commands.js";
import { describeError, runTurn, type TurnIO } from "./loop.js";

/** 对话模式提示符 */
const PROMPT_CHAT = color.cyan("❯ ");
/** shell 命令模式提示符（输入以 ! 开头时切换） */
const PROMPT_SHELL = color.yellow("! ");

/** 工具调用行首标记（块元素，不会被渲染成 emoji） */
const GLYPH_CALL = "▌";
/** 工具结果行首标记（制表符，同上） */
const GLYPH_RESULT = "└";
/** 元信息行首标记 */
const GLYPH_META = "·";

/** 工具名对齐列宽（最长的是 search） */
const TOOL_NAME_WIDTH = 6;
/** 思考 spinner 帧与刷新间隔 */
const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SPINNER_INTERVAL_MS = 90;
/** exec 输出超过这个行数时默认收起 */
const EXEC_COLLAPSE_LINES = 6;
/** 滚轮每格滚动的行数 */
const WHEEL_STEP = 3;

/** 按工具类型配色，一眼区分「读 / 写 / 列目录 / 执行 / 搜索」 */
const TOOL_COLOR: Record<ToolName, (text: string) => string> = {
	read: color.cyan,
	write: color.green,
	list: color.blue,
	exec: color.yellow,
	search: color.magenta,
};

/** 主输出门面：TTY 下走块渲染器（可折叠/可滚动/可重绘），否则逐行打印 */
interface Emitter {
	line(text: string): void;
	block(head: string, body: string[], collapsed: boolean): void;
}

function createEmitter(render: Renderer | null): Emitter {
	return {
		line(text) {
			if (render) render.line(text);
			else info(text);
		},
		block(head, body, collapsed) {
			if (!render) {
				info(head);
				if (!collapsed) for (const line of body) info(line);
				return;
			}
			if (body.length === 0) {
				info(head);
				return;
			}
			render.block(head, body, collapsed);
		},
	};
}

/** 跨轮共享的界面状态 */
interface UiState {
	/** 最近一次思考全文，供 Ctrl+O 展开回放 */
	lastThinking: string;
}

/** 提示词跳转选择器 */
interface Picker {
	block: Block;
	items: Anchor[];
	index: number;
}

/** 状态栏文本：会话 · 模型 · 思考 · 搜索 · 语言（滚动时附带位置） */
export function statusLine(app: App, busy = false, elapsedMs?: number, scrollRows = 0): string {
	const { t } = app.i18n;
	const flag = (value: boolean): string =>
		value ? color.green(t("status.on")) : color.dim(t("status.off"));
	const marker = busy ? color.yellow("◆") : color.cyan("◆");
	const timer = elapsedMs != null ? ` ${formatDuration(elapsedMs)}` : "";
	const title = busy
		? `${color.yellow(app.activeSession.title)} ${color.yellow(`(${t("status.busy")}${timer}…)`)}`
		: color.cyan(app.activeSession.title);
	const parts = [
		`${marker} ${title}`,
		color.bold(app.getModel().id),
		`${t("status.thinking")} ${flag(app.config.thinking)}`,
		`${t("status.search")} ${flag(app.config.search)}`,
		color.dim(app.config.language),
	];
	if (scrollRows > 0) parts.push(color.yellow(`↑${scrollRows}`));
	return parts.join(color.dim("  ·  "));
}

/** 工具调用行：`  ▌ read   package.json` */
function toolCallLine(call: ToolCall): string {
	const paint = TOOL_COLOR[call.name] ?? color.magenta;
	return `  ${paint(GLYPH_CALL)} ${paint(call.name.padEnd(TOOL_NAME_WIDTH))}${color.dim(describeCall(call))}`;
}

/**
 * 工具结果行：`  └ read   package.json                7ms`
 * 耗时右对齐到终端最右；非 TTY 或空间不足时退化为 `内容 耗时`。
 */
function toolResultLine(prefix: string, summary: string, ok: boolean, elapsedMs: number): string {
	const head = `  ${color.dim(GLYPH_RESULT)} ${prefix}`;
	const body = ok ? summary : color.red(summary);
	const right = color.dim(formatDuration(elapsedMs));
	const width = process.stdout.isTTY ? (process.stdout.columns ?? 80) : 0;
	if (width <= 0) return `${head}${body} ${right}`;
	const room = Math.max(8, width - displayWidth(head) - displayWidth(right) - 2);
	return alignRight(`${head}${truncateTo(body, room)}`, right);
}

/** 思考全文 → 带缩进的正文行 */
function thinkBody(text: string): string[] {
	return text
		.split("\n")
		.filter((line) => line.trim() !== "")
		.map((line) => color.gray(`    ${line}`));
}

/** 从 exec 的工具结果里提取可展示的输出体（去掉 [exec] 与退出码两行） */
function execBody(result: ToolResult): string[] {
	const lines = result.output.split("\n");
	const body = lines.slice(2);
	while (body.length > 0 && body[body.length - 1].trim() === "") body.pop();
	return body.map((line) => color.dim(`    ${line}`));
}

/** 执行 `!命令`：直接跑 shell，输出只打印不进对话上下文 */
async function runShellLine(app: App, emit: Emitter, command: string): Promise<void> {
	const { t } = app.i18n;
	if (!command) {
		emit.line(`  ${color.dim(t("shell.usage"))}`);
		return;
	}
	emit.line(`  ${color.yellow(GLYPH_CALL)} ${color.yellow(command)}`);
	const result = await runShell(command, app.cwd);
	const body = result.output ? result.output.split("\n").map((line) => `  ${line}`) : [];
	if (body.length === 0) body.push(color.dim(`  ${t("shell.noOutput")}`));
	const state = result.ok
		? color.green(t("shell.exit", { code: 0 }))
		: color.red(t("shell.exit", { code: result.code }));
	const head = alignRight(
		`  ${color.dim(GLYPH_RESULT)} ${state}`,
		color.dim(formatDuration(result.durationMs)),
	);
	emit.block(head, body, body.length > EXEC_COLLAPSE_LINES);
}

/** 构造本轮的渲染回调（Agent 风格：▌ 工具 / └ 结果 / 折叠思考） */
function createTurnIO(app: App, ui: UiState, emit: Emitter, foldable: boolean): TurnIO {
	const { t } = app.i18n;
	const turnStartedAt = Date.now();
	let batch: ToolCall[] = [];
	let batchStartedAt = turnStartedAt;
	let batchDone = 0;
	let thinkFolded = true;
	let thinkStartedAt = turnStartedAt;
	let spinTimer: ReturnType<typeof setInterval> | null = null;
	let spinFrame = 0;

	const stopSpinner = (): void => {
		if (spinTimer !== null) {
			clearInterval(spinTimer);
			spinTimer = null;
		}
	};

	/** 原地刷新「⠋ 思考 1.2s」活动行 */
	const paintSpinner = (): void => {
		const sec = ((Date.now() - thinkStartedAt) / 1000).toFixed(1);
		const frame = SPINNER[spinFrame % SPINNER.length];
		spinFrame += 1;
		writeLiveLine(
			`  ${color.cyan(frame)} ${color.dim(t("repl.thinkingLabel"))} ${color.dim(`${sec}s`)}`,
		);
	};

	return {
		// ── 思考：默认不展示正文，只显示 spinner 活动行 ──
		onThinkStart() {
			thinkStartedAt = Date.now();
			spinFrame = 0;
			thinkFolded = !app.config.showThinking;
			if (!thinkFolded) {
				emit.line("");
				process.stdout.write(`  ${color.dim(GLYPH_CALL)} ${color.dim(t("repl.thinkingLabel"))}  `);
				return;
			}
			if (!foldable) return; // 非 TTY 没有「活动行」
			paintSpinner();
			spinTimer = setInterval(paintSpinner, SPINNER_INTERVAL_MS);
		},
		onThinkDelta(text) {
			if (!thinkFolded) process.stdout.write(color.gray(text));
		},
		onThinkEnd(fullText, elapsedMs) {
			stopSpinner();
			const summary = t("repl.thinkingDone", {
				sec: formatDuration(elapsedMs),
				chars: fullText.length,
			});
			if (!thinkFolded) {
				process.stdout.write("\r\n");
				emit.line(`  ${color.dim(GLYPH_RESULT)} ${color.dim(`${summary} · ${t("repl.collapse")}`)}`);
				return;
			}
			ui.lastThinking = fullText;
			// 折叠块头会覆盖掉同一位置的活动行
			const head = `  ${color.dim(GLYPH_CALL)} ${color.dim(summary)} ${color.dim(`· ${t("repl.expand")}`)}`;
			emit.block(head, thinkBody(fullText), true);
		},

		// ── 正文 ──
		onContentStart() {
			emit.line("");
		},
		onContentDelta(text) {
			emit.line(text.replace(/\n$/, ""));
		},

		// ── 工具 ──
		onToolBatchStart(calls) {
			batch = calls;
			batchDone = 0;
			batchStartedAt = Date.now();
			emit.line("");
			for (const call of calls) emit.line(toolCallLine(call));
		},
		onToolEnd(call, result, elapsedMs) {
			batchDone += 1;
			const prefix = batch.length > 1 ? color.dim(call.name.padEnd(TOOL_NAME_WIDTH)) : "";
			const head = toolResultLine(prefix, result.summary, result.ok, elapsedMs);
			const body = call.name === "exec" && result.ok ? execBody(result) : [];
			emit.block(head, body, body.length > EXEC_COLLAPSE_LINES);
			if (batchDone >= batch.length && batch.length > 1) {
				const span = formatDuration(Date.now() - batchStartedAt);
				emit.line(`  ${color.dim(`${GLYPH_META} ${t("repl.parallel", { n: batch.length })}  ·  ${span}`)}`);
			}
		},

		onDone(_finishReason, usage) {
			const parts: string[] = [];
			if (usage != null) parts.push(`${usage} ${t("status.tokens")}`);
			parts.push(formatDuration(Date.now() - turnStartedAt));
			emit.line("");
			emit.line(`  ${color.dim(`${GLYPH_META} ${parts.join("  ·  ")}`)}`);
		},
		onNotice(text) {
			emit.line(`  ${color.yellow(`! ${text}`)}`);
		},
	};
}

/** 启动 REPL 主循环 */
export async function startRepl(app: App): Promise<void> {
	const { t } = app.i18n;
	const ui: UiState = { lastThinking: "" };

	// ── 状态栏与块渲染器 ──────────────────────────────────
	const bar = createStatusBar();
	const render = bar.enabled
		? createRenderer({ height: () => bar.height(), width: () => bar.width() })
		: null;
	const emit = createEmitter(render);
	const defaultHint = color.dim("─".repeat(240));
	let paletteShownFor: string | undefined;
	let picker: Picker | null = null;

	// ── 启动头部：极简，仅 ASCII 字形 + 登录状态 ──────────
	const token = app.getToken();
	const stateText = token
		? color.green(`✓ ${t("ui.loginOk", { len: token.length })}`)
		: color.yellow(`✗ ${t("ui.loginMissing")}`);
	for (const line of bannerLines([
		"",
		`${color.bold(APP_NAME)}  ${color.dim(`(${APP_FULL_NAME})`)}`,
		stateText,
	])) {
		emit.line(line);
	}

	// 初始化滚动区域（此前无 anchor，banner 写在顶部），再挂上光标归位
	bar.set(defaultHint, statusLine(app));
	setOutputAnchor(bar.enabled ? () => bar.anchorCursor() : null);
	if (!bar.enabled) info(statusLine(app));

	const scrollRows = (): number => render?.offset() ?? 0;

	const refreshUi = (): void => {
		if (bar.enabled) bar.set(defaultHint, statusLine(app, false, undefined, scrollRows()));
	};

	// ── 忙碌态与实时计时 ──────────────────────────────────
	let activeAbort: AbortController | null = null;
	let busy = false;
	let busyTimer: ReturnType<typeof setInterval> | null = null;
	let busyStartedAt = 0;

	const stopBusyTimer = (): void => {
		if (busyTimer !== null) {
			clearInterval(busyTimer);
			busyTimer = null;
		}
	};

	const setBusy = (next: boolean): void => {
		busy = next;
		stopBusyTimer();
		if (next) busyStartedAt = Date.now();
		bar.set(defaultHint, statusLine(app, busy, next ? 0 : undefined, scrollRows()));
		if (next && bar.enabled) {
			busyTimer = setInterval(() => {
				bar.set(
					defaultHint,
					statusLine(app, true, Date.now() - busyStartedAt, scrollRows()),
				);
			}, 1000);
		}
	};

	const barStatus = (): string =>
		statusLine(app, busy, busy ? Date.now() - busyStartedAt : undefined, scrollRows());

	// ── 选择与复制 ────────────────────────────────────────
	let selection: Selection | null = null;
	let dragAnchor: { row: number; col: number } | null = null;
	let pressRow = 0;
	let pressCol = 0;
	let dragging = false;

	const clearSelection = (): void => {
		if (!selection) return;
		selection = null;
		render?.setSelection(null);
	};

	const copySelection = async (): Promise<void> => {
		if (!render || !selection) return;
		const text = render.selectionText(selection);
		clearSelection();
		if (!text) {
			editor.redraw();
			return;
		}
		const ok = await copyToClipboard(text);
		editor.erase();
		emit.line(
			ok
				? `  ${color.green(t("repl.copied", { chars: text.length }))}`
				: `  ${color.red(t("repl.copyFailed"))}`,
		);
		bar.refresh();
		editor.redraw();
	};

	// ── 提示词跳转选择器 ──────────────────────────────────
	const pickerBody = (items: Anchor[], index: number): string[] => {
		const lines = items.map((a, i) =>
			i === index
				? `${color.cyan("  ▸ ")}${color.bold(`#${i + 1}`)}  ${a.label}`
				: color.dim(`    #${i + 1}  ${a.label}`),
		);
		lines.push(color.dim(`  ${t("goto.hint")}`));
		return lines;
	};

	const closePicker = (message: string): void => {
		if (!picker) return;
		const block = picker.block;
		picker = null;
		render?.update(block, [message]);
	};

	const movePicker = (delta: number): void => {
		if (!picker || !render) return;
		const next = picker.index + delta;
		if (next < 0 || next >= picker.items.length) return;
		picker.index = next;
		render.update(picker.block, pickerBody(picker.items, next));
	};

	const confirmPicker = (): void => {
		if (!picker || !render) return;
		const { items, index } = picker;
		const target = items[index];
		render.scrollToItem(target.itemIndex);
		closePicker(color.green(`  ${t("goto.jumped", { index: index + 1, label: target.label })}`));
	};

	const openGoto = (raw: string): void => {
		if (!render) return;
		const items = render.anchors();
		if (items.length === 0) {
			emit.line(color.dim(`  ${t("goto.empty")}`));
			return;
		}
		// /goto 3 直接跳转，不带参数则弹出选择框
		const direct = /^#?(\d+)$/.exec(raw.trim());
		if (direct) {
			const index = Number(direct[1]) - 1;
			if (index < 0 || index >= items.length) {
				emit.line(color.yellow(`  ${t("goto.outOfRange", { max: items.length })}`));
				return;
			}
			render.scrollToItem(items[index].itemIndex);
			emit.line(
				color.green(`  ${t("goto.jumped", { index: index + 1, label: items[index].label })}`),
			);
			return;
		}
		picker = {
			items,
			index: 0,
			block: render.block(color.bold(`  ${t("goto.title")}`), pickerBody(items, 0), false),
		};
	};

	/** 回显用户输入：同时进入渲染器缓冲区，保证整体重绘不会丢掉它 */
	const echoInput = (text: string): void => {
		emit.line(`${color.cyan("❯ ")}${color.bold(text)}`);
	};

	/** 记录提示词锚点（必须在回显之前调用，这样锚点指向回显那一行） */
	const rememberPrompt = (text: string): void => {
		render?.anchor(truncateTo(text.replace(/\s+/g, " ").trim(), 60));
	};

	// ── 鼠标 ──────────────────────────────────────────────
	const onMouse = (event: MouseEvent): void => {
		if (!render) return;
		const contentRows = Math.max(3, bar.height() - 1); // 最后一行是输入行

		// 滚轮：浏览历史
		if (event.button === 64 || event.button === 65) {
			clearSelection();
			render.scrollBy(event.button === 64 ? WHEEL_STEP : -WHEEL_STEP);
			bar.set(defaultHint, barStatus());
			return;
		}

		// 右键：复制当前选择
		if (event.button === 2) {
			if (!event.release) void copySelection();
			return;
		}

		if (event.button !== 0 && event.button !== 32) return;
		// 输入行与面板不属于主输出
		if (event.y > contentRows) return;
		const y = Math.max(1, event.y);
		const x = Math.max(1, event.x);

		// 左键按下：记录起点，同时清掉上一次的选择
		if (event.button === 0 && !event.release) {
			pressRow = y;
			pressCol = x;
			dragAnchor = { row: y, col: x };
			dragging = false;
			clearSelection();
			return;
		}

		// 按住左键移动：扩展选择（?1002h 才会持续上报运动事件）
		if (event.button === 32) {
			if (!dragAnchor) return;
			if (Math.abs(y - pressRow) > 0 || Math.abs(x - pressCol) > 0) dragging = true;
			selection = { startRow: dragAnchor.row, startCol: dragAnchor.col, endRow: y, endCol: x };
			render.setSelection(selection);
			return;
		}

		// 左键抬起
		dragAnchor = null;
		if (dragging && selection) return; // 拖拽结束：保留高亮，等右键 / Ctrl+C 复制
		dragging = false;
		// 纯点击：切换块折叠
		clearSelection();
		const block = render.blockAtRow(y);
		if (!block) return;
		editor.erase();
		render.toggle(block);
		bar.refresh();
		editor.redraw();
	};

	/** 展开 / 折叠思考内容；展开时回放最近一次思考全文 */
	const toggleThinkingView = (): void => {
		const next = !app.config.showThinking;
		app.setShowThinking(next);
		editor.erase();
		emit.line(`  ${color.dim(t("toggle.thinkingView", { state: next ? t("status.on") : t("status.off") }))}`);
		if (next) {
			const body = thinkBody(ui.lastThinking);
			if (body.length === 0) emit.line(color.dim(`  ${t("repl.noThinking")}`));
			else for (const line of body) emit.line(line);
		}
		editor.redraw();
		bar.set(defaultHint, barStatus());
	};

	const showPalette = (): void => {
		editor.erase();
		printCommandPalette(app.i18n.lang, emit.line);
		editor.redraw();
		bar.refresh();
	};

	const updateHint = (): void => {
		const line = editor.prompting ? editor.line : "";
		if (line.startsWith("/") && !line.includes(" ")) {
			bar.set(formatHint(suggestCommands(line, app.i18n.lang), app.i18n.lang), barStatus());
			if (line === "/" && paletteShownFor !== "/") {
				paletteShownFor = "/";
				showPalette();
			} else if (line !== "/") {
				paletteShownFor = undefined;
			}
			return;
		}
		paletteShownFor = undefined;
		bar.set(defaultHint, barStatus());
	};

	const syncPrompt = (): void => {
		editor.setPrompt(editor.line.startsWith("!") ? PROMPT_SHELL : PROMPT_CHAT);
	};

	const toggle = (kind: "thinking" | "search"): void => {
		if (kind === "thinking") app.setThinking(!app.config.thinking);
		else app.setSearch(!app.config.search);
		bar.set(defaultHint, barStatus());
		editor.redraw();
	};

	/** 键盘侧的历史导航 */
	const scrollByKeyboard = (toBottom: boolean): void => {
		if (!render) return;
		clearSelection();
		if (toBottom) render.scrollToBottom();
		else render.scrollBy(1 << 30); // 大幅上滚，越界会被渲染器夹到顶部
		bar.set(defaultHint, barStatus());
		editor.redraw();
	};

	const editor = new LineEditor({
		prompt: PROMPT_CHAT,
		// 输入行交由我们改写为带样式的回显，所以编辑器自己不要换行
		keepInputLine: true,
		onKey: (key: EditorKey): boolean => {
			// 选择器打开时接管全部按键
			if (picker) {
				if (key.name === "up" || key.name === "k") movePicker(-1);
				else if (key.name === "down" || key.name === "j") movePicker(1);
				else if (key.name === "return" || key.name === "enter") confirmPicker();
				else if (key.name === "escape" || (key.ctrl && key.name === "c")) {
					closePicker(color.dim(`  ${t("goto.cancelled")}`));
				}
				return true;
			}
			if (key.ctrl && key.name === "c") {
				// 有选择就先复制，否则才是中断生成
				if (selection) void copySelection();
				else activeAbort?.abort();
				return true;
			}
			if (key.ctrl && key.name === "t") {
				toggle("thinking");
				return true;
			}
			if (key.ctrl && key.name === "s") {
				toggle("search");
				return true;
			}
			if (key.ctrl && key.name === "o") {
				toggleThinkingView();
				return true;
			}
			if (key.ctrl && key.name === "down") {
				scrollByKeyboard(true);
				return true;
			}
			if (key.ctrl && key.name === "up") {
				scrollByKeyboard(false);
				return true;
			}
			setImmediate(() => {
				syncPrompt();
				updateHint();
			});
			return false;
		},
		// 非输入状态（流式输出中）：只处理中断、滚动与开关。
		// 这里刻意不处理 Ctrl+O —— 展开需要排版输出，不能与流式写入并发。
		onIdleKey: (key: EditorKey): void => {
			if (key.ctrl && key.name === "c") {
				if (selection) void copySelection();
				else activeAbort?.abort();
				return;
			}
			if (key.ctrl && key.name === "down") scrollByKeyboard(true);
			else if (key.ctrl && key.name === "up") scrollByKeyboard(false);
			else if (key.ctrl && key.name === "t") toggle("thinking");
			else if (key.ctrl && key.name === "s") toggle("search");
		},
		onMouse,
	});
	if (bar.enabled) enableMouse();
	refreshUi();

	// ── 尺寸变化 ──────────────────────────────────────────
	process.stdout.on("resize", () => {
		bar.handleResize();
		render?.repaint();
		updateHint();
		editor.redraw();
	});

	// ── 退出清理 ──────────────────────────────────────────
	let cleaned = false;
	const cleanup = (): void => {
		if (cleaned) return;
		cleaned = true;
		stopBusyTimer();
		setOutputAnchor(null);
		disableMouse();
		editor.dispose();
		bar.dispose();
	};
	process.on("exit", cleanup);

	// ── 主循环 ────────────────────────────────────────────
	const io = { print: (text?: string) => emit.line(text ?? "") };

	try {
		for (;;) {
			editor.setPrompt(PROMPT_CHAT);
			const input = await editor.readLine();
			if (input === null) break;

			const trimmed = input.trim();
			if (!trimmed) {
				editor.erase();
				refreshUi();
				continue;
			}

			// 空选择器残留时先关掉
			if (picker) closePicker(color.dim(`  ${t("goto.cancelled")}`));

			// 回显用户输入（覆盖输入行，同时进入渲染器缓冲区）
			echoInput(trimmed);
			clearSelection();

			// `!命令`：直接执行 shell，不进入对话上下文
			if (trimmed.startsWith("!")) {
				await runShellLine(app, emit, trimmed.slice(1).trim());
				refreshUi();
				continue;
			}

			// 便捷别名：直接输入 login / logout（无需斜杠）
			const bare = /^(login|logout)$/i.exec(trimmed);
			if (bare) {
				const outcome = await handleCommand(app, `/${bare[1].toLowerCase()}`, io);
				if (outcome.exit) break;
				refreshUi();
				continue;
			}

			// 斜杠命令
			if (trimmed.startsWith("/")) {
				const outcome = await handleCommand(app, trimmed, io);
				if (outcome.exit) break;
				if (outcome.goto !== undefined) {
					openGoto(outcome.goto);
					refreshUi();
					continue;
				}
				refreshUi();
				continue;
			}

			// 普通对话：先记锚点（供 /goto），再跑一轮
			rememberPrompt(trimmed);
			const controller = new AbortController();
			activeAbort = controller;
			setBusy(true);
			try {
				await runTurn(app, trimmed, createTurnIO(app, ui, emit, render !== null), controller.signal);
			} catch (e) {
				if (controller.signal.aborted) {
					emit.line(color.dim(`  ${t("repl.stopped")}`));
				} else {
					if (isAuthError(e)) app.clearAuthStore();
					printError(`  ${describeError(app, e)}`);
				}
			} finally {
				activeAbort = null;
				setBusy(false);
			}
			refreshUi();
		}
	} finally {
		cleanup();
	}

	emit.line("");
	emit.line(color.dim(`  ${t("app.bye")}`));
}
