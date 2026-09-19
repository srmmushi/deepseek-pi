// 交互式 REPL —— Agent 风格界面
//
// 屏幕布局（自上而下）：
//   1. 启动头部：ASCII 字形 + 会话/配置目录/凭证信息
//   2. 主输出区：对话流、思考、工具调用（滚动区域 1..rows-2）
//   3. 输入行：❯ ...（由滚动区域底部承载，滚动区域底部 = rows-2）
//   4. 提示行：分隔线，输入 / 时变为命令补全
//   5. 状态栏：会话 · 模型 · 思考 · 搜索 · 语言   ← 固定在屏幕最底部
//
// 底部两行用终端滚动区域钉住；输入行由自研编辑器维护，绝不越界擦除。
import type { App } from "../app.js";
import { isAuthError } from "../deepseek/provider.js";
import { describeCall } from "../tools/index.js";
import { printBanner } from "../ui/banner.js";
import { LineEditor, type EditorKey } from "../ui/line-editor.js";
import { color, error as printError, info } from "../ui/output.js";
import { formatHint, printCommandPalette } from "../ui/palette.js";
import { createStatusBar } from "../ui/statusbar.js";
import { padTo } from "../ui/text.js";
import { suggestCommands } from "./command-specs.js";
import { handleCommand } from "./commands.js";
import { describeError, runTurn, type TurnIO } from "./loop.js";

/** 状态栏文本：会话 · 模型 · 思考 · 搜索 · 语言（busy 时高亮提示正在生成） */
export function statusLine(app: App, busy = false): string {
	const { t } = app.i18n;
	const flag = (value: boolean): string =>
		value ? color.green(t("status.on")) : color.dim(t("status.off"));
	const marker = busy ? color.yellow("◆") : color.cyan("◆");
	const title = busy
		? `${color.yellow(app.activeSession.title)} ${color.yellow(`(${t("status.busy")}…)`)}`
		: color.cyan(app.activeSession.title);
	return [
		`${marker} ${title}`,
		color.bold(app.getModel().id),
		`${t("status.thinking")} ${flag(app.config.thinking)}`,
		`${t("status.search")} ${flag(app.config.search)}`,
		color.dim(app.config.language),
	].join(color.dim("  ·  "));
}

/** 构造本轮的渲染回调（Agent 风格：⏺ 工具 / ⎿ 结果 / ✻ 思考） */
function createTurnIO(app: App): TurnIO {
	const { t } = app.i18n;
	return {
		onThinkStart() {
			process.stdout.write(`\n  ${color.dim(`✻ ${t("repl.thinkingLabel")}…`)}\n  `);
		},
		onThinkDelta(text) {
			process.stdout.write(color.dim(text));
		},
		onThinkEnd() {
			process.stdout.write("\n");
		},
		onContentStart() {
			process.stdout.write("\n");
		},
		onContentDelta(text) {
			process.stdout.write(text);
		},
		onToolStart(call) {
			info(
				`\n  ${color.magenta("⏺")} ${color.bold(call.name)}${color.dim(`(${describeCall(call)})`)}`,
			);
		},
		onToolEnd(_call, result) {
			const detail = result.ok ? color.dim(result.summary) : color.red(result.summary);
			info(`  ${color.dim("⎿")}  ${detail}`);
		},
		onDone(_finishReason, usage) {
			if (usage != null) info(`\n  ${color.dim(`· ${usage} ${t("status.tokens")}`)}`);
		},
		onNotice(text) {
			info(`  ${color.yellow(`⚠ ${text}`)}`);
		},
	};
}

/** 启动 REPL 主循环 */
export async function startRepl(app: App): Promise<void> {
	const { t } = app.i18n;

	// ── 启动头部：ASCII 字形与信息并排 ────────────────────
	const token = app.getToken();
	const label = (text: string): string => color.dim(padTo(text, 12));
	const stateText = token
		? color.green(`✓ ${t("ui.credLoaded", { len: token.length })}`)
		: color.yellow(t("ui.credMissing"));
	printBanner([
		color.bold("pi-deepseek-web"),
		t("app.tagline"),
		color.dim("─────────────────────────────"),
		`${label(t("ui.session"))}${color.cyan(app.activeSession.title)}`,
		`${label(t("ui.dir"))}${color.dim(app.paths.configDir)}`,
		`${label(t("ui.cred"))}${stateText}`,
	]);
	info(`  ${color.dim(t("ui.tips"))}`);
	info();

	// ── 底部状态栏 ────────────────────────────────────────
	const bar = createStatusBar();
	// 默认提示行是一条分隔线（把输入区与状态栏隔开）；输入 / 时替换为补全项
	const defaultHint = color.dim("─".repeat(240));
	let paletteShownFor: string | undefined;

	/** 刷新底部状态栏；非 TTY 时降级为一次性打印状态行 */
	const refreshUi = (): void => {
		if (bar.enabled) bar.set(defaultHint, statusLine(app));
	};

	// ── 自研行编辑器 ──────────────────────────────────────
	let activeAbort: AbortController | null = null;
	let busy = false;

	/** 切换忙碌状态（状态栏高亮） */
	const setBusy = (next: boolean): void => {
		busy = next;
		bar.set(defaultHint, statusLine(app, busy));
	};

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
			bar.set(formatHint(suggestCommands(line, app.i18n.lang), app.i18n.lang), statusLine(app));
			if (line === "/" && paletteShownFor !== "/") {
				paletteShownFor = "/";
				showPalette();
			} else if (line !== "/") {
				paletteShownFor = undefined;
			}
			return;
		}
		paletteShownFor = undefined;
		bar.set(defaultHint, statusLine(app));
	};

	const toggle = (kind: "thinking" | "search"): void => {
		if (kind === "thinking") app.setThinking(!app.config.thinking);
		else app.setSearch(!app.config.search);
		bar.set(defaultHint, statusLine(app));
		editor.redraw();
	};

	const editor = new LineEditor({
		prompt: color.cyan("❯ "),
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
			setImmediate(updateHint);
			return false;
		},
		// 非输入状态（流式输出中）：只处理中断与开关
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
	process.stdout.on("resize", () => {
		bar.handleResize();
		bar.set(defaultHint, statusLine(app));
		editor.redraw();
	});

	// ── 退出清理 ──────────────────────────────────────────
	let cleaned = false;
	const cleanup = (): void => {
		if (cleaned) return;
		cleaned = true;
		editor.dispose();
		bar.dispose();
	};
	process.on("exit", cleanup);

	// ── 主循环 ────────────────────────────────────────────
	const io = { print: (text?: string) => info(text ?? "") };

	try {
		for (;;) {
			const input = await editor.readLine();
			if (input === null) break;

			const trimmed = input.trim();
			if (!trimmed) {
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
				await runTurn(app, trimmed, createTurnIO(app), controller.signal);
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
