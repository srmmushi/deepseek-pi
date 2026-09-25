// Token 加密 —— 基于机器标识派生密钥的 AES-256-GCM
//
// 说明：本方案不引入任何第三方加密库，仅使用 Node 内置 crypto。
// 密钥由「主机名 + 用户名 + 平台 + 架构 + 固定盐」经 scrypt 派生，
// 因此密文只能在同机器、同用户下解密；换机需重新 /login。
import {
	createCipheriv,
	createDecipheriv,
	randomBytes,
	scryptSync,
	timingSafeEqual,
} from "node:crypto";
import { arch, hostname, platform, userInfo } from "node:os";

// 注意：这是加密盐，属于**稳定的密码学常量**，不是品牌字符串。
// 项目改名为 DSP 时特意保持不变——一旦改动，已保存的凭证将无法解密（需要重新 /login）。
const APP_SALT = "pi-deepseek-web/v1";
const KEY_LENGTH = 32;
const IV_LENGTH = 12;

/** 密文封装格式 */
export interface EncryptedPayload {
	v: 1;
	alg: "aes-256-gcm";
	iv: string;
	tag: string;
	data: string;
}

/** 采集机器指纹（尽力而为，失败时退化为基础值） */
function machineFingerprint(): string {
	let username = "unknown";
	try {
		username = userInfo().username;
	} catch {
		// 某些精简环境下 userInfo 会抛错，忽略
	}
	return [hostname(), username, platform(), arch()].join("|");
}

let cachedKey: Buffer | undefined;

/** 派生并缓存本地加密密钥 */
function deriveKey(): Buffer {
	if (!cachedKey) {
		cachedKey = scryptSync(machineFingerprint(), APP_SALT, KEY_LENGTH);
	}
	return cachedKey;
}

/** 加密字符串，返回可序列化的封装对象 */
export function encryptString(plain: string): EncryptedPayload {
	const key = deriveKey();
	const iv = randomBytes(IV_LENGTH);
	const cipher = createCipheriv("aes-256-gcm", key, iv);
	const data = Buffer.concat([cipher.update(plain, "utf8"), cipher.final()]);
	const tag = cipher.getAuthTag();
	return {
		v: 1,
		alg: "aes-256-gcm",
		iv: iv.toString("base64"),
		tag: tag.toString("base64"),
		data: data.toString("base64"),
	};
}

/** 解密封装对象；密钥不匹配或数据被篡改时抛错 */
export function decryptString(payload: EncryptedPayload): string {
	if (payload.v !== 1 || payload.alg !== "aes-256-gcm") {
		throw new Error("不支持的密文格式");
	}
	const key = deriveKey();
	const iv = Buffer.from(payload.iv, "base64");
	const tag = Buffer.from(payload.tag, "base64");
	const data = Buffer.from(payload.data, "base64");
	const decipher = createDecipheriv("aes-256-gcm", key, iv);
	decipher.setAuthTag(tag);
	return Buffer.concat([decipher.update(data), decipher.final()]).toString("utf8");
}

/** 常量时间比较（供需要时校验摘要用） */
export function safeEqual(a: string, b: string): boolean {
	const bufA = Buffer.from(a);
	const bufB = Buffer.from(b);
	if (bufA.length !== bufB.length) return false;
	return timingSafeEqual(bufA, bufB);
}
