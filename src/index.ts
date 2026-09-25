#!/usr/bin/env node
// CLI 入口
//
// 用法：
//   dsp                 进入交互模式
//   dsp /login          直接执行命令后退出
//   dsp -c <dir>        指定配置目录
import { handleCommand } from "./agent/commands.js";
import { startRepl } from "./agent/repl.js";
import { App } from "./app.js";
import { helpText, parseArgs } from "./cli/args.js";
import { color, error, info } from "./ui/output.js";

async function main(): Promise<void> {
	const args = parseArgs(process.argv.slice(2));

	if (args.help) {
		info(helpText());
		return;
	}

	const app = await App.create(args.configDir);

	// 单命令模式：执行后立即退出（例如 `dsp /login`）
	if (args.command) {
		const outcome = await handleCommand(app, args.command, { print: (text) => info(text ?? "") });
		if (!outcome.handled) {
			error(`未知命令：${args.command}`);
			process.exitCode = 1;
		}
		return;
	}

	await startRepl(app);
}

main().catch((e: unknown) => {
	error(color.red(`启动失败：${e instanceof Error ? e.message : String(e)}`));
	process.exitCode = 1;
});
