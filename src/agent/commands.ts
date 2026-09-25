// 斜杠命令实现
//
// 覆盖：/help /login /logout /thinking /search /thinking-view /model /lang
//       /system-prompt /new /session /clear /status /quit
import type { App } from "../app.js";
import { modelIds } from "../app.js";
import { loginInteractive } from "../auth/login.js";
import { normalizeLang } from "../i18n/index.js";
import { color } from "../ui/output.js";
import { tokenFingerprint, verifyToken } from "../auth/verify.js";
import { openSystemPromptInEditor, resetSystemPrompt } from "../prompt/system-prompt.js";
import { COMMAND_SPECS, describeOf, usageOf } from "./command-specs.js";
import type { SessionRecord } from "./session-store.js";

/** 命令输出接口（由 REPL 实现） */
export interface CommandIO {
	print(text?: string): void;
}

/** 命令处理结果 */
export interface CommandOutcome {
	/** 是否为已识别的命令 */
	handled: boolean;
	/** 请求退出 REPL */
	exit: boolean;
}

const NOT_HANDLED: CommandOutcome = { handled: false, exit: false };
const HANDLED: CommandOutcome = { handled: true, exit: false };
const EXIT: CommandOutcome = { handled: true, exit: true };

/** 解析 on/off 参数；无参数时返回 undefined（表示切换） */
function parseOnOff(arg: string): boolean | undefined {
	const v = arg.trim().toLowerCase();
	if (!v) return undefined;
	if (["on", "1", "true", "yes", "开", "开启"].includes(v)) return true;
	if (["off", "0", "false", "no", "关", "关闭"].includes(v)) return false;
	return undefined;
}

/** 帮助文本（与命令面板共用同一份命令元数据） */
function helpLines(app: App): string[] {
	const lang = app.i18n.lang;
	const lines = COMMAND_SPECS.map(
		(spec) => `${usageOf(spec).padEnd(30)}${describeOf(spec, lang)}`,
	);
	lines.push("");
	lines.push(
		lang === "zh"
			? "提示：直接输入 login / logout 也可（无需斜杠）；输入 / 可查看全部命令。"
			: "Tip: bare `login` / `logout` also work; type / to list all commands.",
	);
	return lines;
}

/** 网页会话展示文本 */
function webLabel(app: App, record: SessionRecord): string {
	return record.deepseekSessionId ?? app.i18n.t("session.webUnbound");
}

/** 展示单个会话的元信息 */
function metaLine(app: App, record: SessionRecord): string {
	return app.i18n.t("session.meta", {
		cwd: record.cwd,
		web: webLabel(app, record),
		mode: app.config.contextMode,
	});
}

/** 处理一条斜杠命令 */
export async function handleCommand(
	app: App,
	input: string,
	out: CommandIO,
): Promise<CommandOutcome> {
	const trimmed = input.trim();
	if (!trimmed.startsWith("/")) return NOT_HANDLED;

	const spaceIndex = trimmed.search(/\s/);
	const cmd = (spaceIndex === -1 ? trimmed : trimmed.slice(0, spaceIndex)).toLowerCase();
	const args = spaceIndex === -1 ? "" : trimmed.slice(spaceIndex).trim();
	const { t } = app.i18n;

	switch (cmd) {
		case "/help": {
			out.print(color.bold(t("cmd.helpTitle")));
			for (const line of helpLines(app)) out.print(`  ${line}`);
			out.print();
			out.print(color.dim(t("status.hint")));
			return HANDLED;
		}

		case "/login": {
			if (app.isLoggedIn()) out.print(color.dim(t("login.already")));
			try {
				const result = await loginInteractive(app);
				app.setAuth(result.token, result.userAgent);
				out.print(color.green(t("login.success", { path: app.paths.authFile })));
				if (result.userAgent) out.print(color.dim(t("login.ua", { ua: result.userAgent })));
			} catch (e) {
				out.print(color.red(t("login.failed", { error: (e as Error).message })));
			}
			return HANDLED;
		}

		case "/logout": {
			const removed = app.clearAuthStore();
			out.print(removed ? t("logout.done") : t("logout.none"));
			return HANDLED;
		}

		// ── 会话管理 ──────────────────────────────────────

		case "/new": {
			const record = app.newSession();
			out.print(
				color.green(`${t("session.new", { title: record.title })}\n${metaLine(app, record)}`),
			);
			return HANDLED;
		}

		case "/session":
		case "/sessions": {
			if (!args) {
				out.print(color.bold(t("session.current", { title: app.activeSession.title })));
				out.print(color.dim(metaLine(app, app.activeSession)));
				out.print(color.dim(t("session.hint")));
				return HANDLED;
			}

			if (args === "all" || args === "-a" || args === "list") {
				const records = app.listSessions();
				if (records.length === 0) {
					out.print(t("session.empty"));
					return HANDLED;
				}
				out.print(color.bold(t("session.listTitle", { count: records.length })));
				records.forEach((record, index) => {
					const marker = record.id === app.activeSession.id ? color.green(" *") : "";
					out.print(
						`${t("session.row", {
							index: index + 1,
							title: record.title,
							id: record.id.slice(0, 8),
						})}${marker}`,
					);
					out.print(
						color.dim(
							t("session.rowMeta", {
								cwd: record.cwd,
								web: record.deepseekSessionId
									? record.deepseekSessionId.slice(0, 8)
									: t("session.webUnbound"),
								updated: new Date(record.updatedAt).toLocaleString(),
							}),
						),
					);
				});
				out.print(color.dim(t("session.hint")));
				return HANDLED;
			}

			// 切换会话：同时切换工作目录与网页会话
			const result = app.switchSession(args);
			if (result.ok) {
				out.print(
					color.green(
						t("session.switched", {
							title: result.record.title,
							cwd: result.record.cwd,
							web: webLabel(app, result.record),
						}),
					),
				);
			} else if (result.reason === "cwdMissing") {
				out.print(color.yellow(t("session.cwdMissing", { cwd: app.activeSession.cwd })));
				out.print(color.dim(t("session.current", { title: app.activeSession.title })));
			} else {
				out.print(color.red(t("session.notFound", { name: args })));
			}
			return HANDLED;
		}

		// ── 配置 ──────────────────────────────────────────

		case "/thinking": {
			const parsed = parseOnOff(args);
			if (args && parsed === undefined) {
				out.print(t("toggle.unknownArg"));
				return HANDLED;
			}
			const next = parsed ?? !app.config.thinking;
			app.setThinking(next);
			out.print(t("toggle.thinking", { state: next ? t("status.on") : t("status.off") }));
			return HANDLED;
		}

		case "/search": {
			const parsed = parseOnOff(args);
			if (args && parsed === undefined) {
				out.print(t("toggle.unknownArg"));
				return HANDLED;
			}
			const next = parsed ?? !app.config.search;
			app.setSearch(next);
			out.print(t("toggle.search", { state: next ? t("status.on") : t("status.off") }));
			return HANDLED;
		}

		case "/thinking-view":
		case "/think-view": {
			const parsed = parseOnOff(args);
			if (args && parsed === undefined) {
				out.print(t("toggle.unknownArg"));
				return HANDLED;
			}
			const next = parsed ?? !app.config.showThinking;
			app.setShowThinking(next);
			out.print(
				t("toggle.thinkingView", { state: next ? t("status.on") : t("status.off") }),
			);
			return HANDLED;
		}

		case "/model": {
			const list = t("cmd.modelList", {
				models: modelIds().join(", "),
				current: app.getModel().id,
			});
			if (!args) {
				out.print(list);
				return HANDLED;
			}
			out.print(app.setModel(args) ? t("toggle.model", { model: app.getModel().id }) : list);
			return HANDLED;
		}

		case "/lang": {
			if (!args) {
				out.print(`${t("status.lang")}: ${app.config.language}`);
				return HANDLED;
			}
			const lang = normalizeLang(args);
			if (!lang) {
				out.print("zh | en");
				return HANDLED;
			}
			app.setLanguage(lang);
			out.print(app.i18n.t("cmd.langSet", { lang }));
			return HANDLED;
		}

		case "/system-prompt": {
			const sub = args.toLowerCase();
			if (sub === "edit") {
				const ok = await openSystemPromptInEditor(app.paths);
				out.print(
					ok
						? t("sp.edited", { path: app.paths.systemPromptFile })
						: color.yellow(t("sp.current", { path: app.paths.systemPromptFile })),
				);
				return HANDLED;
			}
			if (sub === "reset") {
				resetSystemPrompt(app.paths, app.config.language);
				out.print(t("sp.reset"));
				return HANDLED;
			}
			out.print(t("sp.current", { path: app.paths.systemPromptFile }));
			out.print("────────────────────────────────");
			out.print(app.getSystemPrompt());
			out.print("────────────────────────────────");
			return HANDLED;
		}

		case "/clear": {
			// 重置本地上下文并解绑网页会话（下次发送会新建网页会话）
			app.resetSessionContext();
			out.print(t("cmd.clear"));
			return HANDLED;
		}

		case "/status": {
			out.print(
				t("cmd.status", {
					dir: app.paths.configDir,
					model: app.getModel().id,
					thinking: app.config.thinking ? t("status.on") : t("status.off"),
					search: app.config.search ? t("status.on") : t("status.off"),
					lang: app.config.language,
					mode: app.config.contextMode,
				}),
			);
			out.print(color.dim(metaLine(app, app.activeSession)));

			// 凭证诊断：文件是否存在 ≠ token 是否有效，这里做一次真实校验
			const token = app.getToken();
			if (!token) {
				out.print(color.yellow(t("auth.fileMissing")));
				return HANDLED;
			}
			out.print(t("auth.filePresent", { path: app.paths.authFile }));
			out.print(t("auth.tokenInfo", { len: token.length, fp: tokenFingerprint(token) }));
			out.print(color.dim(t("auth.verifying")));
			const result = await verifyToken(app.config, token);
			if (result.ok) {
				out.print(color.green(t("auth.verified")));
			} else {
				out.print(color.red(t("auth.verifyFailed", { error: result.error ?? "unknown" })));
				out.print(color.yellow(t("auth.reloginHint")));
			}
			return HANDLED;
		}

		case "/quit":
		case "/exit":
			return EXIT;

		default:
			out.print(color.yellow(t("cmd.unknown", { name: cmd })));
			return HANDLED;
	}
}
