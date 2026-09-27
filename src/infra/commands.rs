//! 自定义斜杠命令：`<项目>/.dsp/commands/*.md` 与 `<配置目录>/commands/*.md`
//!
//! 抄的是 Claude Code（`.claude/commands/*.md`）和 OpenCode（`.opencode/command/*.md`）
//! 的做法：**一个 markdown 文件就是一个命令**，正文就是发给模型的提示词。
//! 文件名（去掉 `.md`）即命令名；正文里的 `$ARGUMENTS` 会换成命令后面那段，
//! 没有 `$ARGUMENTS` 就把参数附在末尾。
//!
//! 内置命令优先：查不到才轮到命令文件（判定在 `main.rs` 的 `command()` 里，
//! 走的是「match 落到最后那个分支」），所以自定义命令顶不掉 `/help` 这类，
//! 不会把自己锁在外面。

use std::path::{Path, PathBuf};

use crate::config::ConfigPaths;

/// 一个可用的自定义命令
#[derive(Debug, Clone)]
pub struct Command {
    pub name: String,
    pub path: PathBuf,
    /// 首行文字（去掉 `#`），`/commands` 里显示
    pub about: String,
}

/// 搜索目录：项目里的在前 —— 同名时压过用户级的
pub fn search_dirs(paths: &ConfigPaths, cwd: &Path) -> Vec<PathBuf> {
    vec![
        cwd.join(".dsp").join("commands"),
        paths.config_dir.join("commands"),
    ]
}

/// 全部可用命令，按名字排序
pub fn list(paths: &ConfigPaths, cwd: &Path) -> Vec<Command> {
    let mut out: Vec<Command> = Vec::new();
    for dir in search_dirs(paths, cwd) {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        for p in files {
            if p.extension().map(|e| e != "md").unwrap_or(true) {
                continue;
            }
            let Some(name) = p.file_stem().map(|s| s.to_string_lossy().to_string()) else {
                continue;
            };
            // 项目里的先扫到，同名的用户级命令就让位
            if name.is_empty() || out.iter().any(|c| c.name.eq_ignore_ascii_case(&name)) {
                continue;
            }
            let about = std::fs::read_to_string(&p)
                .map(|t| about_of(&t))
                .unwrap_or_default();
            out.push(Command {
                name,
                path: p,
                about,
            });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// 命令名 → (展开后的提示词, 命令文件路径)
pub fn expand(
    paths: &ConfigPaths,
    cwd: &Path,
    name: &str,
    args: &str,
) -> Option<(String, PathBuf)> {
    let cmd = list(paths, cwd)
        .into_iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))?;
    let body = std::fs::read_to_string(&cmd.path).ok()?;
    let text = if body.contains("$ARGUMENTS") {
        body.replace("$ARGUMENTS", args)
    } else if args.trim().is_empty() {
        body
    } else {
        format!("{body}\n\n{args}")
    };
    Some((text, cmd.path))
}

/// 简介：第一个非空行（顺带去掉 markdown 标题的 `#`）
fn about_of(text: &str) -> String {
    let line = text
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let line = line.trim_start_matches('#').trim();
    if line.chars().count() > 48 {
        format!("{}…", line.chars().take(47).collect::<String>())
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_in(dir: &Path) -> ConfigPaths {
        ConfigPaths {
            config_dir: dir.to_path_buf(),
            auth_file: dir.join("auth.json"),
            config_file: dir.join("config.json"),
            models_file: dir.join("models.json"),
            system_prompt_file: dir.join("system-prompt.md"),
            sessions_dir: dir.join("sessions"),
        }
    }

    #[test]
    fn about_takes_first_meaningful_line() {
        assert_eq!(about_of("\n\n# 审查代码\n\n细节…"), "审查代码");
        assert_eq!(about_of("直接一句话"), "直接一句话");
        assert_eq!(about_of("   "), "");
    }

    /// 参数替换、项目优先、非 md 文件忽略
    #[test]
    fn expand_and_project_precedence() {
        let root = std::env::temp_dir().join("dsp-commands-test");
        let _ = std::fs::remove_dir_all(&root);
        let cfg = root.join("cfg");
        let proj = root.join("proj");
        std::fs::create_dir_all(cfg.join("commands")).unwrap();
        std::fs::create_dir_all(proj.join(".dsp").join("commands")).unwrap();
        std::fs::write(cfg.join("commands").join("review.md"), "用户级：审 $ARGUMENTS").unwrap();
        std::fs::write(cfg.join("commands").join("note.txt"), "不是 md").unwrap();
        std::fs::write(proj.join(".dsp").join("commands").join("review.md"), "项目级：审 $ARGUMENTS").unwrap();
        std::fs::write(proj.join(".dsp").join("commands").join("plain.md"), "没有占位符").unwrap();

        let paths = paths_in(&cfg);
        let names: Vec<String> = list(&paths, &proj).into_iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["plain".to_string(), "review".to_string()]);

        let (text, _) = expand(&paths, &proj, "review", "src/a.rs").unwrap();
        assert_eq!(text, "项目级：审 src/a.rs", "同名时项目里的优先");

        let (text, _) = expand(&paths, &proj, "plain", "顺便看看 b").unwrap();
        assert_eq!(text, "没有占位符\n\n顺便看看 b", "没有占位符就附在末尾");

        assert!(expand(&paths, &proj, "不存在的", "").is_none());
    }
}
