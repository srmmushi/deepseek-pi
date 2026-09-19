// PoW 求解器 —— 基于 DeepSeek 官方 sha3 WASM 的 DeepSeekHashV1
//
// 与 ds-free-api 的 Rust 实现等价：从 WASM 中动态探测 wasm-bindgen 导出符号，
// 调用 wasm_solve 得到答案，再以 base64(JSON) 形式放进 X-Ds-Pow-Response 头。
//
// 注意：JS 无法像 wasmtime 那样按函数签名筛选导出，因此这里按「导出名」探测，
// 并在名称完全缺失时退化为「唯一候选」策略，尽量兼容上游 WASM 更新。
import { info, color } from "../ui/output.js";

/** /chat/create_pow_challenge 返回的挑战数据 */
export interface Challenge {
	algorithm: string;
	challenge: string;
	salt: string;
	signature: string;
	difficulty: number;
	expireAfter: number;
	expireAt: number;
	targetPath: string;
}

/** base64 后的 PoW 头 */
export interface PowResult {
	algorithm: string;
	challenge: string;
	salt: string;
	answer: number;
	signature: string;
	targetPath: string;
}

interface WasmExports {
	memory: WebAssembly.Memory;
	alloc: (len: number, align: number) => number;
	addToStack: (delta: number) => number;
	solve: (retptr: number, cp: number, cl: number, pp: number, pl: number, difficulty: number) => void;
}

/** PoW 相关错误 */
export class PowError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "PowError";
	}
}

function pickByName(
	exports: WebAssembly.ModuleExportDescriptor[],
	name: string,
): string | undefined {
	return exports.find((e) => e.kind === "function" && e.name === name)?.name;
}

function pickByPredicate(
	exports: WebAssembly.ModuleExportDescriptor[],
	predicate: (name: string) => boolean,
): string | undefined {
	return exports.find((e) => e.kind === "function" && predicate(e.name))?.name;
}

/** 找出所有函数型导出，用于「唯一候选」兜底 */
function functionExports(exports: WebAssembly.ModuleExportDescriptor[]): string[] {
	return exports.filter((e) => e.kind === "function").map((e) => e.name);
}

/**
 * 构造 import 对象。
 * DeepSeek 的 sha3 wasm 通常没有 import；若存在，为函数型 import 提供返回 0 的桩，
 * 其余类型直接报错并给出可读提示。
 */
function buildImports(module: WebAssembly.Module): WebAssembly.Imports {
	const imports: Record<string, Record<string, WebAssembly.ImportValue>> = {};
	for (const imp of WebAssembly.Module.imports(module)) {
		imports[imp.module] ??= {};
		if (imp.kind === "function") {
			imports[imp.module][imp.name] = () => 0;
		} else {
			throw new PowError(`WASM 需要非函数型 import（${imp.kind}: ${imp.name}），暂不支持`);
		}
	}
	return imports;
}

/** PoW 求解器：持有已实例化的 WASM 运行时 */
export class PowSolver {
	private constructor(private readonly wasm: WasmExports) {}

	/** 从 WASM 字节构建求解器 */
	static async create(bytes: Uint8Array): Promise<PowSolver> {
		const module = await WebAssembly.compile(bytes as unknown as BufferSource);
		const exports = WebAssembly.Module.exports(module);
		const instance = await WebAssembly.instantiate(module, buildImports(module));

		const memory = instance.exports.memory;
		if (!(memory instanceof WebAssembly.Memory)) {
			throw new PowError("WASM 未导出 memory");
		}

		const addToStack =
			pickByName(exports, "__wbindgen_add_to_stack_pointer") ??
			pickByPredicate(exports, (n) => n.includes("add_to_stack"));
		if (!addToStack) throw new PowError("未找到 __wbindgen_add_to_stack_pointer 导出");

		const alloc =
			pickByName(exports, "__wbindgen_malloc") ??
			pickByPredicate(exports, (n) => n.startsWith("__wbindgen_export_")) ??
			pickByPredicate(exports, (n) => n.includes("malloc"));
		if (!alloc) throw new PowError("未找到内存分配器导出（__wbindgen_malloc）");

		let solve = pickByName(exports, "wasm_solve");
		if (!solve) {
			// 退化为「排除已知符号后的唯一函数」策略
			const known = new Set([addToStack, alloc, "memory"]);
			const rest = functionExports(exports).filter((n) => !known.has(n));
			if (rest.length === 1) solve = rest[0];
		}
		if (!solve) throw new PowError("未找到 wasm_solve 导出");

		const ex = instance.exports as unknown as Record<string, unknown>;
		return new PowSolver({
			memory: memory as WebAssembly.Memory,
			alloc: ex[alloc] as WasmExports["alloc"],
			addToStack: ex[addToStack] as WasmExports["addToStack"],
			solve: ex[solve] as WasmExports["solve"],
		});
	}

	/** 向 WASM 内存写入字符串，返回 (ptr, len) */
	private writeString(text: string): [number, number] {
		const bytes = new TextEncoder().encode(text);
		const len = bytes.length;
		const ptr = this.wasm.alloc(len, 1) >>> 0;
		// 注意：内存可能因增长而重建 buffer，必须每次重新获取
		new Uint8Array(this.wasm.memory.buffer, ptr, len).set(bytes);
		return [ptr, len];
	}

	/** 求解挑战，返回可直接放入请求头的结果 */
	solveChallenge(challenge: Challenge): PowResult {
		if (challenge.algorithm !== "DeepSeekHashV1") {
			throw new PowError(`不支持的 PoW 算法：${challenge.algorithm}`);
		}

		const prefix = `${challenge.salt}_${challenge.expireAt}_`;
		const retptr = this.wasm.addToStack(-16) >>> 0;

		const [ptrChallenge, lenChallenge] = this.writeString(challenge.challenge);
		const [ptrPrefix, lenPrefix] = this.writeString(prefix);

		this.wasm.solve(
			retptr,
			ptrChallenge,
			lenChallenge,
			ptrPrefix,
			lenPrefix,
			challenge.difficulty,
		);

		const view = new DataView(this.wasm.memory.buffer);
		const status = view.getInt32(retptr, true);
		const value = view.getFloat64(retptr + 8, true);
		if (status === 0) throw new PowError("WASM 未求出解");

		return {
			algorithm: challenge.algorithm,
			challenge: challenge.challenge,
			salt: challenge.salt,
			answer: Math.trunc(value),
			signature: challenge.signature,
			targetPath: challenge.targetPath,
		};
	}
}

/** 把 PoW 结果编码为 X-Ds-Pow-Response 头 */
export function encodePowHeader(result: PowResult): string {
	const json = JSON.stringify({
		algorithm: result.algorithm,
		challenge: result.challenge,
		salt: result.salt,
		answer: result.answer,
		signature: result.signature,
		target_path: result.targetPath,
	});
	return Buffer.from(json, "utf8").toString("base64");
}

// ── WASM 缓存 ────────────────────────────────────────────────────────────
let cachedSolver: PowSolver | undefined;
let cachedUrl: string | undefined;

/** 下载 / 复用 POW WASM 并构建求解器（进程内缓存，UA 使用浏览器一致值） */
export async function loadPowSolver(wasmUrl: string, userAgent: string): Promise<PowSolver> {
	if (cachedSolver && cachedUrl === wasmUrl) return cachedSolver;

	info(color.dim(`下载 PoW WASM: ${wasmUrl}`));
	const res = await fetch(wasmUrl, {
		headers: { "User-Agent": userAgent, Accept: "*/*" },
	});
	if (!res.ok) {
		throw new PowError(`下载 WASM 失败：HTTP ${res.status}`);
	}
	const bytes = new Uint8Array(await res.arrayBuffer());
	const solver = await PowSolver.create(bytes);
	cachedSolver = solver;
	cachedUrl = wasmUrl;
	return solver;
}
