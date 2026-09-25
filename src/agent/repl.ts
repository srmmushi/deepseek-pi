// 交互式 REPL —— DSP 界面
//
// 屏幕布局（自上而下）：
//   1. 启动头部：DSP ASCII 字形 + 登录状态（保持极简）
//   2. 主输出区：对话流、思考、工具调用、! 命令结果（滚动区域 1..rows-2）
//   3. 输入行：❯ 对话 / ! 命令（停在滚动区域底部）
//   4. 提示行：分隔线，输入 / 时变为命令补全
//   5. 状态栏：会话 · 模型 · 思考 · 搜索 · 语言   ← 固定在屏幕最底部
//
// 输入约定：
//   普通文本 → 发送给 Agent
//   /xxx     → 斜杠命令（输入 / 查看全部）
//   !xxx     → 直接执行 shell 命令，结果只打印，不进入对话上下文
//
// 字形约定（一律不用 emoji，避免终端把符号渲染成彩色表情导致对不齐）：
//   ▌ 工具调用 / 思考      └ 工具结果      · 元信息
//
// 折叠：思考与 exec 输出都是「块」，点击块头（或 Ctrl+O）展开 / 收起。
// 滚动区域里的历史行无法单独擦除，所以主输出同时记入块渲染器的缓冲区，
// 折叠时整体重排并重绘可见窗口 —— 详见 ui/renderer.ts。
//
// ⚠ 列偏移陷阱（曾导致输出覆盖已有内容）：
//   raw 模式下 \n 只换行、不回列（Windows 上 libuv 会设 DISABLE_NEWLINE_AUTO_RETURN），
//   若某一行不先 \r 归位，列偏移会逐行累积，长行折行后越过滚动区域底边，
//   整屏就开始互相覆盖。因此主输出统一走 info()，并由状态栏在每行写入前
//   把光标钉回「滚动区域底部第 1 列」。
import type { App } from "../app.js";
import { isAuthError } from "../deepseek/provider.js";
import { runShell } from "../tools/exec.js";
import { describeCall, type ToolCall, type ToolName, type ToolResult } from "../tools/index.js";
import { APP_FULL_NAME, APP_NAME, bannerLines } from "../ui/banner.js";
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
import { createRenderer, type Renderer } from "../ui/renderer.js";
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

/** 按工具类型配色，一眼区分「读 / 写 / 列目录 / 执行 / 搜索」 */
const TOOL_COLOR: Record<ToolName, (text: string) => string> = {
	read: color.cyan,
	write: color.green,
	list: color.blue,
	exec: color.yellow,
	search: color.magenta,
};

/** 主输出门面：TTY 下走块渲染器（可折叠 + 整体重绘），否则逐行打印 */
interface Emitter {
	line(text: string): void;
	/** 可折叠块：非 TTY 下没有折叠概念，展开态直接铺开 */
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

/** 状态栏文本：会话 · 模型 · 思考 · 搜索 · 语言 */
// busy 时高亮并显示已耗时（elapsedMs 为 undefined 表示不显示计时）
export function statusLine(app: App, busy = false, elapsedMs?: number): string {
	const { t } = app.i18n;
	const flag = (value: boolean): string =>
		value ? color.green(t("status.on")) : color.dim(t("status.off"));
	const marker = busy ? color.yellow("◆") : color.cyan("◆");
	const timer = elapsedMs != null ? ` ${formatDuration(elapsedMs)}` : "";
	const title = busy
		? `${color.yellow(app.activeSession.title)} ${color.yellow(`(${t("status.busy")}${timer}…)`)}`
		: color.cyan(app.activeSession.title);
	return [
		`${marker} ${title}`,
		color.bold(app.getModel().id),
		`${t("status.thinking")} ${flag(app.config.thinking)}`,
		`${t("status.search")} ${flag(app.config.search)}`,
		color.dim(app.config.language),
	].join(color.dim("  ·  "));
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
	emit.line("");
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
	// shell 输出同样可折叠（超过阈值默认收起）
	emit.block(head, body, body.length > EXEC_COLLAPSE_LINES);
}

/** 构造本轮的渲染回调（Agent 风格：▌ 工具 / └ 结果 / 折叠思考） */
function createTurnIO(app: App, ui: UiState, emit: Emitter, foldable: boolean): TurnIO {
	const { t } = app.i18n;
	const turnStartedAt = Date.now();
	/** 当前并行批次 */
	let batch: ToolCall[] = [];
	let batchStartedAt = turnStartedAt;
	let batchDone = 0;
	/** 本次思考的显示状态 */
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

	/** 原地刷新「⠋ Thinking 2.1s」活动行 */
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
			// 非 TTY 没有「活动行」，结束时打一行摘要即可
			if (!foldable) return;
			paintSpinner();
			spinTimer = setInterval(paintSpinner, SPINNER_INTERVAL_MS);
		},
		onThinkDelta(text) {
			if (!thinkFolded) process.stdout.write(color.gray(text));
			// 折叠态不显示正文：spinner 已在跑，正文只在块里留存
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
			// 折叠块头会覆盖掉相同位置的活动行（首行写入即覆盖）
			const head = `  ${color.dim(GLYPH_CALL)} ${color.dim(summary)} ${color.dim(`· ${t("repl.expand")}`)}`;
			emit.block(head, thinkBody(fullText), true);
		},

		// ── 正文 ──
		onContentStart() {
			emit.line("");
		},
		onContentDelta(text) {
			// 内容按整行到达（loop 已按行切分），去掉尾部换行后逐行入库
			emit.line(text.replace(/\n$/, ""));
		},

		// ── 工具：一批调用先列出，结果按完成顺序打印 ──
		onToolBatchStart(calls) {
			batch = calls;
			batchDone = 0;
			batchStartedAt = Date.now();
			emit.line("");
			for (const call of calls) emit.line(toolCallLine(call));
		},
		onToolEnd(call, result, elapsedMs) {
			batchDone += 1;
			// 并行批次里结果顺序与调用顺序不一致，必须带工具名才好对应
			const prefix = batch.length > 1 ? color.dim(call.name.padEnd(TOOL_NAME_WIDTH)) : "";
			const head = toolResultLine(prefix, result.summary, result.ok, elapsedMs);
			// exec 输出折叠：行数多时默认收起，点击块头展开
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

	// ── 底部状态栏与块渲染器 ──────────────────────────────
	const bar = createStatusBar();
	const render = bar.enabled
		? createRenderer({ height: () => bar.height(), width: () => bar.width() })
		: null;
	const emit = createEmitter(render);
	const defaultHint = color.dim("─".repeat(240));
	let paletteShownFor: string | undefined;

	// ── 启动头部：极简，仅 ASCII 字形 + 登录状态 ──────────
	// 经由 emit 写入，这样它也在渲染器缓冲区里，整体重绘时不会被擦掉
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

	// 初始化滚动区域（此前无 anchor，banner 正常写在顶部），再挂上光标归位
	bar.set(defaultHint, statusLine(app));
	setOutputAnchor(bar.enabled ? () => bar.anchorCursor() : null);
	if (!bar.enabled) info(statusLine(app));

	/** 刷新底部状态栏；非 TTY 时降级为一次性打印状态行 */
	const refreshUi = (): void => {
		if (bar.enabled) bar.set(defaultHint, statusLine(app));
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

	/** 切换忙碌状态（状态栏高亮 + 每秒刷新已耗时） */
	const setBusy = (next: boolean): void => {
		busy = next;
		stopBusyTimer();
		if (next) busyStartedAt = Date.now();
		bar.set(defaultHint, statusLine(app, busy, next ? 0 : undefined));
		if (next && bar.enabled) {
			busyTimer = setInterval(() => {
				bar.set(defaultHint, statusLine(app, true, Date.now() - busyStartedAt));
			}, 1000);
		}
	};

	/** 生成状态栏文本（busy 时带上已耗时） */
	const barStatus = (): string =>
		statusLine(app, busy, busy ? Date.now() - busyStartedAt : undefined);

	const showPalette = (): void => {
		// 先清掉输入行 → 把命令面板打印到主输出区 → 再重绘输入行
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

	/** 输入以 ! 开头时切换到 shell 提示符，让模式一眼可见 */
	const syncPrompt = (): void => {
		editor.setPrompt(editor.line.startsWith("!") ? PROMPT_SHELL : PROMPT_CHAT);
	};

	const toggle = (kind: "thinking" | "search"): void => {
		if (kind === "thinking") app.setThinking(!app.config.thinking);
		else app.setSearch(!app.config.search);
		bar.set(defaultHint, barStatus());
		editor.redraw();
	};

	/** 展开 / 折叠思考内容；展开时回放最近一次思考全文 */
	const toggleThinkingView = (): void => {
		const next = !app.config.showThinking;
		app.setShowThinking(next);
		editor.erase();
		emit.line("");
		emit.line(
			`  ${color.dim(
				t("toggle.thinkingView", { state: next ? t("status.on") : t("status.off") }),
			)}`,
		);
		if (next) {
			const body = thinkBody(ui.lastThinking);
			if (body.length === 0) emit.line(color.dim(`  ${t("repl.noThinking")}`));
			else for (const line of body) emit.line(line);
		}
		editor.redraw();
		bar.set(defaultHint, barStatus());
	};

	/** 鼠标点击：命中可折叠块头则切换折叠 */
	const onMouse = (event: MouseEvent): void => {
		if (event.release || event.button !== 0 || !render) return;
		// 面板两行不属于主输出
		if (event.y > bar.height()) return;
		const block = render.blockAtRow(event.y);
		if (!block) return;
		editor.erase();
		render.toggle(block);
		bar.refresh();
		editor.redraw();
	};

	const editor = new LineEditor({
		prompt: PROMPT_CHAT,
		// 输入中：拦截快捷键；其余交给编辑器，稍后刷新补全提示
		onKey: (key: EditorKey): boolean => {
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
			setImmediate(() => {
				syncPrompt();
				updateHint();
			});
			return false;
		},
		// 非输入状态（流式输出中）：只处理中断与开关。
		// 这里刻意不处理 Ctrl+O —— 展开需要排版输出，不能与流式写入并发。
		onIdleKey: (key: EditorKey): void => {
			if (key.ctrl && key.name === "c") {
				activeAbort?.abort();
				return;
			}
			if (key.ctrl && key.name === "t") toggle("thinking");
			else if (key.ctrl && key.name === "s") toggle("search");
		},
		onMouse,
	});
	// 开启鼠标跟踪，让「点击块头折叠」可用（退出时必须关闭，否则会吞掉拖选）
	if (bar.enabled) enableMouse();
	refreshUi();

	// ── 尺寸变化 ──────────────────────────────────────────
	// 顺序很重要：handleResize 重建滚动区域并重新锚定光标，
	// 之后按新宽度整体重排可见窗口，最后重绘输入行。
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
				refreshUi();
				continue;
			}

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
				refreshUi();
				continue;
			}

			// 普通对话
			const controller = new AbortController();
			activeAbort = controller;
			setBusy(true);
			try {
				await runTurn(app, trimmed, createTurnIO(app, ui, emit, render !== null), controller.signal);
			} catch (e) {
				if (controller.signal.aborted) {
					emit.line(color.dim(`  ${t("repl.stopped")}`));
				} else {
					// token 失效时清掉本地凭证，避免"看起来已登录、实际用不了"
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
