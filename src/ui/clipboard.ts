// 复制文本到系统剪贴板
//
// 两条路径：
//   1. 平台命令（win: clip / mac: pbcopy / linux: wl-copy|xclip|xsel）—— 可靠、无长度限制；
//   2. OSC 52 —— 由终端负责写入剪贴板，本地没有任何剪贴板工具时仍可用
//      （Windows Terminal / iTerm2 / kitty / wezterm 均支持）。
import { spawn } from "node:child_process";

/** 按平台给出候选命令 */
function candidates(): Array<[string, string[]]> {
	if (process.platform === "win32") return [["clip", []]];
	if (process.platform === "darwin") return [["pbcopy", []]];
	return [
		["wl-copy", []],
		["xclip", ["-selection", "clipboard"]],
		["xsel", ["--clipboard", "--input"]],
	];
}

/** 逐个尝试平台命令写入剪贴板 */
function copyViaCommand(text: string): Promise<boolean> {
	const list = candidates();
	return new Promise<boolean>((resolve) => {
		const attempt = (index: number): void => {
			if (index >= list.length) {
				resolve(false);
				return;
			}
			const [cmd, args] = list[index];
			let child;
			try {
				child = spawn(cmd, args, { stdio: ["pipe", "ignore", "ignore"], windowsHide: true });
			} catch {
				attempt(index + 1);
				return;
			}
			let settled = false;
			const next = (ok: boolean): void => {
				if (settled) return;
				settled = true;
				if (ok) resolve(true);
				else attempt(index + 1);
			};
			child.on("error", () => next(false));
			child.on("close", (code) => next(code === 0));
			child.stdin.on("error", () => next(false));
			child.stdin.end(text, "utf8");
		};
		attempt(0);
	});
}

/** 复制文本；全部途径都失败时返回 false */
export async function copyToClipboard(text: string): Promise<boolean> {
	if (!text) return false;
	if (await copyViaCommand(text)) return true;
	if (process.stdout.isTTY) {
		process.stdout.write(`\u001b]52;c;${Buffer.from(text, "utf8").toString("base64")}\u0007`);
		return true;
	}
	return false;
}
