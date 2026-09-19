// /login —— 启动可见浏览器，轮询 LocalStorage 抓取 userToken 并**校验后才保存**
//
// 关键修正（此前的缺陷）：
//   1. 旧实现只要 localStorage 中任意键名「包含 token」就采纳，
//      会误抓 smsdk_token / 设备 token 之类的值，导致「文件已存在但实际未登录」。
//      现在只认精确键名 userToken / user_token。
//   2. 旧实现抓到即保存、从不校验。现在必须先通过一次真实鉴权请求
//      （create_session → delete_session）才认为登录成功。
import type { App } from "../app.js";
import { color, info, warn } from "../ui/output.js";
import { verifyToken } from "./verify.js";

/** 登录结果 */
export interface LoginResult {
	token: string;
	userAgent: string;
}

/** 轮询超时（毫秒） */
const LOGIN_TIMEOUT_MS = 5 * 60 * 1000;
/** 轮询间隔（毫秒） */
const POLL_INTERVAL_MS = 1500;
/** 登录页地址 */
const LOGIN_URL = "https://chat.deepseek.com/";
/** 只接受这些精确键名（区分大小写比较时统一小写） */
const TOKEN_KEYS = ["usertoken", "user_token"];
/** token 最小长度（真实 userToken 远大于此，此处仅做基本过滤） */
const MIN_TOKEN_LENGTH = 20;

type BrowserStorage = Array<[string, string]>;

/** 把存储值规整为纯 token 字符串 */
function normalizeTokenValue(value: string): string | undefined {
	if (!value) return undefined;
	const trimmed = value.trim();
	if (!trimmed) return undefined;
	if (trimmed.startsWith("{") || trimmed.startsWith('"')) {
		try {
			const parsed = JSON.parse(trimmed);
			if (typeof parsed === "string") return acceptToken(parsed);
			if (parsed && typeof parsed === "object") {
				const candidate = parsed.value ?? parsed.token;
				if (typeof candidate === "string") return acceptToken(candidate);
			}
		} catch {
			// 不是合法 JSON，按裸字符串处理
		}
	}
	return undefined;
}

function acceptToken(token: string): string | undefined {
	const t = token.trim();
	return t.length >= MIN_TOKEN_LENGTH ? t : undefined;
}

/** 从存储条目中严格提取 userToken */
function extractToken(entries: BrowserStorage): string | undefined {
	for (const wanted of TOKEN_KEYS) {
		const hit = entries.find(([key]) => key.toLowerCase() === wanted);
		if (!hit) continue;
		const token = normalizeTokenValue(hit[1]);
		if (token) return token;
	}
	return undefined;
}

/** 依次尝试可用的浏览器通道（默认 Edge） */
async function launchBrowser(
	chromium: { launch: (options: Record<string, unknown>) => Promise<unknown> },
): Promise<{ browser: any; channel: string }> {
	const override = process.env.PI_LOGIN_BROWSER?.trim().toLowerCase();
	const channels: Array<string | undefined> = override
		? override === "chromium"
			? [undefined]
			: [override]
		: ["msedge", "chrome", undefined];

	const errors: string[] = [];
	for (const channel of channels) {
		try {
			const browser = await chromium.launch({
				headless: false,
				...(channel ? { channel } : {}),
				args: ["--no-first-run", "--no-default-browser-check"],
			});
			return { browser, channel: channel ?? "chromium" };
		} catch (e) {
			errors.push(`${channel ?? "chromium"}: ${(e as Error).message}`);
		}
	}
	throw new Error(errors.join(" | "));
}

/** 读取页面上的存储条目 */
async function readStorage(page: any): Promise<BrowserStorage> {
	return (await page.evaluate(() => {
		const collect = (store: any): Array<[string, string]> => {
			const out: Array<[string, string]> = [];
			try {
				for (let i = 0; i < store.length; i += 1) {
					const key = store.key(i);
					if (key == null) continue;
					out.push([key, store.getItem(key) ?? ""]);
				}
			} catch {
				// 忽略跨域等异常
			}
			return out;
		};
		const g = globalThis as any;
		return [...collect(g.localStorage), ...collect(g.sessionStorage)];
	})) as BrowserStorage;
}

/**
 * 交互式登录：拉起浏览器（默认 Edge），
 * 等用户在页面内完成登录 → 抓到 userToken → 校验通过后才返回。
 */
export async function loginInteractive(app: App): Promise<LoginResult> {
	const { t } = app.i18n;

	let playwright: any;
	try {
		// 使用变量说明符：即使未安装 playwright 也不影响其他模块的类型检查
		const specifier = "playwright";
		playwright = (await import(specifier)) as any;
	} catch {
		throw new Error(t("login.needPlaywright"));
	}

	info(color.dim(t("login.starting")));
	const { browser, channel } = await launchBrowser(playwright.chromium);
	info(color.dim(`browser: ${channel}`));

	try {
		const context = await browser.newContext();
		const page = await context.newPage();
		await page.goto(LOGIN_URL, { waitUntil: "domcontentloaded", timeout: 60_000 });

		info(color.cyan(t("login.waiting")));

		const deadline = Date.now() + LOGIN_TIMEOUT_MS;
		let token: string | undefined;
		let userAgent = "";
		let lastCandidate: string | undefined;
		let lastReject: string | undefined;

		while (Date.now() < deadline) {
			if (page.isClosed()) throw new Error("浏览器窗口已被关闭");
			try {
				userAgent = (await page.evaluate(
					() => (globalThis as any).navigator.userAgent as string,
				)) as string;

				const candidate = extractToken(await readStorage(page));

				// 只在候选值发生变化时校验，避免反复打接口
				if (candidate && candidate !== lastCandidate) {
					lastCandidate = candidate;
					info(color.dim(t("login.verifying")));
					const result = await verifyToken(app.config, candidate, userAgent);
					if (result.ok) {
						token = candidate;
						break;
					}
					lastReject = result.error ?? "unknown";
					info(color.yellow(t("login.invalid", { error: lastReject })));
				}
			} catch {
				// 页面跳转中可能短暂失败，继续轮询
			}
			await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS));
		}

		if (!token) {
			if (lastReject) info(color.yellow(t("login.lastReject", { error: lastReject })));
			throw new Error(t("login.timeout", { minutes: Math.round(LOGIN_TIMEOUT_MS / 60000) }));
		}

		return { token, userAgent };
	} finally {
		try {
			await browser.close();
		} catch {
			warn("关闭浏览器失败");
		}
	}
}
