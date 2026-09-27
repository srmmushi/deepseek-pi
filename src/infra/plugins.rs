//! 插件：安装 / 列出 / 卸载
//!
//! 一个插件就是一个 zip 包，解到 `<配置目录>/plugins/<名字>/`。包根上要有
//! `plugin.json`（写名字、版本、说明），可选 `prompt.md` —— 有就追加进系统提示词。
//!
//! 为什么先只做「提示词扩展」这一种载荷：它最省事又立刻有用，而且和本项目
//! 「系统提示词 + 工具说明」的架构天然对齐 —— 不改代码就能改变行为。
//! 以后要加别的类型（命令、工具），在 manifest 里加字段即可，安装逻辑不用动。
//!
//! 安装是**先校验再落盘**：全部条目路径合法才动磁盘，中途不会留下半个插件。

use std::path::{Path, PathBuf};

use crate::config::{AppConfig, ConfigPaths};
use crate::infra::zip;

/// 装好的一个插件
#[derive(Debug, Clone)]
pub struct Plugin {
    pub name: String,
    pub version: String,
    pub description: String,
    pub dir: PathBuf,
    /// 追加进系统提示词的文件（有才给）
    pub prompt: Option<PathBuf>,
    /// manifest 有问题时记下原因。这种插件照样列出来 —— 否则用户会以为装丢了
    pub problem: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct Manifest {
    name: Option<String>,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
    /// 自定义提示词文件名，默认 `prompt.md`
    #[serde(default)]
    prompt: Option<String>,
}

/// 插件根目录
pub fn plugins_dir(paths: &ConfigPaths) -> PathBuf {
    paths.config_dir.join("plugins")
}

/// 名字要当目录名用，所以严格过滤：只允许字母数字与 `-_.`，且不能以点开头
/// （顺带挡掉 `.`、`..` 这类目录穿越 —— 卸载时同样走这里）。
fn sanitize_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    if name.is_empty() || name.len() > 64 || name.starts_with('.') {
        return None;
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return None;
    }
    Some(name.to_string())
}

/// 安装一个 zip 插件包（同名已装则覆盖，等于升级）。
pub fn install(paths: &ConfigPaths, zip_path: &Path) -> Result<Plugin, String> {
    let bytes =
        std::fs::read(zip_path).map_err(|e| format!("读不了 {}：{e}", zip_path.display()))?;
    let mut entries = zip::read_zip(&bytes)?;
    strip_single_root(&mut entries);

    // ── 1. 找 manifest 并解析
    let manifest_at = entries
        .iter()
        .position(|e| !e.is_dir && is_manifest(&e.name))
        .ok_or("包里没有 plugin.json（或 manifest.json）")?;
    let manifest: Manifest = serde_json::from_slice(&entries[manifest_at].data)
        .map_err(|e| format!("plugin.json 不是合法 JSON：{e}"))?;
    let name = sanitize_name(manifest.name.as_deref().unwrap_or(""))
        .ok_or("plugin.json 里的 name 不合法：只允许字母数字与 - _ .，不能以点开头")?;

    // ── 2. 先把全部要写的文件核一遍，路径有问题的整包拒绝
    let mut files: Vec<(PathBuf, &[u8])> = Vec::new();
    for e in &entries {
        if e.is_dir {
            continue;
        }
        let rel = zip::safe_relative(&e.name)
            .ok_or_else(|| format!("包里有越界路径「{}」，拒绝安装", e.name))?;
        files.push((rel, &e.data));
    }
    if files.is_empty() {
        return Err("包里没有文件".to_string());
    }

    // ── 3. 落盘
    let target = plugins_dir(paths).join(&name);
    let replaced = target.exists();
    if replaced {
        std::fs::remove_dir_all(&target).map_err(|e| format!("清理旧版本失败：{e}"))?;
    }
    std::fs::create_dir_all(&target).map_err(|e| format!("建目录失败：{e}"))?;
    for (rel, data) in &files {
        let dst = target.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败：{e}"))?;
        }
        std::fs::write(&dst, data).map_err(|e| format!("写 {} 失败：{e}", dst.display()))?;
    }

    // ── 4. 认一下提示词文件
    let prompt_rel = manifest.prompt.as_deref().unwrap_or("prompt.md");
    let prompt = zip::safe_relative(prompt_rel)
        .map(|p| target.join(p))
        .filter(|p| p.is_file());

    Ok(Plugin {
        name,
        version: manifest.version,
        description: manifest.description,
        dir: target,
        prompt,
        problem: None,
    })
}

/// 列出装好的插件（按名字排序）。缺 manifest、JSON 坏了的也会列出并说明问题。
pub fn list(paths: &ConfigPaths) -> Vec<Plugin> {
    let dir = plugins_dir(paths);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<Plugin> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|p| load(&p))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 卸载。名字同样过 `sanitize_name` —— 否则 `uninstall ../..` 就能删到别处。
pub fn uninstall(paths: &ConfigPaths, name: &str) -> Result<String, String> {
    let name = sanitize_name(name).ok_or("插件名不合法")?;
    let dir = plugins_dir(paths).join(&name);
    if !dir.is_dir() {
        return Err(format!("没有装过插件「{name}」"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("删除失败：{e}"))?;
    Ok(name)
}

/// 各插件贡献的系统提示词片段：`(插件名, 内容)`。关掉的插件不算。
///
/// 读不出来或空文件跳过 —— 那种插件在 `report` 里已经标了问题，够显眼了。
pub fn prompt_additions(paths: &ConfigPaths, config: &AppConfig) -> Vec<(String, String)> {
    list(paths)
        .into_iter()
        .filter(|p| !is_disabled(config, &p.name))
        .filter_map(|p| {
            let text = std::fs::read_to_string(p.prompt?).ok()?;
            let text = text.trim().to_string();
            (!text.is_empty()).then_some((p.name, text))
        })
        .collect()
}

/// 这个插件被关掉了吗
pub fn is_disabled(config: &AppConfig, name: &str) -> bool {
    config
        .disabled_plugins
        .iter()
        .any(|n| n.eq_ignore_ascii_case(name))
}

/// 开 / 关一个插件。名字没装过就直接说，不动配置。
fn set_enabled(
    paths: &ConfigPaths,
    config: &mut AppConfig,
    name: &str,
    on: bool,
) -> Result<Vec<String>, String> {
    let name = sanitize_name(name).ok_or("插件名不合法")?;
    if !list(paths)
        .iter()
        .any(|p| p.name.eq_ignore_ascii_case(&name))
    {
        return Err(format!("没有装过插件「{name}」（/plugins 看列表）"));
    }
    config.disabled_plugins.retain(|n| !n.eq_ignore_ascii_case(&name));
    if !on {
        config.disabled_plugins.push(name.clone());
    }
    Ok(vec![
        format!("插件 {name} 已{}", if on { "开启" } else { "关闭" }),
        "下次启动（或 /new 之后）生效 —— 同一会话的系统提示词前缀是冻结的".to_string(),
    ])
}

/// CLI（`dsp plugins …`）与界面（`/plugins …`）**共用**的入口。
///
/// 两处都走它，是为了不让命令行和界面各说一套话、各写一套解析 ——
/// 这也是这一版整理代码时合并掉的主要重复。返回要显示的行。
pub fn apply(
    paths: &ConfigPaths,
    config: &mut AppConfig,
    line: &str,
) -> Result<Vec<String>, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let cmd = words.first().copied().unwrap_or("");
    let is_on = |w: &str| w.eq_ignore_ascii_case("enable") || w.eq_ignore_ascii_case("on");
    let is_off = |w: &str| w.eq_ignore_ascii_case("disable") || w.eq_ignore_ascii_case("off");

    // 开关收两种语序：`<名字> enable` 与 `enable <名字>`
    if is_on(cmd) || is_off(cmd) {
        let name = words
            .get(1)
            .copied()
            .ok_or("用法：/plugins <名字> enable|disable")?;
        return set_enabled(paths, config, name, is_on(cmd));
    }
    if words.len() >= 2 && (is_on(words[1]) || is_off(words[1])) {
        return set_enabled(paths, config, words[0], is_on(words[1]));
    }

    match cmd {
        // 路径可能带空格，所以剩下的整串当参数（不按词切）
        "install" => {
            let arg = line
                .trim_start_matches(|c: char| c.is_whitespace())
                .strip_prefix("install")
                .unwrap_or("")
                .trim();
            if arg.is_empty() {
                return Err("用法：/plugins install <插件包.zip>".to_string());
            }
            let p = install(paths, std::path::Path::new(arg))?;
            let ver = if p.version.is_empty() {
                "——".to_string()
            } else {
                p.version.clone()
            };
            let mut out = vec![format!("已安装插件 {} v{ver}", p.name)];
            if !p.description.is_empty() {
                out.push(format!("  {}", p.description));
            }
            out.push(format!("  装在 {}", p.dir.display()));
            out.push(match &p.prompt {
                Some(f) => format!(
                    "  提示词 {} —— 下次启动（或 /new 之后）追加进系统提示词",
                    f.file_name().unwrap_or_default().to_string_lossy()
                ),
                None => "  包里没有 prompt.md，装上但不会改变行为".to_string(),
            });
            Ok(out)
        }
        "uninstall" | "remove" => {
            let name = words.get(1).copied().ok_or("用法：/plugins uninstall <名字>")?;
            Ok(vec![format!("已卸载插件 {}", uninstall(paths, name)?)])
        }
        "" => Ok(report(paths, config)),
        other => Err(format!(
            "不认识「{other}」。可用：/plugins · <名字> enable|disable · install <包.zip> · uninstall <名字>"
        )),
    }
}

/// 列表文本：**开关状态 + 名字 + 版本号 + 描述**。
/// CLI（`dsp plugins`）与界面（`/plugins`）共用这一份，免得两处说法不一致。
pub fn report(paths: &ConfigPaths, config: &AppConfig) -> Vec<String> {
    let dir = plugins_dir(paths);
    let items = list(paths);
    if items.is_empty() {
        return vec![
            format!("还没装插件。插件目录：{}", dir.display()),
            "装一个：dsp install <插件包.zip>".to_string(),
        ];
    }
    let mut out = vec![format!(
        "已装插件 {}（目录 {}）",
        items.len(),
        dir.display()
    )];
    for p in items {
        let on = !is_disabled(config, &p.name);
        let ver = if p.version.is_empty() {
            "——".to_string()
        } else {
            p.version.clone()
        };
        let warn = if p.problem.is_some() { " ⚠" } else { "" };
        out.push(format!(
            "  [{}] {:<22} v{:<10}{warn}",
            if on { "开" } else { "关" },
            p.name,
            ver
        ));
        if !p.description.is_empty() {
            out.push(format!("         {}", p.description));
        }
        if p.prompt.is_some() && !on {
            out.push("         （已关闭：不参与系统提示词）".to_string());
        }
        if let Some(pr) = &p.prompt {
            if on {
                out.push(format!(
                    "         提示词 {}",
                    pr.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
        if let Some(prob) = &p.problem {
            out.push(format!("         问题：{prob}"));
        }
    }
    out.push("开关：/plugins <名字> enable|disable（dsp plugins 同理）".to_string());
    out.push("改动在下次启动（或 /new 之后）生效。".to_string());
    out
}

fn is_manifest(name: &str) -> bool {
    // 两种分隔符都切：Windows 打的包（Compress-Archive）写的是反斜杠
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.eq_ignore_ascii_case("plugin.json") || base.eq_ignore_ascii_case("manifest.json")
}

fn load(dir: &Path) -> Option<Plugin> {
    let name = dir.file_name()?.to_string_lossy().to_string();
    let mut plugin = Plugin {
        name: name.clone(),
        version: String::new(),
        description: String::new(),
        dir: dir.to_path_buf(),
        prompt: None,
        problem: None,
    };

    let found = ["plugin.json", "manifest.json"]
        .iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file());
    let mut prompt_rel = "prompt.md".to_string();
    match found {
        None => plugin.problem = Some("缺 plugin.json".to_string()),
        Some(f) => {
            match std::fs::read_to_string(&f).and_then(|t| {
                serde_json::from_str::<Manifest>(&t).map_err(std::io::Error::other)
            }) {
                Err(e) => plugin.problem = Some(format!("plugin.json 读不了：{e}")),
                Ok(m) => {
                    plugin.version = m.version;
                    plugin.description = m.description;
                    if let Some(n) = m.name.as_deref().and_then(sanitize_name) {
                        plugin.name = n;
                    }
                    if let Some(p) = m.prompt {
                        prompt_rel = p;
                    }
                }
            }
        }
    }

    if let Some(rel) = zip::safe_relative(&prompt_rel) {
        let p = dir.join(rel);
        if p.is_file() {
            plugin.prompt = Some(p);
        }
    }
    Some(plugin)
}

/// 有些打包方式会套一层顶层目录（`Compress-Archive -Path 目录` 就是），
/// 那就剥掉 —— 否则 manifest 找不到，装出来还会多套一层。
///
/// 只在**所有**条目共享同一个第一段、且条目不止一个时才动手，
/// 免得把「根目录下就一个文件」的包剥成空。
fn strip_single_root(entries: &mut Vec<zip::Entry>) {
    if entries.len() < 2 {
        return;
    }
    let first = |name: &str| name.split(['/', '\\']).next().unwrap_or("").to_string();
    let Some(root) = entries.first().map(|e| first(&e.name)) else {
        return;
    };
    if root.is_empty() || !entries.iter().all(|e| first(&e.name) == root) {
        return;
    }
    let cut = root.len() + 1;
    for e in entries.iter_mut() {
        e.name = if e.name.len() > cut {
            e.name[cut..].to_string()
        } else {
            String::new()
        };
    }
    entries.retain(|e| !e.name.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_filter_is_strict() {
        assert_eq!(sanitize_name("my-plugin"), Some("my-plugin".to_string()));
        assert_eq!(sanitize_name("a.b_1"), Some("a.b_1".to_string()));
        // 目录穿越与分隔符一律拒绝 —— 安装和卸载都靠它兜底
        assert_eq!(sanitize_name(".."), None);
        assert_eq!(sanitize_name("../x"), None);
        assert_eq!(sanitize_name(".hidden"), None);
        assert_eq!(sanitize_name("a/b"), None);
        assert_eq!(sanitize_name("a\\b"), None);
        assert_eq!(sanitize_name("  "), None);
        assert_eq!(sanitize_name("a b"), None);
    }

    /// 套了一层顶层目录的包要能剥掉，但「只有一个文件的包」不能剥成空
    #[test]
    fn strips_one_wrapping_dir_only_when_safe() {
        let mk = |names: &[&str]| -> Vec<zip::Entry> {
            names
                .iter()
                .map(|n| zip::Entry {
                    name: (*n).to_string(),
                    data: Vec::new(),
                    is_dir: n.ends_with('/'),
                })
                .collect()
        };

        let mut a = mk(&["plug/", "plug/plugin.json", "plug/prompt.md"]);
        strip_single_root(&mut a);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].name, "plugin.json");

        let mut b = mk(&["plugin.json", "prompt.md"]);
        strip_single_root(&mut b);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].name, "plugin.json");

        let mut c = mk(&["only.txt"]);
        strip_single_root(&mut c);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "only.txt");
    }
}
