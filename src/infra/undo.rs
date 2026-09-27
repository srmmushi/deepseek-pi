//! 改动留底与回退（`/undo`）
//!
//! 参考成熟 agent 的做法（Aider 的 `/undo`）：回退单位是**一轮**，不是单个文件、
//! 也不是时间快照 —— agent 改砸了，一次退掉「这一轮动的全部文件」才有用。
//!
//! 做法很朴素，不需要 git：`write` / `edit` 真正落盘**之前**，先把目标文件当前内容
//! 复制到 `<配置目录>/undo/<会话 id>/`。**每次动手都留一份底**，所以：
//!
//! - `/undo`（退最后一轮）= 把那一轮碰过的文件恢复成「那一轮动手之前」的样子；
//! - `/undo all` = 每个文件只恢复它**最早**那份底，也就是本会话开始时的样子。
//!
//! 一位只留第一次的底是不够的：文件第一轮改过、第二轮又改了，退第二轮时
//! 它也得跟着回去 —— 那种情况恰恰最需要退。原本不存在的文件记 `backup: null`，
//! 回退时把它删掉。
//!
//! 边界要说清楚：只覆盖 `write` / `edit` 两个工具。`exec` 里跑的
//! `git checkout`、`rm`、构建产物之类，这里管不到。

use std::path::{Path, PathBuf};

/// 一个文件的留底记录
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    /// 第几轮（从 1 开始）。`/undo` 退的就是最后一轮
    pub turn: u32,
    /// 被改的文件的绝对路径
    pub path: PathBuf,
    /// 备份文件名；`None` = 改之前这个文件不存在
    pub backup: Option<String>,
}

/// 一个会话的改动账本
#[derive(Debug)]
pub struct Undo {
    dir: PathBuf,
    turn: u32,
    entries: Vec<Entry>,
}

impl Undo {
    /// 打开（或新建）某个会话的账本
    pub fn open(dir: PathBuf) -> Self {
        let entries: Vec<Entry> = crate::config::read_json(&dir.join("index.json")).unwrap_or_default();
        let turn = entries.iter().map(|e| e.turn).max().unwrap_or(0);
        Self {
            dir,
            turn,
            entries,
        }
    }

    /// 记下「新一轮开始」。由 `run_turn` 在开头调用。
    pub fn begin_turn(&mut self) {
        self.turn += 1;
    }

    /// 留底。由工具线程调用，所以整体在锁里 —— 见 `AgentRuntime.undo`。
    ///
    /// 每次动手都留一份：只有每次都有底，`/undo` 才能把「最后一轮」退干净
    /// （包括那些前几轮就已经改过的老文件）。
    ///
    /// 注：同一批并行工具里如果对同一个文件动两次，留底与写入不是原子的，
    /// 底可能取得偏后一点。这种情况罕见，且最多让 `/undo` 少退半步。
    pub fn snapshot(&mut self, abs: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let backup = if abs.is_file() {
            let name = format!("{}_{}", self.entries.len() + 1, file_stem_of(abs));
            std::fs::copy(abs, self.dir.join(&name))?;
            Some(name)
        } else {
            None
        };
        self.entries.push(Entry {
            turn: self.turn,
            path: abs.to_path_buf(),
            backup,
        });
        self.save_index()
    }

    /// 记录条数（界面提示用）
    pub fn tracked(&self) -> usize {
        self.entries.len()
    }

    /// 最近一轮留下过底的文件
    pub fn last_turn_files(&self) -> Vec<PathBuf> {
        let Some(last) = self.entries.iter().map(|e| e.turn).max() else {
            return Vec::new();
        };
        self.entries
            .iter()
            .filter(|e| e.turn == last)
            .map(|e| e.path.clone())
            .collect()
    }

    /// 退掉最后一轮。返回「恢复成了什么样」的说明行。
    pub fn revert_last(&mut self) -> Result<Vec<String>, String> {
        let Some(last) = self.entries.iter().map(|e| e.turn).max() else {
            return Err("这个会话还没有记录到任何文件改动".to_string());
        };
        let (doomed, keep): (Vec<Entry>, Vec<Entry>) =
            self.entries.iter().cloned().partition(|e| e.turn == last);
        let lines = restore_all(&self.dir, &doomed)?;
        self.entries = keep;
        self.save_index().map_err(|e| e.to_string())?;
        Ok(lines)
    }

    /// 退掉整个会话记录到的全部改动：每个文件只恢复它**最早**那份底
    pub fn revert_all(&mut self) -> Result<Vec<String>, String> {
        if self.entries.is_empty() {
            return Err("这个会话还没有记录到任何文件改动".to_string());
        }
        let mut seen = std::collections::HashSet::new();
        let firsts: Vec<Entry> = self
            .entries
            .iter()
            .filter(|e| seen.insert(e.path.clone()))
            .cloned()
            .collect();
        let lines = restore_all(&self.dir, &firsts)?;
        self.entries.clear();
        self.save_index().map_err(|e| e.to_string())?;
        Ok(lines)
    }

    fn save_index(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        // write_json 走的是 anyhow，这里统一成 io::Error —— 调用方只关心「存不下」
        crate::config::write_json(&self.dir.join("index.json"), &self.entries)
            .map_err(std::io::Error::other)
    }
}

/// 倒序恢复（同一个文件多轮改过时，倒着来才能落回最早的底）
fn restore_all(dir: &Path, entries: &[Entry]) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    for e in entries.iter().rev() {
        match &e.backup {
            Some(b) => {
                std::fs::copy(dir.join(b), &e.path)
                    .map_err(|x| format!("恢复 {} 失败：{x}", e.path.display()))?;
                lines.push(format!("已恢复 {}", e.path.display()));
            }
            None => {
                if e.path.exists() {
                    std::fs::remove_file(&e.path)
                        .map_err(|x| format!("删除 {} 失败：{x}", e.path.display()))?;
                    lines.push(format!("已删除（原本不存在）{}", e.path.display()));
                }
            }
        }
    }
    Ok(lines)
}

/// 备份文件名用：只保留文件名部分，且压掉不适合当文件名的字符
fn file_stem_of(p: &Path) -> String {
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dsp-undo-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// 改之前不存在的文件：回退就是删掉它
    #[test]
    fn revert_removes_files_that_did_not_exist() {
        let root = tmp("new-file");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let target = work.join("new.txt");

        let mut u = Undo::open(root.join("undo"));
        u.begin_turn();
        u.snapshot(&target).unwrap();
        std::fs::write(&target, "agent 写的").unwrap();

        let lines = u.revert_last().unwrap();
        assert_eq!(lines.len(), 1);
        assert!(!target.exists(), "原本不存在，回退后应当被删掉");
    }

    /// 改之前存在的文件：回退恢复原内容
    #[test]
    fn revert_restores_previous_content() {
        let root = tmp("existing-file");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let target = work.join("a.txt");
        std::fs::write(&target, "原内容").unwrap();

        let mut u = Undo::open(root.join("undo"));
        u.begin_turn();
        u.snapshot(&target).unwrap();
        std::fs::write(&target, "被改坏了").unwrap();

        u.revert_last().unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "原内容");
    }

    /// 关键语义：老文件在第二轮又被改，退第二轮时它也得跟着回去
    /// （只留第一次的底会漏掉它 —— 这正是当初写错、被这条测试抓出来的地方）
    #[test]
    fn undo_one_turn_also_restores_files_touched_in_earlier_turns() {
        let root = tmp("turns");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let a = work.join("a.txt");
        let b = work.join("b.txt");
        std::fs::write(&a, "a 原").unwrap();
        std::fs::write(&b, "b 原").unwrap();

        let mut u = Undo::open(root.join("undo"));
        u.begin_turn();
        u.snapshot(&a).unwrap();
        std::fs::write(&a, "a 第一轮").unwrap();

        u.begin_turn();
        u.snapshot(&b).unwrap();
        u.snapshot(&a).unwrap();
        std::fs::write(&a, "a 第二轮").unwrap();
        std::fs::write(&b, "b 第二轮").unwrap();

        assert_eq!(u.tracked(), 3, "每次动手都留一份底");
        u.revert_last().unwrap();
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            "a 第一轮",
            "退第二轮：a 回到第二轮动手之前"
        );
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b 原");

        // 再退一次才轮到第一轮
        u.revert_last().unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "a 原");
    }

    /// `/undo all`：一个文件只恢复最早那份底（会话开始时的样子）
    #[test]
    fn revert_all_goes_back_to_session_start() {
        let root = tmp("all");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let a = work.join("a.txt");
        std::fs::write(&a, "v0").unwrap();

        let mut u = Undo::open(root.join("undo"));
        for v in ["v1", "v2", "v3"] {
            u.begin_turn();
            u.snapshot(&a).unwrap();
            std::fs::write(&a, v).unwrap();
        }
        assert_eq!(u.tracked(), 3);
        u.revert_all().unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "v0");
        assert!(u.revert_all().is_err(), "账本清空后应当明说没得退");
    }

    /// 没有留底时给一句人话，而不是空操作
    #[test]
    fn reverting_nothing_says_so() {
        let root = tmp("empty");
        let mut u = Undo::open(root.join("undo"));
        assert!(u.revert_last().unwrap_err().contains("还没有记录"));
    }
}
