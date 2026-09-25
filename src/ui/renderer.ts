// 块式主输出渲染器（带滚动、选择、锚点）
//
// 为什么不用「写完就算」：主输出写在终端滚动区域里，一旦某行滚上去就无法单独擦除。
// 而折叠块、文本选择高亮、滚动历史、/goto 跳转都要求能重排历史，
// 所以主输出在写入终端的同时也记入内存缓冲区，需要时按当前状态重排并整体重绘。
//
// 三条不变式：
//   1. 缓冲区里每个「逻辑行」在屏幕上必须恰好占 1 行 —— 写入与重绘都先硬折行；
//   2. 窗口是底部对齐的：offset = 0 时最后一行落在屏幕第 height() 行；
//   3. offset > 0（用户正在看历史）时不再增量写入，只进缓冲，回到最底后整体重绘。
import { anchorNow, info } from "./output.js";
import { charWidth, displayWidth, stripAnsi } from "./text.js";

/** 可折叠块：头行常显，体行按 collapsed 决定显示与否 */
export interface Block {
	head: string;
	body: string[];
	collapsed: boolean;
}

/** 用户消息锚点（/goto 的跳转目标） */
export interface Anchor {
	/** 展示用文案（截断后的提示词） */
	label: string;
	/** 在缓冲区里的条目下标 */
	itemIndex: number;
}

/** 选择区间（屏幕坐标，行 1 起、列 1 起，列按显示宽度计） */
export interface Selection {
	startRow: number;
	startCol: number;
	endRow: number;
	endCol: number;
}

/** 缓冲区条目 */
type Item = { kind: "line"; text: string } | { kind: "block"; block: Block };

/** 渲染器所需的几何信息（由状态栏提供） */
export interface RenderLayout {
	height(): number;
	width(): number;
}

export interface Renderer {
	/** 追加普通行 */
	line(text: string): void;
	/** 追加可折叠块 */
	block(head: string, body: string[], collapsed: boolean): Block;
	/** 切换折叠并重绘 */
	toggle(block: Block): void;
	/** 就地替换块体（选择框高亮用）并重绘 */
	update(block: Block, body: string[]): void;
	/** 屏幕行（1 起）→ 该行所属可点击块 */
	blockAtRow(row: number): Block | undefined;
	/** 记录一个跳转锚点，返回条目下标 */
	anchor(label: string): number;
	/** 全部锚点 */
	anchors(): Anchor[];
	/** 滚动指定行数（正值 = 向上看更早内容） */
	scrollBy(rows: number): void;
	/** 回到最底部 */
	scrollToBottom(): void;
	/** 当前滚动偏移（行，0 = 底部） */
	offset(): number;
	/** 滚动到指定条目，使其出现在窗口顶部附近 */
	scrollToItem(itemIndex: number): void;
	/** 设置选择区间（null = 清除）并重绘 */
	setSelection(selection: Selection | null): void;
	/** 取选择区间对应的纯文本（已去 ANSI） */
	selectionText(selection: Selection): string;
	/** 按当前宽度重排重绘（尺寸变化后调用） */
	repaint(): void;
}

/** 缓冲区保留的最大条目数 */
const MAX_ITEMS = 4000;

/** 跳过 ANSI 颜色序列，返回「可见字符 + 显示宽度」序列 */
function glyphs(text: string): Array<{ ch: string; w: number }> {
	const out: Array<{ ch: string; w: number }> = [];
	for (let i = 0; i < text.length; i += 1) {
		if (text[i] === "\u001b") {
			const match = /^\u001b\[[0-9;]*m/.exec(text.slice(i));
			if (match) {
				out.push({ ch: match[0], w: 0 });
				i += match[0].length - 1;
				continue;
			}
		}
		out.push({ ch: text[i], w: charWidth(text.codePointAt(i) ?? 0) });
	}
	return out;
}

/** 按显示列区间把一行切成 前 / 中 / 后 三段（保留 ANSI 顺序） */
function sliceColumns(text: string, from: number, to: number): [string, string, string] {
	let pre = "";
	let mid = "";
	let post = "";
	let col = 0;
	for (const g of glyphs(text)) {
		if (col >= to) post += g.ch;
		else if (col + g.w > from && col < to) mid += g.ch;
		else if (col < from) pre += g.ch;
		else post += g.ch;
		col += g.w;
	}
	return [pre, mid, post];
}

export function createRenderer(layout: RenderLayout): Renderer {
	const items: Item[] = [];
	const anchorList: Anchor[] = [];
	/** 滚动偏移（行），0 = 贴在底部 */
	let offset = 0;
	let selection: Selection | null = null;
	/** 最后一次铺排结果：屏幕行 → 文本 / 归属块 */
	let screenText: string[] = [];
	let screenOwner: Array<Block | undefined> = [];
	/** 最后一次铺排：每个条目首行在总行序列里的下标 */
	let itemRows: number[] = [];

	const width = (): number => Math.max(20, layout.width() - 1);
	/**
	 * 可用于输出的行数。
	 * 滚动区域的最后一行始终由输入行占用（增量写入时会滚动上去），
	 * 因此重绘窗口必须比区域高度少 1 行，否则最后一行会被输入行覆盖。
	 */
	const height = (): number => Math.max(3, layout.height() - 1);

	/** 按显示宽度硬折行：每个逻辑行在屏幕上必须恰好占 1 行 */
	const wrap = (text: string, limit: number): string[] => {
		let total = 0;
		for (const g of glyphs(text)) total += g.w;
		if (total <= limit) return [text];

		const out: string[] = [];
		let cur = "";
		let used = 0;
		for (const g of glyphs(text)) {
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

	/** 按当前折叠状态，把缓冲区扁平化为「每元素 = 1 屏幕行」 */
	const flatten = (limit: number): Array<{ text: string; owner?: Block; item: number }> => {
		const rows: Array<{ text: string; owner?: Block; item: number }> = [];
		itemRows = [];
		items.forEach((item, index) => {
			itemRows.push(rows.length);
			if (item.kind === "line") {
				for (const seg of wrap(item.text, limit)) rows.push({ text: seg, item: index });
				return;
			}
			const { block } = item;
			for (const seg of wrap(block.head, limit)) rows.push({ text: seg, owner: block, item: index });
			if (!block.collapsed) {
				for (const bodyLine of block.body) {
					for (const seg of wrap(bodyLine, limit)) rows.push({ text: seg, item: index });
				}
			}
		});
		return rows;
	};

	/** 铺排并重绘整个可见窗口（底部对齐 + 滚动偏移 + 选择高亮） */
	const paint = (): void => {
		const limit = width();
		const h = height();
		const rows = flatten(limit);
		const maxOffset = Math.max(0, rows.length - h);
		offset = Math.min(Math.max(0, offset), maxOffset);

		const end = rows.length - offset;
		const start = Math.max(0, end - h);
		const visible = rows.slice(start, end);
		const startRow = Math.max(1, h - visible.length + 1);

		// 屏幕行映射（按屏幕行号索引，供命中测试与选择取文本）
		screenText = new Array(h + 1).fill("");
		screenOwner = new Array(h + 1).fill(undefined);
		visible.forEach((row, i) => {
			const y = startRow + i;
			if (y < 1 || y > h) return;
			screenText[y] = row.text;
			screenOwner[y] = row.owner;
		});

		let buf = "";
		visible.forEach((row, i) => {
			const y = startRow + i;
			let text = row.text;
			if (selection) {
				const lo = Math.min(selection.startRow, selection.endRow);
				const hi = Math.max(selection.startRow, selection.endRow);
				if (y >= lo && y <= hi) {
					const from = y === selection.startRow ? selection.startCol - 1 : 0;
					const to = y === selection.endRow ? selection.endCol - 1 : Number.MAX_SAFE_INTEGER;
					const [pre, mid, post] = sliceColumns(text, from, to);
					if (stripAnsi(mid)) text = `${pre}\u001b[7m${mid}\u001b[27m${post}`;
				}
			}
			buf += `\u001b[${y};1H\u001b[2K${text}`;
		});
		for (let y = 1; y < startRow; y += 1) buf += `\u001b[${y};1H\u001b[2K`;
		process.stdout.write(buf);
		// 重绘后把光标交还给输入行所在行（区域最后一行）
		anchorNow();
	};

	/** 把数组整理成「屏幕行」形状（下标 0 作为占位，行号 1..height） */
	const ensureScreenShape = (): void => {
		const h = height();
		if (screenText.length !== h + 1) {
			screenText = new Array(h + 1).fill("");
			screenOwner = new Array(h + 1).fill(undefined);
		}
	};

	/**
	 * 增量追加时同步维护「屏幕行 → 文本/块」映射。
	 * 贴底追加会让窗口整体上移，所以等价于 shift + push。
	 * 没有这一步的话，流式输出之后就点不中任何块（映射只在整体重绘时才算）。
	 */
	const pushRows = (rows: Array<{ text: string; owner?: Block }>): void => {
		if (rows.length === 0) return;
		ensureScreenShape();
		for (const row of rows) {
			screenText.shift();
			screenText.push(row.text);
			screenOwner.shift();
			screenOwner.push(row.owner);
		}
	};

	/** 增量写入；用户正在看历史时不写（避免把他们拽回底部） */
	const append = (rows: Array<{ text: string; owner?: Block }>): void => {
		// offset > 0 表示用户正在翻历史：只入缓冲，等回到最底再整体重绘
		if (offset > 0) return;
		for (const row of rows) info(row.text);
		pushRows(rows);
	};

	const trim = (): void => {
		if (items.length <= MAX_ITEMS) return;
		const drop = items.length - MAX_ITEMS;
		items.splice(0, drop);
		for (const a of anchorList) a.itemIndex -= drop;
		for (let i = anchorList.length - 1; i >= 0; i -= 1) {
			if (anchorList[i].itemIndex < 0) anchorList.splice(i, 1);
		}
	};

	return {
		line(text: string): void {
			items.push({ kind: "line", text });
			append(wrap(text, width()).map((seg) => ({ text: seg })));
			trim();
		},

		block(head: string, body: string[], collapsed: boolean): Block {
			const block: Block = { head, body, collapsed };
			items.push({ kind: "block", block });
			const rows: Array<{ text: string; owner?: Block }> = wrap(head, width()).map((seg) => ({
				text: seg,
				owner: block,
			}));
			if (!collapsed) {
				for (const bodyLine of body) {
					for (const seg of wrap(bodyLine, width())) rows.push({ text: seg });
				}
			}
			append(rows);
			trim();
			return block;
		},

		toggle(block: Block): void {
			block.collapsed = !block.collapsed;
			paint();
		},

		update(block: Block, body: string[]): void {
			block.body = body;
			paint();
		},

		blockAtRow(row: number): Block | undefined {
			return screenOwner[row];
		},

		anchor(label: string): number {
			anchorList.push({ label, itemIndex: items.length });
			return items.length;
		},

		anchors(): Anchor[] {
			return anchorList;
		},

		scrollBy(rows: number): void {
			offset += rows;
			paint();
		},

		scrollToBottom(): void {
			offset = 0;
			selection = null;
			paint();
		},

		offset(): number {
			return offset;
		},

		scrollToItem(itemIndex: number): void {
			const rows = flatten(width());
			const total = rows.length;
			const target = itemRows[itemIndex];
			if (target === undefined) return;
			// 让目标条目出现在窗口顶部
			offset = Math.max(0, total - target - height());
			selection = null;
			paint();
		},

		setSelection(next: Selection | null): void {
			selection = next;
			paint();
		},

		selectionText(sel: Selection): string {
			const lo = Math.min(sel.startRow, sel.endRow);
			const hi = Math.max(sel.startRow, sel.endRow);
			const out: string[] = [];
			for (let y = lo; y <= hi; y += 1) {
				const text = screenText[y] ?? "";
				const from = y === sel.startRow ? sel.startCol - 1 : 0;
				const to = y === sel.endRow ? sel.endCol - 1 : Number.MAX_SAFE_INTEGER;
				const [, mid] = sliceColumns(text, from, to);
				out.push(stripAnsi(mid));
			}
			return out.join("\n").replace(/\s+$/, "");
		},

		repaint(): void {
			paint();
		},
	};
}
