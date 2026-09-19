// 凭证校验 —— 用「纯读取」接口确认 token 是否可用
//
// 关键：不能用「创建会话」来校验。那样每校验一次就会在网页端留下一个会话，
// 是「一个 pi 会话在网页端出现多个会话」的来源之一。
// 这里改用 GET /users/current：读当前用户信息，无任何副作用。
import { createHash } from "node:crypto";
import type { AppConfig } from "../config/store.js";
import { ApiError, DeepSeekClient } from "../deepseek/client.js";

export interface VerifyResult {
	ok: boolean;
	/** 失败原因（可读） */
	error?: string;
	/** 业务 / 系统错误码 */
	code?: number;
}

/** token 指纹：只暴露哈希前 8 位，便于比对而不泄露明文 */
export function tokenFingerprint(token: string): string {
	return createHash("sha256").update(token).digest("hex").slice(0, 8);
}

interface UserCurrentEnvelope {
	code: number;
	msg?: string;
	data?: { biz_code?: number; biz_msg?: string } | null;
}

/** 校验 token 是否有效（无副作用） */
export async function verifyToken(
	config: AppConfig,
	token: string,
	userAgent?: string,
): Promise<VerifyResult> {
	try {
		const client = await DeepSeekClient.create({
			...config,
			userAgent: userAgent || config.userAgent,
		});
		const res = await client.raw("/users/current", { method: "GET", token });
		const env = (await res.json()) as UserCurrentEnvelope;

		if (env.code !== 0) {
			return { ok: false, code: env.code, error: `${env.code}: ${env.msg || "invalid token"}` };
		}
		const bizCode = env.data?.biz_code ?? 0;
		if (bizCode !== 0) {
			return {
				ok: false,
				code: bizCode,
				error: `${bizCode}: ${env.data?.biz_msg || "business error"}`,
			};
		}
		return { ok: true };
	} catch (e) {
		if (e instanceof ApiError) {
			return { ok: false, code: e.code, error: `${e.code}: ${e.message}` };
		}
		return { ok: false, error: (e as Error).message };
	}
}
