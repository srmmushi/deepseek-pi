//! 登录凭证：加密存储 + 载入
//!
//! 加密方案与 TS 版**逐字节一致**：
//!   key   = scrypt(机器指纹, "pi-deepseek-web/v1", 32)   （Node scrypt 默认参数 N=16384,r=8,p=1）
//!   指纹  = hostname | username | platform | arch      （platform/arch 用 Node 的取值）
//!   密文  = base64(iv[12]) + base64(tag[16]) + base64(ciphertext)，AES-256-GCM
//!
//! 注意 `platform`/`arch` 必须用 Node 的字符串（win32 / x64），
//! 而不是 Rust 的 windows / x86_64，否则派生出的密钥不同、已有凭证解不开。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::RngCore;
use scrypt::{scrypt, Params as ScryptParams};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::config::ConfigPaths;

/// 加密盐（密码学常量，改名时特意保持不变，否则已保存凭证无法解密）
const APP_SALT: &[u8] = b"pi-deepseek-web/v1";
const KEY_LENGTH: usize = 32;
const IV_LENGTH: usize = 12;

/// 密文封装（与 TS 的 EncryptedPayload 字段名一致）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedPayload {
    pub v: u32,
    pub alg: String,
    pub iv: String,
    pub tag: String,
    pub data: String,
}

/// 磁盘上的凭证文件结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthFileShape {
    pub provider: String,
    pub version: u32,
    #[serde(rename = "userAgent", default)]
    pub user_agent: String,
    #[serde(rename = "capturedAt", default)]
    pub captured_at: u64,
    pub token: EncryptedPayload,
}

/// 解密后的凭证
#[derive(Debug, Clone)]
pub struct AuthData {
    pub token: String,
    pub user_agent: String,
    pub captured_at: u64,
}

/// 平台字符串：必须与 Node 的 process.platform 一致
fn node_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

/// 架构字符串：必须与 Node 的 process.arch 一致
fn node_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "ia32"
    } else {
        "unknown"
    }
}

/// 机器指纹：hostname | username | platform | arch
pub fn machine_fingerprint() -> String {
    let host = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let user = whoami::username();
    format!("{host}|{user}|{}|{}", node_platform(), node_arch())
}

/// 由机器指纹派生本地密钥
fn derive_key() -> Result<[u8; KEY_LENGTH]> {
    let params = ScryptParams::new(14, 8, 1, KEY_LENGTH)
        .map_err(|e| anyhow!("scrypt 参数非法：{e}"))?;
    let mut out = [0u8; KEY_LENGTH];
    scrypt(machine_fingerprint().as_bytes(), APP_SALT, &params, &mut out)
        .map_err(|e| anyhow!("scrypt 派生失败：{e}"))?;
    Ok(out)
}

/// 加密字符串
pub fn encrypt_string(plain: &str) -> Result<EncryptedPayload> {
    let key = derive_key()?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow!("密钥长度错误：{e}"))?;
    let mut iv = [0u8; IV_LENGTH];
    rand::thread_rng().fill_bytes(&mut iv);
    let nonce = Nonce::from_slice(&iv);
    // aes-gcm 的输出是 ciphertext || tag(16)
    let sealed = cipher
        .encrypt(nonce, Payload { msg: plain.as_bytes(), aad: b"" })
        .map_err(|_| anyhow!("加密失败"))?;
    let split = sealed.len().saturating_sub(16);
    let (data, tag) = sealed.split_at(split);
    Ok(EncryptedPayload {
        v: 1,
        alg: "aes-256-gcm".to_string(),
        iv: B64.encode(iv),
        tag: B64.encode(tag),
        data: B64.encode(data),
    })
}

/// 解密封装对象
pub fn decrypt_string(payload: &EncryptedPayload) -> Result<String> {
    if payload.v != 1 || payload.alg != "aes-256-gcm" {
        return Err(anyhow!("不支持的密文格式"));
    }
    let key = derive_key()?;
    let iv = B64.decode(&payload.iv).context("iv base64 解码失败")?;
    let tag = B64.decode(&payload.tag).context("tag base64 解码失败")?;
    let data = B64.decode(&payload.data).context("data base64 解码失败")?;
    if iv.len() != IV_LENGTH {
        return Err(anyhow!("iv 长度异常：{}", iv.len()));
    }
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow!("密钥长度错误：{e}"))?;
    // 拼回 ciphertext || tag 交给 aes-gcm
    let mut sealed = data;
    sealed.extend_from_slice(&tag);
    let plain = cipher
        .decrypt(Nonce::from_slice(&iv), Payload { msg: &sealed, aad: b"" })
        .map_err(|_| anyhow!("解密失败：密钥不匹配或数据被篡改（换机需重新登录）"))?;
    String::from_utf8(plain).context("明文不是合法 UTF-8")
}

/// 保存凭证
pub fn save_auth(paths: &ConfigPaths, data: &AuthData) -> Result<()> {
    let shape = AuthFileShape {
        provider: "deepseek-web".to_string(),
        version: 1,
        user_agent: data.user_agent.clone(),
        captured_at: data.captured_at,
        token: encrypt_string(&data.token)?,
    };
    crate::config::write_json(&paths.auth_file, &shape)
}

/// 读取并解密凭证；不存在或无法解密时返回 None
pub fn load_auth(paths: &ConfigPaths) -> Option<AuthData> {
    let shape: AuthFileShape = crate::config::read_json(&paths.auth_file)?;
    let token = decrypt_string(&shape.token).ok()?;
    if token.is_empty() {
        return None;
    }
    Some(AuthData {
        token,
        user_agent: shape.user_agent,
        captured_at: shape.captured_at,
    })
}

/// 删除凭证文件
pub fn clear_auth(paths: &ConfigPaths) -> bool {
    if paths.auth_file.exists() {
        std::fs::remove_file(&paths.auth_file).is_ok()
    } else {
        false
    }
}

/// 是否已登录
pub fn has_auth(paths: &ConfigPaths) -> bool {
    load_auth(paths).is_some()
}

/// token 指纹（展示 / 排查用，不泄露原文）
pub fn token_fingerprint(token: &str) -> String {
    // 简单求和哈希即可，仅用于人眼比对
    let mut h: u64 = 1469598103934665603;
    for b in token.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("{h:016x}")
}

/// 从文件读出凭证（供 /status 判断文件是否存在）
pub fn auth_file_exists(paths: &ConfigPaths) -> bool {
    paths.auth_file.exists()
}

/// 提示：手工粘贴 token（Rust 版不做浏览器自动化）
pub fn save_token_from_input(paths: &ConfigPaths, token: &str, user_agent: &str) -> Result<AuthData> {
    let data = AuthData {
        token: token.trim().to_string(),
        user_agent: user_agent.to_string(),
        captured_at: now_ms(),
    };
    save_auth(paths, &data)?;
    Ok(data)
}

/// 当前毫秒时间戳
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 便捷：判断某路径下的凭证是否可解密
pub fn auth_readable(paths: &ConfigPaths) -> bool {
    Path::new(&paths.auth_file).exists() && load_auth(paths).is_some()
}
