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
//   ▌ 工具调用      └ 工具结果      · 元信息
//
// ⚠ 列偏移陷阱（曾导致输出覆盖已有内容）：
//   raw 模式下 \n 只换行、不回列（Windows 上 libuv 会设 DISABLE_NEWLINE_AUTO_RETURN），
//   若某一行不先 \r 归位，列偏移会逐行累积，长行折行后越过滚动区域底边，
//   整屏就开始互相覆盖。因此：主输出统一走 info()，内容行自行补 \r，
//   并由 statusbar.anchorCursor() 在每行写入前把光标钉回「滚动区域底部第 1 列」。
import type { App } from "../app.js";
import { isAuthError } from "../deepseek/provider.js";
import { runShell } from "../tools/exec.js";
import { describeCall, type ToolCall, type ToolName } from "../tools/index.js";
import { APP_FULL_NAME, APP_NAME, printBanner } from "../ui/banner.js";
import { LineEditor, type EditorKey } from "../ui/line-editor.js";
import {
	color,
	endLiveLine,
	error as printError,
	info,
	setOutputAnchor,
	writeLiveLine,
} from "../ui/output.js";
import { formatHint, printCommandPalette } from "../ui/palette.js";
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

/** 思考回放最多显示的行数（防止一次刷屏） */
const MAX_THINK_REPLAY_LINES = 300;

/** 按工具类型配色，一眼区分「读 / 写 / 列目录 / 执行 / 搜索」 */
const TOOL_COLOR: Record<ToolName, (text: string) => string> = {
	read: color.cyan,
	write: color.green,
	list: color.blue,
	exec: color.yellow,
	search: color.magenta,
};

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

/** 执行 `!命令`：直接跑 shell，输出只打印不进对话上下文 */
async function runShellLine(app: App, command: string): Promise<void> {
	const { t } = app.i18n;
	if (!command) {
		info(`  ${color.dim(t("shell.usage"))}`);
		return;
	}
	info();
	info(`  ${color.yellow(GLYPH_CALL)} ${color.yellow(command)}`);
	const result = await runShell(command, app.cwd);
	if (result.output) {
		for (const line of result.output.split("\n")) info(`  ${line}`);
	} else {
		info(color.dim(`  ${t("shell.noOutput")}`));
	}
	const state = result.ok
		? color.green(t("shell.exit", { code: 0 }))
		: color.red(t("shell.exit", { code: result.code }));
	info(alignRight(`  ${color.dim(GLYPH_RESULT)} ${state}`, color.dim(formatDuration(result.durationMs))));
}

/** 构造本轮的渲染回调（Agent 风格：▌ 工具 / └ 结果 / 折叠思考） */
function createTurnIO(app: App, ui: UiState): TurnIO {
	const { t } = app.i18n;
	const turnStartedAt = Date.now();
	/** 当前并行批次 */
	let batch: ToolCall[] = [];
	let batchStartedAt = turnStartedAt;
	let batchDone = 0;
	/** 本次思考的显示状态 */
	let thinkFolded = true;
	let thinkChars = 0;
	let thinkPaintedAt = 0;

	return {
		// ── 思考：折叠时只维护一行实时指示，展开时逐字流式 ──
		onThinkStart() {
			thinkChars = 0;
			thinkPaintedAt = 0;
			thinkFolded = !app.config.showThinking;
			if (!thinkFolded) {
				info();
				process.stdout.write(`  ${color.dim(GLYPH_CALL)} ${color.dim(t("repl.thinkingLabel"))}  `);
			}
		},
		onThinkDelta(text) {
			thinkChars += text.length;
			if (!thinkFolded) {
				process.stdout.write(color.gray(text));
				return;
			}
			// 折叠态：节流刷新单行指示，避免逐字重绘造成的闪烁
			const now = Date.now();
			if (now - thinkPaintedAt < 100) return;
			thinkPaintedAt = now;
			writeLiveLine(
				`  ${color.dim(GLYPH_CALL)} ${color.dim(t("repl.thinkingLive", { chars: thinkChars }))}`,
			);
		},
		onThinkEnd(fullText, elapsedMs) {
			ui.lastThinking = fullText;
			const summary = t("repl.thinkingDone", {
				sec: formatDuration(elapsedMs),
				chars: fullText.length,
			});
			const hint = t(thinkFolded ? "repl.expand" : "repl.collapse");
			if (thinkFolded) {
				// 折叠：把活动行就地定稿为一行摘要
				endLiveLine(`  ${color.dim(GLYPH_CALL)} ${color.dim(summary)} ${color.dim(`· ${hint}`)}`);
				return;
			}
			process.stdout.write("\r\n");
			info(`  ${color.dim(GLYPH_RESULT)} ${color.dim(`${summary} · ${hint}`)}`);
		},

		// ── 正文 ──
		onContentStart() {
			info();
		},
		onContentDelta(text) {
			// 每个内容行都先 \r 归位：raw 模式下 \n 不回列，不归位会逐行右移
			process.stdout.write(`\r${text}`);
		},

		// ── 工具：一批调用先列出，结果按完成顺序打印 ──
		onToolBatchStart(calls) {
			batch = calls;
			batchDone = 0;
			batchStartedAt = Date.now();
			info();
			for (const call of calls) info(toolCallLine(call));
		},
		onToolEnd(call, result, elapsedMs) {
			batchDone += 1;
			// 并行批次里结果顺序与调用顺序不一致，必须带工具名才好对应
			const prefix =
				batch.length > 1 ? color.dim(call.name.padEnd(TOOL_NAME_WIDTH)) : "";
			info(toolResultLine(prefix, result.summary, result.ok, elapsedMs));
			if (batchDone >= batch.length && batch.length > 1) {
				const span = formatDuration(Date.now() - batchStartedAt);
				info(`  ${color.dim(`${GLYPH_META} ${t("repl.parallel", { n: batch.length })}  ·  ${span}`)}`);
			}
		},

		onDone(_finishReason, usage) {
			const parts: string[] = [];
			if (usage != null) parts.push(`${usage} ${t("status.tokens")}`);
			parts.push(formatDuration(Date.now() - turnStartedAt));
			info(`\n  ${color.dim(`${GLYPH_META} ${parts.join("  ·  ")}`)}`);
		},
		onNotice(text) {
			info(`  ${color.yellow(`! ${text}`)}`);
		},
	};
}

/** 启动 REPL 主循环 */
export async function startRepl(app: App): Promise<void> {
	const { t } = app.i18n;
	const ui: UiState = { lastThinking: "" };

	// ── 启动头部：极简，仅 ASCII 字形 + 登录状态 ──────────
	const token = app.getToken();
	const stateText = token
		? color.green(`✓ ${t("ui.loginOk", { len: token.length })}`)
		: color.yellow(`✗ ${t("ui.loginMissing")}`);
	printBanner([
		"",
		`${color.bold(APP_NAME)}  ${color.dim(`(${APP_FULL_NAME})`)}`,
		stateText,
	]);

	// ── 底部状态栏 ────────────────────────────────────────
	const bar = createStatusBar();
	// 主输出每行写入前，把光标钉回滚动区域底部第 1 列（防列偏移累积 / 防越界）
	setOutputAnchor(bar.enabled ? () => bar.anchorCursor() : null);
	// 默认提示行是一条分隔线（把输入区与状态栏隔开）；输入 / 时替换为补全项
	const defaultHint = color.dim("─".repeat(240));
	let paletteShownFor: string | undefined;

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
		printCommandPalette(app.i18n.lang);
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
		info();
		info(
			`  ${color.dim(
				t("toggle.thinkingView", { state: next ? t("status.on") : t("status.off") }),
			)}`,
		);
		if (next) {
			const text = ui.lastThinking;
			if (!text) {
				info(color.dim(`  ${t("repl.noThinking")}`));
			} else {
				info();
				const lines = text.split("\n");
				const shown = lines.slice(0, MAX_THINK_REPLAY_LINES);
				for (const line of shown) info(color.gray(`  ${line}`));
				if (lines.length > shown.length) {
					info(color.dim(`  ${t("repl.expanded")}`));
				}
			}
		}
		editor.redraw();
		bar.set(defaultHint, barStatus());
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
	});
	refreshUi();
	if (!bar.enabled) info(statusLine(app));

	// ── 尺寸变化 ──────────────────────────────────────────
	// 顺序很重要：handleResize 会重建滚动区域并把光标重新锚定到底部，
	// 之后才能安全地重算提示行与重绘输入行（否则输入行会漂到旧位置）。
	process.stdout.on("resize", () => {
		bar.handleResize();
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
		editor.dispose();
		bar.dispose();
	};
	process.on("exit", cleanup);

	// ── 主循环 ────────────────────────────────────────────
	const io = { print: (text?: string) => info(text ?? "") };

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
				await runShellLine(app, trimmed.slice(1).trim());
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
				await runTurn(app, trimmed, createTurnIO(app, ui), controller.signal);
			} catch (e) {
				if (controller.signal.aborted) {
					info(color.dim(`  ${t("repl.stopped")}`));
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

	info();
	info(color.dim(`  ${t("app.bye")}`));
}
