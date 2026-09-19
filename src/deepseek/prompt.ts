// DeepSeek 原生 ChatML 提示词构建
//
// 对齐上游参考实现的请求提示词构建：
//   - 角色标记使用全角竖线 <｜Role｜>；
//   - user 轮次前置 <｜end▁of▁sentence｜>；
//   - 工具结果用 <｜tool▁outputs▁begin｜> / <｜tool▁output▁begin｜> 包裹；
//   - 末尾补一个不闭合的 <｜Assistant｜> 作为生成起点。
export type ChatRole = "system" | "user" | "assistant" | "tool";

export interface ChatMessage {
	role: ChatRole;
	content: string;
}

const TAG_START = "<｜";
const TAG_END = "｜>";
const EOS = "<｜end▁of▁sentence｜>";

/** 生成角色标签（首字母大写） */
function roleTag(role: string): string {
	const r = role.charAt(0).toUpperCase() + role.slice(1);
	return `${TAG_START}${r}${TAG_END}`;
}

/** 合并连续同角色消息，避免模型对连续同角色标签产生混淆 */
function mergeMessages(messages: ChatMessage[]): ChatMessage[] {
	const merged: ChatMessage[] = [];
	for (const msg of messages) {
		const last = merged[merged.length - 1];
		if (last && last.role === msg.role && msg.role !== "tool") {
			last.content += `\n${msg.content}`;
			continue;
		}
		merged.push({ ...msg });
	}
	return merged;
}

function formatMessage(msg: ChatMessage): string {
	if (msg.role === "user") return `${EOS}${roleTag("user")}${msg.content}`;
	return `${roleTag(msg.role)}${msg.content}`;
}

/**
 * 构建完整 prompt。
 * @param messages 会话消息（含 system / user / assistant / tool）
 * @param systemText 需要注入到 System 段的文本（系统提示词 + 工具说明）
 */
export function buildPrompt(messages: ChatMessage[], systemText: string): string {
	const merged = mergeMessages(messages);
	const parts: string[] = [];

	let i = 0;
	while (i < merged.length) {
		const msg = merged[i];
		if (msg.role === "tool") {
			// 连续 tool 消息合并进同一个 tool▁outputs 块
			const items: string[] = [];
			while (i < merged.length && merged[i].role === "tool") {
				items.push(`${TAG_START}tool▁output▁begin${TAG_END}${merged[i].content}${TAG_START}tool▁output▁end${TAG_END}`);
				i += 1;
			}
			parts.push(`${TAG_START}tool▁outputs▁begin${TAG_END}${items.join("")}${TAG_START}tool▁outputs▁end${TAG_END}`);
		} else {
			parts.push(formatMessage(msg));
			i += 1;
		}
	}

	// 注入系统段（工具说明随系统提示词一起下发）
	if (systemText) {
		const sysIndex = parts.findIndex((p) => p.startsWith(roleTag("system")));
		if (sysIndex >= 0) {
			const existing = parts[sysIndex];
			const at = existing.lastIndexOf("\n");
			const insertAt = at === -1 ? existing.length : at;
			parts[sysIndex] = `${existing.slice(0, insertAt)}\n\n${systemText}${existing.slice(insertAt)}`;
		} else {
			parts.unshift(`${roleTag("system")}${systemText}\n`);
		}
	}

	const last = parts[parts.length - 1];
	if (!last || !last.startsWith(roleTag("assistant"))) {
		parts.push(`${roleTag("assistant")}\n`);
	}

	return parts.join("");
}
