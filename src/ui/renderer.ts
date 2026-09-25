// 块式主输出渲染器
//
// 为什么需要它：主输出写在终端的滚动区域里，一旦某行滚上去就无法单独擦除。
// 而「点击折叠」要求历史内容能收起来，所以必须能把**整个可见窗口**按新的折叠
// 状态重排后重绘。做法：主输出在写入终端的同时也记入内存缓冲区，
// 每次重绘都从缓冲区按当前折叠状态扁平化、折行、底部对齐，再逐行绝对定位写入。
//
// 关键约束：缓冲区里每个「逻辑行」在屏幕上必须恰好占 1 行。
// 因此写入与重绘都先把逻辑行按当前宽度硬折成多段，每段独占一行。
import { info } from "./output.js";
import { charWidth, displayWidth, stripAnsi } from "./text.js";

/** 可折叠块：头行常显，体行按 collapsed 决定显示与否 */
export interface Block {
	head: string;
	body: string[];
	collapsed: boolean;
}

/** 缓冲区条目 */
type Item = { kind: "line"; text: string } | { kind: "block"; block: Block };

/** 渲染器所需的几何信息（由状态栏提供） */
export interface RenderLayout {
	/** 可见区域高度（= 滚动区域底部行号） */
	height(): number;
	/** 可用宽度（列数；内部会再留 1 列避免触发延迟换行） */
	width(): number;
}

export interface Renderer {
	/** 追加普通行（立即写入终端） */
	line(text: string): void;
	/** 追加可折叠块（头 + 体按折叠态写入终端） */
	block(head: string, body: string[], collapsed: boolean): Block;
	/** 切换折叠状态并整体重绘 */
	toggle(block: Block): void;
	/** 屏幕行号（1 起）→ 该行所属的可点击块 */
	blockAtRow(row: number): Block | undefined;
	/** 按当前宽度重建可见窗口（尺寸变化后调用） */
	repaint(): void;
}

/** 跳过 ANSI 颜色序列，返回「可见字符 + 显示宽度」序列 */
function glyphs(text: string): Array<{ ch: string; w: number }> {
	const out: Array<{ ch: string; w: number }> = [];
	for (let i = 0; i < text.length; i += 1) {
		if (text[i] === "\u001b") {
			const match = /^\u001b\[[0-9;]*m/.exec(text.slice(i));
			if (match) {
				// 颜色序列整体挂在下一个可见字符前，保持顺序
				out.push({ ch: match[0], w: 0 });
				i += match[0].length - 1;
				continue;
			}
		}
		out.push({ ch: text[i], w: charWidth(text.codePointAt(i) ?? 0) });
	}
	return out;
}

/** 缓冲区保留的最大条目数（远大于任何可见高度，滚动出屏的块无需再支持点击） */
const MAX_ITEMS = 300;

export function createRenderer(layout: RenderLayout): Renderer {
	const items: Item[] = [];
	/** 可见窗口每行对应的块（下标 0 = 屏幕第 1 行） */
	let rowOwners: Array<Block | undefined> = [];

	const width = (): number => Math.max(20, layout.width() - 1);
	const height = (): number => Math.max(4, layout.height());

	/** 按显示宽度硬折行：每个逻辑行在屏幕上必须恰好占 1 行 */
	const wrap = (text: string, limit: number): string[] => {
		const parts = glyphs(text);
		let total = 0;
		for (const g of parts) total += g.w;
		if (total <= limit) return [text];

		const out: string[] = [];
		let cur = "";
		let used = 0;
		for (const g of parts) {
			if (used + g.w > limit && stripAnsi(cur)) {
				out.push(cur);
				cur = "";
				used = 0;
			}
			cur += g.ch;
			used += g.w;
		}
		out.push(cur);
		return out;
	};

	/** 按当前折叠状态，把缓冲区扁平化为「每元素 = 1 屏幕行」的文本列表 */
	const flatten = (limit: number): Array<{ text: string; owner?: Block }> => {
		const rows: Array<{ text: string; owner?: Block }> = [];
		for (const item of items) {
			if (item.kind === "line") {
				for (const seg of wrap(item.text, limit)) rows.push({ text: seg });
				continue;
			}
			const { block } = item;
			for (const seg of wrap(block.head, limit)) rows.push({ text: seg, owner: block });
			if (!block.collapsed) {
				for (const bodyLine of block.body) {
					for (const seg of wrap(bodyLine, limit)) rows.push({ text: seg });
				}
			}
		}
		return rows;
	};

	/**
	 * 重算「屏幕行 → 块」映射。
	 * 窗口是**底部对齐**的：最后一行落在屏幕第 height() 行，
	 * 所以命中测试必须用屏幕行号索引，而不是窗口下标。
	 */
	const refreshOwners = (): void => {
		const rows = flatten(width());
		const h = height();
		const visible = rows.slice(Math.max(0, rows.length - h));
		const startRow = Math.max(1, h - visible.length + 1);
		const owners: Array<Block | undefined> = new Array(h + 2).fill(undefined);
		visible.forEach((row, i) => {
			const screenRow = startRow + i;
			if (screenRow >= 1 && screenRow <= h) owners[screenRow] = row.owner;
		});
		rowOwners = owners;
	};

	/** 缓冲区只保留最近若干条目，避免长会话内存无限增长 */
	const trim = (): void => {
		if (items.length > MAX_ITEMS) items.splice(0, items.length - MAX_ITEMS);
	};

	/** 把可见窗口的最后 height 行打印到滚动区域（底部对齐） */
	const paintWindow = (): void => {
		const limit = width();
		const rows = flatten(limit);
		const visible = rows.slice(Math.max(0, rows.length - height()));
		refreshOwners();

		const start = Math.max(1, height() - visible.length + 1);
		let buf = "";
		for (let i = 0; i < visible.length; i += 1) {
			buf += `\u001b[${start + i};1H\u001b[2K${visible[i].text}`;
		}
		// 清掉窗口上方可能残留的旧内容
		for (let row = 1; row < start; row += 1) buf += `\u001b[${row};1H\u001b[2K`;
		process.stdout.write(buf);
	};

	return {
		line(text: string): void {
			items.push({ kind: "line", text });
			// 增量路径：直接追加写入（滚动区域会自动滚动），保持流式实时性
			for (const seg of wrap(text, width())) info(seg);
			trim();
			refreshOwners();
		},

		block(head: string, body: string[], collapsed: boolean): Block {
			const block: Block = { head, body, collapsed };
			items.push({ kind: "block", block });
			// 增量路径：先写头，再按折叠态写体
			for (const seg of wrap(head, width())) info(seg);
			if (!collapsed) {
				for (const bodyLine of body) {
					for (const seg of wrap(bodyLine, width())) info(seg);
				}
			}
			trim();
			refreshOwners();
			return block;
		},

		toggle(block: Block): void {
			block.collapsed = !block.collapsed;
			paintWindow();
		},

		blockAtRow(row: number): Block | undefined {
			return rowOwners[row];
		},

		repaint(): void {
			paintWindow();
		},
	};
}
