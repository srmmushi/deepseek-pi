// 命令行参数解析
import { APP_FULL_NAME, APP_NAME } from "../ui/banner.js";

export interface CliArgs {
	/** --config-dir / -c 指定的配置目录 */
	configDir?: string;
	/** --help / -h */
	help: boolean;
	/** 直接执行的斜杠命令（如 `/login`），执行后退出 */
	command?: string;
}

/** 解析 argv（不含 node 与脚本路径） */
export function parseArgs(argv: string[]): CliArgs {
	const result: CliArgs = { help: false };
	for (let i = 0; i < argv.length; i += 1) {
		const arg = argv[i];
		if (arg === "--help" || arg === "-h") {
			result.help = true;
			continue;
		}
		if (arg === "--config-dir" || arg === "-c") {
			const next = argv[i + 1];
			if (next) {
				result.configDir = next;
				i += 1;
			}
			continue;
		}
		if (arg.startsWith("--config-dir=")) {
			result.configDir = arg.slice("--config-dir=".length);
			continue;
		}
		if (arg.startsWith("/")) {
			// 斜杠命令：把其后的参数一并带上
			result.command = argv.slice(i).join(" ");
			break;
		}
	}
	return result;
}

/** 帮助文本 */
export function helpText(): string {
	const cmd = APP_NAME.toLowerCase();
	return [
		`${APP_NAME} (${APP_FULL_NAME}) —— 终端编程助手，仅使用 DeepSeek 网页版`,
		"",
		"用法：",
		`  ${cmd} [--config-dir <path>]`,
		`  ${cmd} /login            # 直接执行命令后退出`,
		"",
		"选项：",
		"  -c, --config-dir <path>   指定配置目录（默认 ~/.pi/agent，也可用环境变量 PI_CONFIG_DIR）",
		"  -h, --help                显示本帮助",
		"",
		"首次使用：",
		`  1) ${cmd} /login   在浏览器中登录，自动抓取并加密保存 token`,
		`  2) ${cmd}          进入交互模式开始对话`,
	].join("\n");
}
