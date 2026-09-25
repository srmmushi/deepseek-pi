//! 配置目录解析与运行时配置读写
//!
//! 目录优先级（从高到低）：`--config-dir` > 环境变量 `PI_CONFIG_DIR` > `~/.pi/agent`。
//! 路径与文件格式刻意与历史实现保持一致，这样可以直接复用已有的登录凭证与会话。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 配置目录派生出的全部路径
#[derive(Debug, Clone)]
pub struct ConfigPaths {
    pub config_dir: PathBuf,
    pub auth_file: PathBuf,
    pub config_file: PathBuf,
    pub models_file: PathBuf,
    pub system_prompt_file: PathBuf,
    pub sessions_dir: PathBuf,
}

/// 展开路径里的 `~`
pub fn expand_tilde(input: &str) -> String {
    if input == "~" {
        return home_dir().to_string_lossy().to_string();
    }
    if let Some(rest) = input.strip_prefix("~/").or_else(|| input.strip_prefix("~\\")) {
        return home_dir().join(rest).to_string_lossy().to_string();
    }
    input.to_string()
}

/// 用户主目录（优先 HOME / USERPROFILE）
fn home_dir() -> PathBuf {
    if let Ok(h) = std::env::var("HOME") {
        if !h.trim().is_empty() {
            return PathBuf::from(h);
        }
    }
    if let Ok(h) = std::env::var("USERPROFILE") {
        if !h.trim().is_empty() {
            return PathBuf::from(h);
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// 计算配置根目录
pub fn resolve_config_dir(cli_override: Option<&str>) -> PathBuf {
    if let Some(dir) = cli_override {
        if !dir.trim().is_empty() {
            return PathBuf::from(expand_tilde(dir.trim()));
        }
    }
    if let Ok(dir) = std::env::var("PI_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(expand_tilde(dir.trim()));
        }
    }
    home_dir().join(".pi").join("agent")
}

/// 由配置根目录派生全部文件路径
pub fn resolve_paths(cli_override: Option<&str>) -> ConfigPaths {
    let config_dir = resolve_config_dir(cli_override);
    ConfigPaths {
        auth_file: config_dir.join("auth").join("deepseek-web.json"),
        config_file: config_dir.join("config.json"),
        models_file: config_dir.join("models.json"),
        system_prompt_file: config_dir.join("system-prompt.md"),
        sessions_dir: config_dir.join("sessions"),
        config_dir,
    }
}

// ── 运行时配置 ──────────────────────────────────────────────

/// 界面语言
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    pub fn detect() -> Self {
        for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(v) = std::env::var(key) {
                if v.to_ascii_lowercase().starts_with("zh") {
                    return Lang::Zh;
                }
            }
        }
        // Windows 下看系统区域
        if cfg!(windows) {
            if let Ok(v) = std::env::var("USERPROFILE") {
                if !v.is_ascii() {
                    return Lang::Zh;
                }
            }
        }
        Lang::En
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        let v = input.trim().to_ascii_lowercase();
        if v.starts_with("zh") || v == "cn" {
            Some(Lang::Zh)
        } else if v.starts_with("en") {
            Some(Lang::En)
        } else {
            None
        }
    }

    pub fn other(self) -> Self {
        match self {
            Lang::Zh => Lang::En,
            Lang::En => Lang::Zh,
        }
    }
}

/// 上下文策略
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContextMode {
    /// 复用同一网页会话，每轮只发增量
    Reuse,
    /// 每轮打包完整历史 + 一次性会话
    Replay,
}

/// 运行时配置（对应 config.json）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub language: Lang,
    pub thinking: bool,
    pub search: bool,
    pub model: String,
    #[serde(rename = "userAgent")]
    pub user_agent: String,
    #[serde(rename = "wasmUrl")]
    pub wasm_url: String,
    #[serde(rename = "apiBase")]
    pub api_base: String,
    #[serde(rename = "clientVersion")]
    pub client_version: String,
    #[serde(rename = "clientPlatform")]
    pub client_platform: String,
    #[serde(rename = "clientLocale")]
    pub client_locale: String,
    pub proxy: String,
    #[serde(rename = "requestIntervalMs")]
    pub request_interval_ms: u64,
    #[serde(rename = "maxToolSteps")]
    pub max_tool_steps: usize,
    #[serde(rename = "contextMode")]
    pub context_mode: ContextMode,
    /// 用哪个浏览器打开登录页（`/login browser`）。
    /// 空 = 自动探测；也可以填 sysinfo 探测出的 id。
    pub browser: String,
    /// 设备标识：密码登录要带上它，服务端据此认得这台机器。
    /// 空 = 还没生成，第一登录时随机生成并落盘。
    #[serde(rename = "deviceId")]
    pub device_id: String,
}

/// 默认 PoW WASM 地址（上游更新静态资源后需替换）
pub const DEFAULT_WASM_URL: &str =
    "https://fe-static.deepseek.com/chat/static/sha3_wasm_bg.7b9ca65ddd.wasm";

const DEFAULT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36";

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            language: Lang::detect(),
            thinking: false,
            search: true,
            model: "deepseek-chat".to_string(),
            user_agent: DEFAULT_UA.to_string(),
            wasm_url: DEFAULT_WASM_URL.to_string(),
            api_base: "https://chat.deepseek.com/api/v0".to_string(),
            client_version: "2.0.0".to_string(),
            client_platform: "web".to_string(),
            client_locale: "zh_CN".to_string(),
            proxy: String::new(),
            request_interval_ms: 1200,
            max_tool_steps: 25,
            context_mode: ContextMode::Reuse,
            browser: String::new(),
            device_id: String::new(),
        }
    }
}

/// 可切换的模型
pub const MODELS: [(&str, &str, bool); 2] = [
    ("deepseek-chat", "DeepSeek Chat", false),
    ("deepseek-reasoner", "DeepSeek Reasoner", true),
];

fn ensure_parent(file: &Path) -> Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("创建目录失败：{}", dir.display()))?;
    }
    Ok(())
}

/// 写入 JSON（缩进用 Tab，与历史实现一致）
pub fn write_json<T: Serialize>(file: &Path, value: &T) -> Result<()> {
    ensure_parent(file)?;
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(file, format!("{text}\n"))?;
    Ok(())
}

/// 读取 JSON；失败时返回 None
pub fn read_json<T: for<'a> Deserialize<'a>>(file: &Path) -> Option<T> {
    let text = std::fs::read_to_string(file).ok()?;
    serde_json::from_str(&text).ok()
}

/// 加载配置；文件不存在时写入默认值
pub fn load_config(paths: &ConfigPaths) -> AppConfig {
    let config: AppConfig = read_json(&paths.config_file).unwrap_or_default();
    if !paths.config_file.exists() {
        let _ = write_json(&paths.config_file, &config);
    }
    config
}

/// 保存配置
pub fn save_config(paths: &ConfigPaths, config: &AppConfig) -> Result<()> {
    write_json(&paths.config_file, config)
}

/// 生成 models.json（仅一个 provider）
pub fn ensure_models_file(paths: &ConfigPaths) -> Result<()> {
    if paths.models_file.exists() {
        return Ok(());
    }
    let models: Vec<serde_json::Value> = MODELS
        .iter()
        .map(|(id, name, reasoning)| {
            serde_json::json!({ "id": id, "name": name, "reasoning": reasoning })
        })
        .collect();
    write_json(
        &paths.models_file,
        &serde_json::json!({
            "providers": {
                "deepseek-web": {
                    "name": "DeepSeek Web",
                    "apiBase": "https://chat.deepseek.com/api/v0",
                    "notes": "DeepSeek 网页版逆向接口，仅此一个供应商。",
                    "models": models,
                }
            }
        }),
    )
}
