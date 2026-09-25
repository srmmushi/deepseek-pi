//! DeepSeek 网页版协议层：PoW 求解 + REST 客户端 + completion 生命周期
//!
//! 三个子模块合在一个文件里，因为它们共享同一套错误类型与配置。

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use reqwest::blocking::Client as HttpClient;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::AppConfig;
use crate::stream::{is_retryable, SseParser, StreamEvent};

/// DeepSeek 站点源
pub const ORIGIN: &str = "https://chat.deepseek.com";

/// 账号密码登录（手机号 / 邮箱）。已由多个第三方项目佐证存在，
/// 但请求体字段名仍以实际响应为准（失败时会把原始响应打出来）。
const EP_LOGIN: &str = "/users/login";
/// 微信扫码登录页（二维码编号就从它的 HTML 里抠）
const SIGN_IN_URL: &str = "https://chat.deepseek.com/sign_in";
/// 拿微信的 code 换 DeepSeek 的 userToken。
///
/// **未能离线核实** —— 扫码链路（取编号 / 取图片 / 轮询 errcode）都是实测可用的，
/// 只有最后这步换 token 的路径查不到。跑不通时会把服务端原始响应打出来。
const EP_WECHAT_LOGIN: &str = "/users/login_by_wechat";
const EP_SESSION_CREATE: &str = "/chat_session/create";
const EP_SESSION_DELETE: &str = "/chat_session/delete";
const EP_SESSION_PAGE: &str = "/chat_session/fetch_page";

/// 取 `key` 之后的那一串「像编号的字符」（字母数字以及 - _）。
/// 用来从 HTML 的 `src="/connect/qrcode/XXX"` 或微信的
/// `window.wx_errcode=405;window.wx_code='YYY';` 里抠值，不引入正则。
fn after_key(text: &str, key: &str) -> Option<String> {
    let at = text.find(key)? + key.len();
    let value: String = text[at..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// 从登录响应里挖出 userToken。嵌套层级没核实过，几种常见位置都试一遍。
fn dig_token(data: &Value) -> Option<String> {
    const KEYS: [&str; 2] = ["token", "user_token"];
    let pick = |v: &Value| -> Option<String> {
        KEYS.iter()
            .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
            .map(|s| s.to_string())
    };
    pick(data)
        .or_else(|| data.get("user").and_then(|u| pick(u)))
        .or_else(|| {
            data.get("data")
                .and_then(|d| d.get("user"))
                .and_then(|u| pick(u))
        })
}
const EP_POW_CHALLENGE: &str = "/chat/create_pow_challenge";
const EP_COMPLETION: &str = "/chat/completion";
const EP_STOP_STREAM: &str = "/chat/stop_stream";

/// PoW target_path
pub const POW_TARGET_COMPLETION: &str = "/api/v0/chat/completion";

/// 统一错误类型
#[derive(Debug)]
pub enum DsError {
    /// 业务错误（信封里的 code / biz_code）
    Api { code: i64, message: String },
    /// HTTP 层错误
    Http { status: u16, body: String },
    /// CloudFront WAF 拦截
    Waf,
    /// 上游 hint（限流等）
    Hint(String, bool),
    /// 其它
    Other(String),
}

impl std::fmt::Display for DsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DsError::Api { code, message } => write!(f, "接口错误 {code}: {message}"),
            DsError::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            DsError::Waf => write!(f, "WAF challenge"),
            DsError::Hint(msg, _) => write!(f, "{msg}"),
            DsError::Other(msg) => write!(f, "{msg}"),
        }
    }
}
impl std::error::Error for DsError {}

impl DsError {
    /// 凭证失效
    pub fn is_auth(&self) -> bool {
        matches!(self, DsError::Api { code: 40003, .. })
    }
    /// 限流
    pub fn is_rate_limit(&self) -> bool {
        match self {
            DsError::Hint(_, overloaded) => *overloaded,
            DsError::Api { code, .. } => *code == 1001 || *code == 1201,
            _ => false,
        }
    }
    pub fn is_retryable(&self) -> bool {
        is_retryable(self)
    }
}

/// ── PoW 挑战 ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Challenge {
    pub algorithm: String,
    pub challenge: String,
    pub salt: String,
    pub signature: String,
    pub difficulty: i64,
    pub expire_at: i64,
    pub target_path: String,
}

/// base64(JSON) 后的 PoW 头
pub fn encode_pow_header(challenge: &Challenge, answer: i64) -> String {
    let payload = json!({
        "algorithm": challenge.algorithm,
        "challenge": challenge.challenge,
        "salt": challenge.salt,
        "answer": answer,
        "signature": challenge.signature,
        "target_path": challenge.target_path,
    });
    B64.encode(payload.to_string())
}

/// ── PoW 求解器（wasmi 执行官方 sha3 WASM）────────────────────

/// PoW 求解器：持有已实例化的 WASM 运行时
pub struct PowSolver {
    store: wasmi::Store<()>,
    instance: wasmi::Instance,
    memory: wasmi::Memory,
    malloc: String,
    add_to_stack: String,
    solve: String,
}

impl PowSolver {
    /// 从 WASM 字节构建求解器
    pub fn new(wasm: &[u8]) -> Result<Self> {
        let engine = wasmi::Engine::default();
        let module = wasmi::Module::new(&engine, wasm).context("解析 PoW WASM 失败")?;
        let mut store = wasmi::Store::new(&engine, ());
        // DeepSeek 的 sha3 wasm 通常没有 import；有的话给出明确提示
        let linker = wasmi::Linker::new(&engine);
        let instance = linker
            .instantiate(&mut store, &module)
            .context("实例化 PoW WASM 失败（可能存在未提供的 import）")?
            .start(&mut store)
            .context("启动 PoW WASM 失败")?;

        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| anyhow!("PoW WASM 未导出 memory"))?;

        // 收集函数型导出名，用于「按名探测 + 唯一候选兜底」
        // 收集函数导出：名字 + 参数个数 + 返回值个数。
        // 光有名字不够 —— 新版 wasm-bindgen 会把分配器导成 __wbindgen_export_N
        // 这种与位置相关的名字，只有签名才靠得住。
        let funcs: Vec<(String, usize, usize)> = module
            .exports()
            .filter_map(|export| match export.ty() {
                wasmi::ExternType::Func(ty) => Some((
                    export.name().to_string(),
                    ty.params().into_iter().count(),
                    ty.results().into_iter().count(),
                )),
                _ => None,
            })
            .collect();
        let listing = || {
            funcs
                .iter()
                .map(|f| format!("{}({}/{})", f.0, f.1, f.2))
                .collect::<Vec<_>>()
                .join(", ")
        };

        // 先按名字找，名字对不上再按签名找 —— 且要求签名唯一，才敢认。
        // 之前用「前缀是 __wbindgen_export_」兜底，很容易挑中 free 之类的函数，
        // 参数个数不对、一调用就 trap，报出来就是「调用分配器失败」。
        let pick = |want: &str, params: usize, results: usize| -> Option<String> {
            if let Some(found) = funcs.iter().find(|f| f.0 == want) {
                return Some(found.0.clone());
            }
            let mut hits = funcs
                .iter()
                .filter(|f| f.1 == params && f.2 == results)
                .map(|f| f.0.clone());
            let first = hits.next()?;
            hits.next().is_none().then_some(first)
        };

        // __wbindgen_malloc(size, align) -> ptr
        let malloc = pick("__wbindgen_malloc", 2, 1)
            .ok_or_else(|| anyhow!("未找到内存分配器 (i32,i32)->i32。现有导出：{}", listing()))?;
        // __wbindgen_add_to_stack_pointer(delta) -> ptr
        let add_to_stack = pick("__wbindgen_add_to_stack_pointer", 1, 1)
            .ok_or_else(|| anyhow!("未找到栈指针函数 (i32)->i32。现有导出：{}", listing()))?;
        let solve = pick("wasm_solve", 4, 0)
            .or_else(|| {
                // 名字也被改过时的兜底：排除已认出的两个，只剩一个就认它
                let rest: Vec<&(String, usize, usize)> = funcs
                    .iter()
                    .filter(|f| f.0 != malloc && f.0 != add_to_stack && f.0 != "memory")
                    .collect();
                (rest.len() == 1).then(|| rest[0].0.clone())
            })
            .ok_or_else(|| anyhow!("未找到 wasm_solve。现有导出：{}", listing()))?;

        Ok(Self {
            store,
            instance,
            memory,
            malloc,
            add_to_stack,
            solve,
        })
    }

    /// 在 WASM 内存里写入字节，返回 (ptr, len)
    fn write_bytes(&mut self, data: &[u8]) -> Result<(i32, i32)> {
        let malloc = self
            .instance
            .get_func(&self.store, &self.malloc)
            .ok_or_else(|| anyhow!("缺少分配器"))?;
        let name = self.malloc.clone();
        let size = data.len();
        let mut results = [wasmi::Val::I32(0)];
        malloc
            .call(
                &mut self.store,
                &[wasmi::Val::I32(size as i32), wasmi::Val::I32(1)],
                &mut results,
            )
            .with_context(|| format!("调用分配器 {name} 失败（传参 i32,i32 = {size},1）"))?;
        let ptr = match results[0] {
            wasmi::Val::I32(v) => v,
            _ => bail!("分配器返回值异常"),
        };
        let mem = self.memory.data_mut(&mut self.store);
        let start = ptr as usize;
        let end = start + data.len();
        if end > mem.len() {
            bail!("WASM 内存不足（需要 {end} 字节，实际 {}）", mem.len());
        }
        mem[start..end].copy_from_slice(data);
        Ok((ptr, data.len() as i32))
    }

    /// 调用一个函数，参数类型按实际签名动态适配
    fn call_dynamic(&mut self, name: &str, ints: &[i64]) -> Result<()> {
        let func = self
            .instance
            .get_func(&self.store, name)
            .ok_or_else(|| anyhow!("缺少函数导出：{name}"))?;
        let ty = func.ty(&self.store);
        let params_ty = ty.params().to_vec();
        let results_ty = ty.results().to_vec();

        let mut params = Vec::with_capacity(params_ty.len());
        for (i, t) in params_ty.iter().enumerate() {
            let raw = *ints.get(i).unwrap_or(&0);
            // wasmi 0.36 起 ValType 位于 wasmi::core，且 F32/F64 是包装类型
            params.push(match t {
                wasmi::core::ValType::I32 => wasmi::Val::I32(raw as i32),
                wasmi::core::ValType::I64 => wasmi::Val::I64(raw),
                wasmi::core::ValType::F32 => {
                    wasmi::Val::F32(wasmi::core::F32::from(raw as f32))
                }
                wasmi::core::ValType::F64 => {
                    wasmi::Val::F64(wasmi::core::F64::from(raw as f64))
                }
                _ => wasmi::Val::I32(raw as i32),
            });
        }
        let mut results = vec![wasmi::Val::I32(0); results_ty.len()];
        func.call(&mut self.store, &params, &mut results)
            .with_context(|| format!("调用 {name} 失败"))?;
        Ok(())
    }

    /// 求解挑战，返回 answer
    pub fn solve(&mut self, challenge: &Challenge) -> Result<i64> {
        if challenge.algorithm != "DeepSeekHashV1" {
            bail!("不支持的 PoW 算法：{}", challenge.algorithm);
        }
        // 前缀是 salt_expireAt_，取不到 expire_at 会导致永远无解
        let prefix = format!("{}_{}_", challenge.salt, challenge.expire_at);

        // 12/16 字节的返回区（wasm-bindgen 惯例：i32 status + f64 value）
        let add = self
            .instance
            .get_func(&self.store, &self.add_to_stack)
            .ok_or_else(|| anyhow!("缺少栈指针导出"))?;
        let mut res = [wasmi::Val::I32(0)];
        add.call(
            &mut self.store,
            &[wasmi::Val::I32(-16)],
            &mut res,
        )
        .context("调整 WASM 栈指针失败")?;
        let retptr = match res[0] {
            wasmi::Val::I32(v) => v,
            _ => bail!("栈指针返回值异常"),
        };

        let (cp, cl) = self.write_bytes(challenge.challenge.as_bytes())?;
        let (pp, pl) = self.write_bytes(prefix.as_bytes())?;

        let solve_name = self.solve.clone();
        self.call_dynamic(
            &solve_name,
            &[
                retptr as i64,
                cp as i64,
                cl as i64,
                pp as i64,
                pl as i64,
                challenge.difficulty,
            ],
        )?;

        let mem = self.memory.data(&self.store);
        let base = retptr as usize;
        if base + 16 > mem.len() {
            bail!("返回区越界");
        }
        let status = i32::from_le_bytes(mem[base..base + 4].try_into().unwrap());
        let value = f64::from_le_bytes(mem[base + 8..base + 16].try_into().unwrap());
        if status == 0 {
            bail!("WASM 未求出解（difficulty={}）", challenge.difficulty);
        }
        Ok(value as i64)
    }
}

/// 下载 / 复用 PoW WASM（进程内缓存）
pub fn load_pow_solver(url: &str, user_agent: &str, proxy: &str) -> Result<PowSolver> {
    let mut builder = HttpClient::builder()
        .user_agent(user_agent)
        .timeout(Duration::from_secs(60));
    if !proxy.is_empty() {
        builder = builder.proxy(reqwest::Proxy::all(proxy)?);
    }
    let http = builder.build()?;
    let res = http
        .get(url)
        .header("Accept", "*/*")
        .send()
        .with_context(|| format!("下载 PoW WASM 失败：{url}"))?;
    if !res.status().is_success() {
        bail!("下载 PoW WASM 失败：HTTP {}", res.status().as_u16());
    }
    let bytes = res.bytes()?;
    PowSolver::new(&bytes)
}

// ── REST 客户端 ─────────────────────────────────────────────

/// 由 UA 推导 sec-ch-ua 系列头，让请求更接近真实浏览器
fn client_hints(ua: &str) -> Vec<(&'static str, String)> {
    let chrome = extract_version(ua, "Chrome/");
    let Some(chrome) = chrome else {
        return vec![];
    };
    let edge = extract_version(ua, "Edg/");
    let mut brands = match edge {
        Some(edge_ver) => vec![
            format!("\"Microsoft Edge\";v=\"{edge_ver}\""),
            format!("\"Chromium\";v=\"{chrome}\""),
        ],
        None => vec![
            format!("\"Google Chrome\";v=\"{chrome}\""),
            format!("\"Chromium\";v=\"{chrome}\""),
        ],
    };
    brands.push("\"Not(A:Brand\";v=\"24\"".to_string());
    let platform = if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Mac OS X") {
        "macOS"
    } else {
        "Linux"
    };
    vec![
        ("sec-ch-ua", brands.join(", ")),
        ("sec-ch-ua-mobile", "?0".to_string()),
        ("sec-ch-ua-platform", format!("\"{platform}\"")),
        ("sec-fetch-dest", "empty".to_string()),
        ("sec-fetch-mode", "cors".to_string()),
        ("sec-fetch-site", "same-origin".to_string()),
    ]
}

fn extract_version(ua: &str, marker: &str) -> Option<String> {
    let start = ua.find(marker)? + marker.len();
    let digits: String = ua[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

/// DeepSeek 网页版客户端
pub struct DeepSeekClient {
    config: AppConfig,
    http: HttpClient,
    last_request_at: Mutex<Option<Instant>>,
}

impl DeepSeekClient {
    pub fn new(config: &AppConfig) -> Result<Self> {
        let mut builder = HttpClient::builder()
            .timeout(Duration::from_secs(300))
            .danger_accept_invalid_certs(false);
        if !config.proxy.is_empty() {
            builder = builder.proxy(reqwest::Proxy::all(&config.proxy)?);
        }
        Ok(Self {
            config: config.clone(),
            http: builder.build()?,
            last_request_at: Mutex::new(None),
        })
    }

    /// 伪造浏览器请求头
    fn build_headers(&self, token: Option<&str>, pow: Option<&str>, has_body: bool) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let mut put = |k: &str, v: &str| {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                headers.insert(name, value);
            }
        };
        put("User-Agent", &self.config.user_agent);
        put("Accept", "*/*");
        put(
            "Accept-Language",
            &format!("{},en;q=0.9", self.config.client_locale.replace('_', "-")),
        );
        put("Origin", ORIGIN);
        put("Referer", &format!("{ORIGIN}/"));
        for (k, v) in client_hints(&self.config.user_agent) {
            put(k, &v);
        }
        if !self.config.client_version.is_empty() {
            put("x-client-version", &self.config.client_version);
        }
        if !self.config.client_platform.is_empty() {
            put("x-client-platform", &self.config.client_platform);
        }
        if !self.config.client_locale.is_empty() {
            put("x-client-locale", &self.config.client_locale);
        }
        if has_body {
            put("Content-Type", "application/json");
        }
        // 空 token 视为匿名：登录请求本身就没有凭证，
        // 带上 "Bearer " 反而可能被风控挡下来
        if let Some(t) = token {
            if !t.is_empty() {
                put("Authorization", &format!("Bearer {t}"));
            }
        }
        if let Some(p) = pow {
            put("x-ds-pow-response", p);
        }
        headers
    }

    /// 保守限速
    fn throttle(&self) {
        let gap = self.config.request_interval_ms;
        if gap == 0 {
            return;
        }
        let mut guard = self.last_request_at.lock().unwrap();
        if let Some(last) = *guard {
            let elapsed = last.elapsed().as_millis() as u64;
            if elapsed < gap {
                std::thread::sleep(Duration::from_millis(gap - elapsed));
            }
        }
        *guard = Some(Instant::now());
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.config.api_base, path)
    }

    /// POST JSON 并解开信封，返回 biz_data
    fn post_json(&self, path: &str, token: &str, body: &Value) -> Result<Value, DsError> {
        self.throttle();
        let res = self
            .http
            .post(self.url(path))
            .headers(self.build_headers(Some(token), None, true))
            .body(body.to_string())
            .send()
            .map_err(|e| DsError::Other(format!("请求失败：{e}")))?;

        let status = res.status();
        if status.as_u16() == 202 {
            return Err(DsError::Waf);
        }
        let text = res
            .text()
            .map_err(|e| DsError::Other(format!("读取响应失败：{e}")))?;
        if !status.is_success() {
            return Err(DsError::Http {
                status: status.as_u16(),
                body: text.chars().take(300).collect(),
            });
        }
        let env: Value = serde_json::from_str(&text)
            .map_err(|e| DsError::Other(format!("响应不是合法 JSON：{e}")))?;
        let code = env.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        if code != 0 {
            return Err(DsError::Api {
                code,
                message: env
                    .get("msg")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error")
                    .to_string(),
            });
        }
        let data = env.get("data").filter(|d| !d.is_null()).ok_or(DsError::Api {
            code: -1,
            message: "响应缺少 data 字段".to_string(),
        })?;
        let biz_code = data.get("biz_code").and_then(|v| v.as_i64()).unwrap_or(0);
        if biz_code != 0 {
            return Err(DsError::Api {
                code: biz_code,
                message: data
                    .get("biz_msg")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown biz error")
                    .to_string(),
            });
        }
        Ok(data.get("biz_data").cloned().unwrap_or(Value::Null))
    }

    /// 手机号 / 邮箱 + 密码登录，成功返回 userToken。
    ///
    /// 手机号会走 `mobile` + `area_code`，其余一律当邮箱走 `email`。
    pub fn login_with_password(
        &self,
        account: &str,
        password: &str,
        device_id: &str,
    ) -> Result<String, DsError> {
        let account = account.trim();
        let mut body = json!({
            "password": password,
            "device_id": device_id,
            "os": "web",
        });

        let digits: String = account.chars().filter(|c| c.is_ascii_digit()).collect();
        let only_digits = account
            .chars()
            .all(|c| c.is_ascii_digit() || c == '+' || c == '-' || c == ' ');
        if only_digits && digits.len() >= 6 {
            let (area, mobile) = if digits.len() > 11 {
                // 写成 +8613800138000 时，前面的就是国家码
                (format!("+{}", &digits[..digits.len() - 11]), digits[digits.len() - 11..].to_string())
            } else {
                ("+86".to_string(), digits.clone())
            };
            body["mobile"] = json!(mobile);
            body["area_code"] = json!(area);
        } else {
            body["email"] = json!(account);
        }

        let data = self.post_json(EP_LOGIN, "", &body)?;
        dig_token(&data).ok_or_else(|| DsError::Api {
            code: -1,
            message: format!("登录响应里没有 token，原始内容：{data}"),
        })
    }

    /// 从登录页里抠出微信二维码的编号。
    ///
    /// 页面里有一张 `<img class="js_qrcode img web_qrcode_img" src="/connect/qrcode/021fI1iv1lah1w38">`，
    /// 编号每次刷新都会变，所以必须现取现用。
    pub fn wechat_qr_uuid(&self) -> Result<String, DsError> {
        let res = self
            .http
            .get(SIGN_IN_URL)
            .headers(self.build_headers(None, None, false))
            .send()
            .map_err(|e| DsError::Other(format!("打开登录页失败：{e}")))?;
        let html = res.text().unwrap_or_default();
        let uuid = after_key(&html, "/connect/qrcode/").ok_or_else(|| DsError::Api {
            code: -1,
            message: "登录页里没找到 /connect/qrcode 的二维码地址".to_string(),
        })?;
        Ok(uuid)
    }

    /// 去微信取二维码图片（open.weixin.qq.com 直接返回图片字节）
    pub fn wechat_qr_png(&self, uuid: &str) -> Result<Vec<u8>, DsError> {
        let res = self
            .http
            .get(format!("https://open.weixin.qq.com/connect/qrcode/{uuid}"))
            .headers(self.build_headers(None, None, false))
            .send()
            .map_err(|e| DsError::Other(format!("下载二维码失败：{e}")))?;
        if !res.status().is_success() {
            return Err(DsError::Api {
                code: -1,
                message: format!("下载二维码失败：HTTP {}", res.status().as_u16()),
            });
        }
        res.bytes()
            .map(|b| b.to_vec())
            .map_err(|e| DsError::Other(format!("读取二维码失败：{e}")))
    }

    /// 轮询扫码状态，返回微信的 errcode。
    /// 约定：408 未扫码 · 404 已扫待确认 · 405 已确认（附带 wx_code）· 403 过期或取消。
    pub fn wechat_scan_state(&self, uuid: &str) -> Result<(i32, String), DsError> {
        let res = self
            .http
            .get(format!(
                "https://long.open.weixin.qq.com/connect/l/qrconnect?uuid={uuid}&_={}",
                crate::auth::now_ms()
            ))
            .headers(self.build_headers(None, None, false))
            .send()
            .map_err(|e| DsError::Other(format!("轮询扫码状态失败：{e}")))?;
        let text = res.text().unwrap_or_default();
        let code = after_key(&text, "wx_errcode=")
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(0);
        let wx_code = after_key(&text, "wx_code='").unwrap_or_default();
        Ok((code, wx_code))
    }

    /// 用微信给的 code 换 userToken。
    ///
    /// **路径未能核实** —— 扫码那一段（取编号 → 取图片 → 轮询 errcode）都是真的，
    /// 但最后这一步 DeepSeek 拿什么接口换 token 无从查证，这里按同类接口推测。
    /// 跑不通时错误信息里会带服务端原始响应，照着改一行即可。
    pub fn login_by_wechat(&self, wx_code: &str, device_id: &str) -> Result<String, DsError> {
        let data = self.post_json(
            EP_WECHAT_LOGIN,
            "",
            &json!({ "code": wx_code, "device_id": device_id, "os": "web" }),
        )?;
        dig_token(&data).ok_or_else(|| DsError::Api {
            code: -1,
            message: format!("微信登录响应里没有 token，原始内容：{data}"),
        })
    }

    /// 创建会话
    pub fn create_session(&self, token: &str) -> Result<String, DsError> {
        let data = self.post_json(EP_SESSION_CREATE, token, &json!({}))?;
        data.get("chat_session")
            .and_then(|s| s.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or(DsError::Api {
                code: -1,
                message: "创建会话失败：缺少 chat_session.id".to_string(),
            })
    }

    /// 读取会话列表里某个会话的标题。
    ///
    /// 网页端在首轮结束后会自动给会话起名（「排查 Rust 生命周期报错」这种），
    /// 比我们按提示词截断出来的好看。接口形状若变动，这里返回 None，
    /// 调用方继续用本地标题，不影响任何功能。
    pub fn session_title(&self, token: &str, session_id: &str) -> Option<String> {
        let data = self
            .post_json(EP_SESSION_PAGE, token, &json!({ "count": 50 }))
            .ok()?;
        data.get("chat_sessions")?
            .as_array()?
            .iter()
            .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(session_id))
            .and_then(|s| s.get("title").and_then(|v| v.as_str()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// 删除会话（失败静默）
    pub fn delete_session(&self, token: &str, session_id: &str) {
        let _ = self.post_json(
            EP_SESSION_DELETE,
            token,
            &json!({ "chat_session_id": session_id }),
        );
    }

    /// 请求 PoW 挑战
    pub fn create_pow_challenge(
        &self,
        token: &str,
        target_path: &str,
    ) -> Result<Challenge, DsError> {
        let data = self.post_json(
            EP_POW_CHALLENGE,
            token,
            &json!({ "target_path": target_path }),
        )?;
        let raw = data.get("challenge").ok_or(DsError::Api {
            code: -1,
            message: "获取 PoW 挑战失败".to_string(),
        })?;
        let field = |k: &str| raw.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let num = |k: &str| raw.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        Ok(Challenge {
            algorithm: field("algorithm"),
            challenge: field("challenge"),
            salt: field("salt"),
            signature: field("signature"),
            difficulty: num("difficulty"),
            // 关键：前缀依赖 expire_at，取不到会退化成 salt_0_
            expire_at: num("expire_at"),
            target_path: {
                let t = field("target_path");
                if t.is_empty() {
                    target_path.to_string()
                } else {
                    t
                }
            },
        })
    }

    /// 发起 completion，返回可读的响应体
    pub fn completion(
        &self,
        token: &str,
        pow_header: &str,
        payload: &Value,
    ) -> Result<reqwest::blocking::Response, DsError> {
        self.throttle();
        let res = self
            .http
            .post(self.url(EP_COMPLETION))
            .headers(self.build_headers(Some(token), Some(pow_header), true))
            .body(payload.to_string())
            .send()
            .map_err(|e| DsError::Other(format!("请求失败：{e}")))?;
        let status = res.status();
        if status.as_u16() == 202 {
            return Err(DsError::Waf);
        }
        if !status.is_success() {
            let text = res.text().unwrap_or_default();
            return Err(DsError::Http {
                status: status.as_u16(),
                body: text.chars().take(300).collect(),
            });
        }
        Ok(res)
    }

    /// 中断流式输出（失败不致命）
    pub fn stop_stream(&self, token: &str, session_id: &str, message_id: u64) {
        let _ = self.post_json(
            EP_STOP_STREAM,
            token,
            &json!({ "chat_session_id": session_id, "message_id": message_id }),
        );
    }
}

// ── completion 编排 ─────────────────────────────────────────

/// 网页会话句柄（复用模式）
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct WebSessionHandle {
    pub session_id: Option<String>,
    pub parent_message_id: Option<u64>,
}

/// 单次流式对话：逐事件回调。回调返回 false 表示请求中断。
///
/// 参数逐个传入（而不是打包成结构体），这样重试循环可以每轮重新借用 `&mut`。
#[allow(clippy::too_many_arguments)]
pub fn stream_chat(
    client: &DeepSeekClient,
    solver: &mut PowSolver,
    token: &str,
    prompt: &str,
    model_type: &str,
    thinking_enabled: bool,
    search_enabled: bool,
    handle: Option<&mut WebSessionHandle>,
    on_event: &mut dyn FnMut(StreamEvent) -> bool,
) -> Result<(), DsError> {
    let owned_session = handle.is_none();
    let mut handle = handle;
    let already_bound = handle
        .as_ref()
        .and_then(|h| h.session_id.clone())
        .is_some();

    let session_id = match handle.as_ref().and_then(|h| h.session_id.clone()) {
        Some(id) => id,
        None => {
            let id = client.create_session(token)?;
            if let Some(h) = handle.as_mut() {
                h.session_id = Some(id.clone());
            }
            id
        }
    };

    let mut message_id: Option<u64> = None;
    let mut finished = false;
    let mut saw_event = false;
    let mut raw_head = String::new();

    let result = (|| -> Result<(), DsError> {
        let challenge = client.create_pow_challenge(token, POW_TARGET_COMPLETION)?;
        let answer = solver
            .solve(&challenge)
            .map_err(|e| DsError::Other(format!("PoW 求解失败：{e}")))?;
        let pow_header = encode_pow_header(&challenge, answer);

        let payload = json!({
            "chat_session_id": session_id,
            "parent_message_id": if already_bound {
                handle.as_ref().and_then(|h| h.parent_message_id)
            } else {
                None
            },
            "model_type": model_type,
            "prompt": prompt.to_string(),
            "ref_file_ids": [],
            "thinking_enabled": thinking_enabled,
            "search_enabled": search_enabled,
            "preempt": false,
        });

        let mut res = client.completion(token, &pow_header, &payload)?;
        let mut parser = SseParser::new();
        let mut buf = [0u8; 8192];

        loop {
            let n = res
                .read(&mut buf)
                .map_err(|e| DsError::Other(format!("读取流失败：{e}")))?;
            if n == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            if message_id.is_none() && raw_head.len() < 4096 {
                raw_head.push_str(&text);
                message_id = crate::stream::extract_message_id(&raw_head);
            }
            let events = parser
                .push(&text)
                .map_err(|e| DsError::Hint(e.message, e.overloaded))?;
            if !events.is_empty() {
                saw_event = true;
            }
            for evt in events {
                if !on_event(evt) {
                    break;
                }
            }
            if parser.done() {
                finished = true;
                break;
            }
        }

        if !finished {
            let tail = parser
                .flush()
                .map_err(|e| DsError::Hint(e.message, e.overloaded))?;
            for evt in tail {
                if matches!(evt, StreamEvent::Done { .. }) {
                    finished = true;
                }
                saw_event = true;
                if !on_event(evt) {
                    break;
                }
            }
        }

        if !saw_event {
            return Err(DsError::Other(format!(
                "无法解析的响应：{}",
                raw_head.chars().take(200).collect::<String>()
            )));
        }
        Ok(())
    })();

    // 收尾：回写 message id、必要时中断、一次性会话删除
    if let Some(h) = handle.as_mut() {
        if let Some(id) = message_id {
            h.parent_message_id = Some(id);
        }
    }
    if !finished {
        if let Some(id) = message_id {
            client.stop_stream(token, &session_id, id);
        }
    }
    if owned_session {
        client.delete_session(token, &session_id);
    } else if !already_bound && message_id.is_none() {
        client.delete_session(token, &session_id);
        if let Some(h) = handle.as_mut() {
            h.session_id = None;
        }
    }

    result
}

/// 带退避重试的流式对话（仅在尚未产出任何事件时重试）
#[allow(clippy::too_many_arguments)]
pub fn stream_chat_with_retry(
    client: &DeepSeekClient,
    solver: &mut PowSolver,
    token: &str,
    prompt: &str,
    model_type: &str,
    thinking_enabled: bool,
    search_enabled: bool,
    handle: Option<&mut WebSessionHandle>,
    on_event: &mut dyn FnMut(StreamEvent) -> bool,
    max_attempts: usize,
) -> Result<(), DsError> {
    let mut attempt = 0;
    let mut handle = handle;
    loop {
        attempt += 1;
        let mut yielded = false;
        {
            let mut wrapper = |evt: StreamEvent| {
                yielded = true;
                on_event(evt)
            };
            let result = stream_chat(
                client,
                solver,
                token,
                prompt,
                model_type,
                thinking_enabled,
                search_enabled,
                handle.as_deref_mut(),
                &mut wrapper,
            );
            match result {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if matches!(e, DsError::Waf)
                        || yielded
                        || !e.is_retryable()
                        || attempt >= max_attempts
                    {
                        return Err(e);
                    }
                }
            }
        }
        let backoff = 1000u64 * 2u64.pow((attempt - 1) as u32);
        std::thread::sleep(Duration::from_millis(backoff));
    }
}
