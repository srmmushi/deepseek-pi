//! 把当前 git 提交编进二进制，`/info` 的「构建号」用它。
//!
//! 取短哈希（`88139a6` 这种）。这一步经常在奇怪的环境里失败，所以做了三重兜底：
//!   1. `git rev-parse --short HEAD`，并加 `-c safe.directory=*` ——
//!      WSL 里以 root 构建时仓库属主对不上，git 会直接拒绝（dubious ownership），
//!      加了这个参数才是「本机自己的仓库」，不然只能拿到 unknown。
//!   2. 直接读 `.git/HEAD` 和它指向的引用文件 —— 没装 git、或 git 用不了时也能成。
//!   3. 外部传入的 `DSP_GIT_HASH` —— 打包、CI 里可以自己指定。
//! 三条都拿不到才退化成 `unknown`（例如仓库被打包分发、没有 .git）。

use std::process::Command;

fn main() {
    let hash = git(&["-c", "safe.directory=*", "rev-parse", "--short", "HEAD"])
        .and_then(|h| short(&h, 7))
        .or_else(read_git_head)
        .or_else(|| {
            std::env::var("DSP_GIT_HASH")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string());

    // 工作区有改动时标出来：否则「构建号指向某次提交」会误导人
    let dirty = hash != "unknown" && git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let build = if dirty { format!("{hash}-dirty") } else { hash };

    println!("cargo:rustc-env=DSP_GIT_HASH={build}");

    // 提交变了就重新编译，别再拿着旧哈希
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-env-changed=DSP_GIT_HASH");
}

/// 直接读 `.git`：`HEAD` 里通常是 `ref: refs/heads/main`，顺着它读对应文件；
/// 分离头指针时 `HEAD` 里本身就是哈希。
fn read_git_head() -> Option<String> {
    let head = std::fs::read_to_string(".git/HEAD").ok()?;
    let head = head.trim();
    let hash = match head.strip_prefix("ref:") {
        Some(reference) => std::fs::read_to_string(format!(".git/{}", reference.trim())).ok()?,
        None => head.to_string(),
    };
    let hash = hash.trim().to_string();
    if hash.is_empty() {
        return None;
    }
    short(&hash, 7)
}

/// 取前 n 个字符
fn short(text: &str, n: usize) -> Option<String> {
    let out: String = text.trim().chars().take(n).collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 跑一条 git 命令，成功就返回去掉首尾空白的结果
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}
