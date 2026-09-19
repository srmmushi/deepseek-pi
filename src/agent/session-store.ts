// 会话持久化 —— pi 会话与 DeepSeek 网页会话 1:1 绑定
//
// 存储位置：<configDir>/sessions/<id>.json
//
// 关键设计：
//   1. **懒创建**：新建 pi 会话时不会立刻在网页端建会话，
//      直到用户第一次真正发送提示词，才调用 /chat_session/create，
//      并把返回的 id 写入 deepseekSessionId。
//   2. **parentMessageId**：复用网页会话时，用它链式追加消息，
//      上下文由服务端维护，无需每轮重发历史。
import { randomUUID } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { ChatMessage } from "../deepseek/prompt.js";

/** 单个会话记录 */
export interface SessionRecord {
	/** 本地会话 id */
	id: string;
	/** 展示用标题（取首条用户输入前若干字符） */
	title: string;
	/** 该会话绑定的工作目录 */
	cwd: string;
	/** DeepSeek 网页会话 id；null 表示尚未创建（懒创建） */
	deepseekSessionId: string | null;
	/** 下一轮 completion 使用的 parent_message_id */
	parentMessageId: number | null;
	/** 本地消息历史（用于界面展示与 replay 回退） */
	messages: ChatMessage[];
	createdAt: number;
	updatedAt: number;
}

/** 会话标题最大长度 */
const TITLE_MAX = 40;

/** 由首条用户输入生成标题 */
export function deriveTitle(input: string): string {
	const firstLine = input.trim().split(/\r?\n/)[0] ?? "";
	const title = firstLine.slice(0, TITLE_MAX);
	return title || "新会话";
}

/** 会话集合的磁盘存储 */
export class SessionStore {
	constructor(private readonly dir: string) {}

	private fileOf(id: string): string {
		return join(this.dir, `${id}.json`);
	}

	private ensureDir(): void {
		if (!existsSync(this.dir)) mkdirSync(this.dir, { recursive: true });
	}

	/** 读取单个会话 */
	load(id: string): SessionRecord | undefined {
		const file = this.fileOf(id);
		if (!existsSync(file)) return undefined;
		try {
			return JSON.parse(readFileSync(file, "utf8")) as SessionRecord;
		} catch {
			return undefined;
		}
	}

	/** 写入会话（覆盖） */
	save(record: SessionRecord): void {
		this.ensureDir();
		record.updatedAt = Date.now();
		writeFileSync(this.fileOf(record.id), `${JSON.stringify(record, null, "\t")}\n`, "utf8");
	}

	/** 列出全部会话（按更新时间倒序） */
	list(): SessionRecord[] {
		if (!existsSync(this.dir)) return [];
		const records: SessionRecord[] = [];
		for (const name of readdirSync(this.dir)) {
			if (!name.endsWith(".json")) continue;
			const id = name.slice(0, -".json".length);
			const record = this.load(id);
			if (record) records.push(record);
		}
		return records.sort((a, b) => b.updatedAt - a.updatedAt);
	}

	/** 新建会话（不创建网页会话） */
	create(cwd: string, title = "新会话"): SessionRecord {
		const now = Date.now();
		const record: SessionRecord = {
			id: randomUUID(),
			title,
			cwd,
			deepseekSessionId: null,
			parentMessageId: null,
			messages: [],
			createdAt: now,
			updatedAt: now,
		};
		this.save(record);
		return record;
	}

	/** 删除会话文件 */
	remove(id: string): boolean {
		const file = this.fileOf(id);
		if (!existsSync(file)) return false;
		rmSync(file, { force: true });
		return true;
	}

	/** 按 id 前缀查找（便于命令行输入短 id） */
	findByPrefix(prefix: string): SessionRecord | undefined {
		const all = this.list();
		const exact = all.find((s) => s.id === prefix);
		if (exact) return exact;
		const matches = all.filter((s) => s.id.startsWith(prefix));
		return matches.length === 1 ? matches[0] : undefined;
	}
}
