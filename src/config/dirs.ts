// 配置目录解析 —— 支持环境变量 PI_CONFIG_DIR 与命令行参数 --config-dir
//
// 优先级（从高到低）：
//   1. 命令行参数 --config-dir <path>
//   2. 环境变量 PI_CONFIG_DIR
//   3. 默认 ~/.pi/agent
import { homedir } from "node:os";
import { join, resolve } from "node:path";

/** 单个目录解析结果，集中暴露所有派生路径 */
export interface ConfigPaths {
	/** 配置根目录 */
	configDir: string;
	/** 加密后的 token 存储 */
	authFile: string;
	/** 运行时配置（语言 / 开关 / UA 等） */
	configFile: string;
	/** 精简后的模型清单（单一 provider） */
	modelsFile: string;
	/** 可编辑的系统提示词 */
	systemPromptFile: string;
	/** 会话记录目录（每个会话一个 json） */
	sessionsDir: string;
}

/** 展开路径中的 ~ 为用户主目录 */
export function expandTilde(input: string): string {
	if (input === "~") return homedir();
	if (input.startsWith("~/") || input.startsWith("~\\")) {
		return join(homedir(), input.slice(2));
	}
	return input;
}

/** 计算配置根目录（不含派生文件） */
export function resolveConfigDir(cliOverride?: string): string {
	if (cliOverride && cliOverride.trim()) {
		return resolve(expandTilde(cliOverride.trim()));
	}
	const envDir = process.env.PI_CONFIG_DIR;
	if (envDir && envDir.trim()) {
		return resolve(expandTilde(envDir.trim()));
	}
	return join(homedir(), ".pi", "agent");
}

/** 由配置根目录派生全部文件路径 */
export function resolveConfigPaths(cliOverride?: string): ConfigPaths {
	const configDir = resolveConfigDir(cliOverride);
	return {
		configDir,
		authFile: join(configDir, "auth", "deepseek-web.json"),
		configFile: join(configDir, "config.json"),
		modelsFile: join(configDir, "models.json"),
		systemPromptFile: join(configDir, "system-prompt.md"),
		sessionsDir: join(configDir, "sessions"),
	};
}
