// 文件系统类工具：write / read / list / search
import { promises as fs } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { formatBytes } from "../ui/text.js";
import type { ToolContext, ToolResult } from "./types.js";

/** 单次读取的最大字符数 */
const MAX_READ_CHARS = 120_000;
/** list 最多返回的条目数 */
const MAX_LIST_ENTRIES = 500;
/** 递归搜索最多扫描的文件数 */
const MAX_SEARCH_FILES = 6000;
/** 搜索最多返回的匹配数 */
const MAX_SEARCH_MATCHES = 200;
/** 单个文件参与搜索的最大字节数 */
const MAX_SEARCH_FILE_BYTES = 1_500_000;
/** 搜索时跳过的目录 */
const SKIP_DIRS = new Set([
	"node_modules",
	".git",
	"dist",
	"build",
	"out",
	".next",
	".nuxt",
	".cache",
	"vendor",
	"target",
	".venv",
	"venv",
	"__pycache__",
	".idea",
	".vscode",
]);

/** 解析为绝对路径（相对路径基于工作目录） */
function toAbsolute(cwd: string, p: string): string {
	if (isAbsolute(p)) return p;
	return resolve(cwd, p);
}

/** 展示用的相对路径 */
function display(cwd: string, abs: string): string {
	const rel = relative(cwd, abs);
	if (!rel) return ".";
	return rel.split(sep).join("/");
}

/** 写入文件（一次性完整覆盖，自动创建父目录） */
export async function runWrite(
	args: { content: string; path: string },
	ctx: ToolContext,
): Promise<ToolResult> {
	const abs = toAbsolute(ctx.cwd, args.path);
	try {
		await fs.mkdir(dirname(abs), { recursive: true });
		await fs.writeFile(abs, args.content, "utf8");
		const bytes = Buffer.byteLength(args.content, "utf8");
		return {
			ok: true,
			output: ctx.i18n.t("tool.writeDone", { path: args.path, bytes }),
			summary: ctx.i18n.t("tool.sumWrite", { path: args.path, size: formatBytes(bytes) }),
		};
	} catch (e) {
		return { ok: false, output: `write 失败：${(e as Error).message}`, summary: args.path };
	}
}

/** 读取文件内容 */
export async function runRead(args: { path: string }, ctx: ToolContext): Promise<ToolResult> {
	const abs = toAbsolute(ctx.cwd, args.path);
	try {
		const stat = await fs.stat(abs);
		if (stat.isDirectory()) {
			return {
				ok: false,
				output: `${args.path} 是目录，请使用 list`,
				summary: args.path,
			};
		}
		const buffer = await fs.readFile(abs);
		if (buffer.includes(0)) {
			return { ok: false, output: `${args.path} 疑似二进制文件，无法读取`, summary: args.path };
		}
		let text = buffer.toString("utf8");
		// 行数按截断前的完整内容统计
		const lineCount = text.split("\n").length;
		let truncated = false;
		if (text.length > MAX_READ_CHARS) {
			text = text.slice(0, MAX_READ_CHARS);
			truncated = true;
		}
		const header = `[read] ${args.path}\n`;
		const footer = truncated ? `\n${ctx.i18n.t("tool.maxOutput", { limit: MAX_READ_CHARS })}` : "";
		return {
			ok: true,
			output: `${header}${text}${footer}`,
			summary: ctx.i18n.t("tool.sumRead", {
				path: args.path,
				lines: lineCount,
				size: formatBytes(buffer.byteLength),
			}),
		};
	} catch (e) {
		return { ok: false, output: `read 失败：${(e as Error).message}`, summary: args.path };
	}
}

/** 列出目录（单层） */
export async function runList(args: { path: string }, ctx: ToolContext): Promise<ToolResult> {
	const abs = toAbsolute(ctx.cwd, args.path || ".");
	try {
		const entries = await fs.readdir(abs, { withFileTypes: true });
		const rows: string[] = [];
		const sorted = entries.sort((a, b) => {
			if (a.isDirectory() !== b.isDirectory()) return a.isDirectory() ? -1 : 1;
			return a.name.localeCompare(b.name);
		});
		for (const entry of sorted.slice(0, MAX_LIST_ENTRIES)) {
			if (entry.isDirectory()) {
				rows.push(`dir   ${entry.name}/`);
			} else {
				let size = 0;
				try {
					size = (await fs.stat(join(abs, entry.name))).size;
				} catch {
					size = 0;
				}
				rows.push(`file  ${entry.name}  (${size}B)`);
			}
		}
		const more =
			entries.length > MAX_LIST_ENTRIES
				? `\n${ctx.i18n.t("tool.maxOutput", { limit: MAX_LIST_ENTRIES })}`
				: "";
		return {
			ok: true,
			output: `[list] ${display(ctx.cwd, abs)}\n${rows.join("\n")}${more}`,
			summary: ctx.i18n.t("tool.sumList", {
				path: display(ctx.cwd, abs),
				count: Math.min(entries.length, MAX_LIST_ENTRIES),
			}),
		};
	} catch (e) {
		return { ok: false, output: `list 失败：${(e as Error).message}`, summary: args.path };
	}
}

/** 递归搜索关键词（不区分大小写），返回 文件:行号: 内容 */
export async function runSearch(args: { query: string }, ctx: ToolContext): Promise<ToolResult> {
	const needle = args.query.toLowerCase();
	const matches: string[] = [];
	let scanned = 0;

	const stack: string[] = [ctx.cwd];
	while (stack.length > 0 && matches.length < MAX_SEARCH_MATCHES && scanned < MAX_SEARCH_FILES) {
		const dir = stack.pop() as string;
		const entries = await fs.readdir(dir, { withFileTypes: true }).catch(() => null);
		if (!entries) continue;

		for (const entry of entries) {
			if (matches.length >= MAX_SEARCH_MATCHES || scanned >= MAX_SEARCH_FILES) break;
			const full = join(dir, entry.name);
			if (entry.isDirectory()) {
				if (SKIP_DIRS.has(entry.name)) continue;
				stack.push(full);
				continue;
			}
			if (!entry.isFile()) continue;
			scanned += 1;
			const stat = await fs.stat(full).catch(() => null);
			if (!stat || stat.size > MAX_SEARCH_FILE_BYTES) continue;
			const buffer = await fs.readFile(full).catch(() => null);
			if (!buffer || buffer.includes(0)) continue; // 跳过不可读与二进制文件
			const lines = buffer.toString("utf8").split(/\r?\n/);
			for (let i = 0; i < lines.length; i += 1) {
				if (lines[i].toLowerCase().includes(needle)) {
					matches.push(`${display(ctx.cwd, full)}:${i + 1}: ${lines[i].trim().slice(0, 200)}`);
					if (matches.length >= MAX_SEARCH_MATCHES) break;
				}
			}
		}
	}

	if (matches.length === 0) {
		return {
			ok: true,
			output: `未找到包含 "${args.query}" 的内容`,
			summary: ctx.i18n.t("tool.sumSearchNone", { query: args.query }),
		};
	}
	const tail =
		matches.length >= MAX_SEARCH_MATCHES
			? `\n${ctx.i18n.t("tool.maxOutput", { limit: MAX_SEARCH_MATCHES })}`
			: "";
	return {
		ok: true,
		output: `[search] ${args.query}\n${matches.join("\n")}${tail}`,
		summary: ctx.i18n.t("tool.sumSearch", { query: args.query, count: matches.length }),
	};
}
