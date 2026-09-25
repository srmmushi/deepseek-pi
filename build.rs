//! 把当前 git 提交编进二进制，`/info` 的「构建号」用它。
//!
//! 用户问的「构建号」= GitHub 上那次提交的编号，所以这里取 `git rev-parse --short HEAD`。
//! 仓库是打包分发的（没有 .git）时退化成 `unknown`，不影响编译。

use std::process::Command;

fn main() {
    let hash = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    // 工作区有改动时标出来：否则「构建号指向某次提交」会误导人
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let build = if dirty { format!("{hash}-dirty") } else { hash };

    println!("cargo:rustc-env=DSP_GIT_HASH={build}");

    // 提交变了就重新编译，别再拿着旧哈希
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-env-changed=DSP_GIT_HASH");
}

/// 跑一条 git 命令，成功就返回去掉首尾空白的结果
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Some(text)
}
