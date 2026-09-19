// 登录凭证持久化 —— auth/deepseek-web.json
//
// 文件内只保存加密后的 token；User-Agent 等非敏感信息明文保存，便于请求复用。
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import type { ConfigPaths } from "../config/dirs.js";
import { decryptString, encryptString, type EncryptedPayload } from "./crypto.js";

/** 解密后的登录凭证 */
export interface AuthData {
	token: string;
	userAgent: string;
	capturedAt: number;
}

/** 磁盘上的凭证文件结构 */
interface AuthFileShape {
	provider: "deepseek-web";
	version: 1;
	userAgent: string;
	capturedAt: number;
	token: EncryptedPayload;
}

function ensureDirFor(file: string): void {
	const dir = dirname(file);
	if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
}

/** 保存凭证（token 加密，文件权限尽量收紧） */
export function saveAuth(paths: ConfigPaths, data: AuthData): void {
	const payload: AuthFileShape = {
		provider: "deepseek-web",
		version: 1,
		userAgent: data.userAgent,
		capturedAt: data.capturedAt,
		token: encryptString(data.token),
	};
	ensureDirFor(paths.authFile);
	writeFileSync(paths.authFile, `${JSON.stringify(payload, null, "\t")}\n`, {
		encoding: "utf8",
		mode: 0o600,
	});
}

/** 读取并解密凭证；不存在或无法解密时返回 undefined */
export function loadAuth(paths: ConfigPaths): AuthData | undefined {
	if (!existsSync(paths.authFile)) return undefined;
	try {
		const raw = readFileSync(paths.authFile, "utf8");
		const parsed = JSON.parse(raw) as AuthFileShape;
		const token = decryptString(parsed.token);
		if (!token) return undefined;
		return {
			token,
			userAgent: parsed.userAgent ?? "",
			capturedAt: parsed.capturedAt ?? 0,
		};
	} catch {
		// 换机 / 文件损坏都会走到这里，调用方会提示重新登录
		return undefined;
	}
}

/** 删除凭证文件 */
export function clearAuth(paths: ConfigPaths): boolean {
	if (!existsSync(paths.authFile)) return false;
	rmSync(paths.authFile, { force: true });
	return true;
}

/** 判断是否已登录 */
export function hasAuth(paths: ConfigPaths): boolean {
	return loadAuth(paths) !== undefined;
}
