// 命令元数据 —— 供 /help、命令面板与输入补全共用同一份数据源
import type { Lang } from "../i18n/index.js";

/** 单条命令定义 */
export interface CommandSpec {
	/** 命令名（含斜杠） */
	name: string;
	/** 参数占位（可选） */
	args?: string;
	/** 中文说明 */
	zh: string;
	/** 英文说明 */
	en: string;
}

/** 全部可用命令 */
export const COMMAND_SPECS: CommandSpec[] = [
	{ name: "/help", zh: "显示本帮助", en: "Show this help" },
	{ name: "/login", zh: "浏览器登录（默认 Edge），也可直接输入 login", en: "Browser login (Edge by default); bare `login` works too" },
	{ name: "/logout", zh: "清除本地登录凭证", en: "Remove local credentials" },
	{ name: "/new", zh: "新建会话（网页会话懒创建）", en: "New session (web session created lazily)" },
	{
		name: "/session",
		args: "[all|序号|id]",
		zh: "查看 / 列出 / 切换会话",
		en: "Show / list / switch sessions",
	},
	{ name: "/clear", zh: "重置当前会话上下文", en: "Reset current session context" },
	{ name: "/thinking", args: "[on|off]", zh: "深度思考开关（Ctrl+T）", en: "Toggle deep thinking (Ctrl+T)" },
	{ name: "/search", args: "[on|off]", zh: "智能搜索开关（Ctrl+S）", en: "Toggle smart search (Ctrl+S)" },
	{ name: "/model", args: "[id]", zh: "查看 / 切换模型", en: "Show / switch model" },
	{ name: "/lang", args: "[zh|en]", zh: "查看 / 切换界面语言", en: "Show / switch UI language" },
	{
		name: "/system-prompt",
		args: "[edit|reset]",
		zh: "查看 / 编辑系统提示词",
		en: "View / edit the system prompt",
	},
	{ name: "/status", zh: "查看配置并真实校验凭证", en: "Show config and verify the credential" },
	{ name: "/quit", zh: "退出", en: "Exit" },
];

/** 命令的完整写法（含参数占位） */
export function usageOf(spec: CommandSpec): string {
	return spec.args ? `${spec.name} ${spec.args}` : spec.name;
}

/** 取对应语言的说明 */
export function describeOf(spec: CommandSpec, lang: Lang): string {
	return lang === "zh" ? spec.zh : spec.en;
}

/** 按输入前缀筛选命令（用于输入 / 时的实时补全） */
export function suggestCommands(prefix: string, _lang: Lang): CommandSpec[] {
	const text = prefix.trim().toLowerCase();
	if (!text.startsWith("/")) return [];
	return COMMAND_SPECS.filter((spec) => spec.name.toLowerCase().startsWith(text));
}
