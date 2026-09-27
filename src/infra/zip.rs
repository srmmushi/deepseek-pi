//! 极简 ZIP 读取器 —— 只做「解开一个 zip 包」这一件事。
//!
//! 为什么不拉一个 zip 库：为了解压插件包而多引入一个依赖不划算，而 zip 的
//! 结构足够简单 —— 从尾部找到中央目录，然后逐个条目取数据。deflate 直接借
//! `flate2`：它本来就在依赖树里（`image` 解 png 用到），所以不新增下载。
//!
//! 支持：存储（method 0）与 deflate（method 8）；数据描述符（大小以中央目录
//! 为准）；带前置目录的包。不支持：加密、多卷、zip64 大尺寸 —— 插件包不该
//! 有这些需求，遇到就明确报错，不猜。

use std::path::PathBuf;

const EOCD_SIG: u32 = 0x0605_4b50;
const CD_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;

/// zip 里的一条记录
#[derive(Debug, Clone)]
pub struct Entry {
    /// 包内的相对路径，如 `my-plugin/plugin.json`
    pub name: String,
    pub data: Vec<u8>,
    pub is_dir: bool,
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(i)?,
        *b.get(i + 1)?,
        *b.get(i + 2)?,
        *b.get(i + 3)?,
    ]))
}

/// 把包内条目名折成「相对目标目录的安全路径」。
///
/// 这是防 zip-slip 的地方：`../../.ssh/authorized_keys` 这类名字若直接 join，
/// 就会写到目标目录之外。规则是从严 —— 只要出现 `..` 或盘符，整条拒绝，
/// 而不是悄悄把危险组件过滤掉（那样会得到一个位置不对的文件，更难排查）。
pub fn safe_relative(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in name.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        // 盘符（C:）与 NTFS 备用数据流（file:stream）都用冒号，一律拒绝
        if part.contains(':') {
            return None;
        }
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 解开一个 zip 包，按包内顺序返回全部条目。
pub fn read_zip(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    // ── 1. 从尾部找 EOCD。注释最长 65535 字节，所以回溯范围是有限的
    if bytes.len() < 22 {
        return Err("文件太小，不是 zip".to_string());
    }
    let lowest = bytes.len().saturating_sub(22 + 0xFFFF);
    let mut eocd = None;
    let mut i = bytes.len() - 22;
    loop {
        if u32_at(bytes, i) == Some(EOCD_SIG) {
            eocd = Some(i);
            break;
        }
        if i == lowest {
            break;
        }
        i -= 1;
    }
    let eocd = eocd.ok_or("不是 zip：找不到中央目录结束标记（EOCD）")?;

    let count = u16_at(bytes, eocd + 10).ok_or("zip 头被截断")? as usize;
    let cd_size = u32_at(bytes, eocd + 12).ok_or("zip 头被截断")? as usize;
    let cd_off = u32_at(bytes, eocd + 16).ok_or("zip 头被截断")? as usize;
    if cd_off.saturating_add(cd_size) > bytes.len() {
        return Err("zip 中央目录越界".to_string());
    }

    // ── 2. 顺着中央目录逐条取数据（大小以它为准，兼容带数据描述符的包）
    let mut out = Vec::with_capacity(count);
    let mut p = cd_off;
    for _ in 0..count {
        if u32_at(bytes, p) != Some(CD_SIG) {
            break;
        }
        let method = u16_at(bytes, p + 10).ok_or("zip 条目头被截断")?;
        let comp_size = u32_at(bytes, p + 20).ok_or("zip 条目头被截断")? as usize;
        let name_len = u16_at(bytes, p + 28).ok_or("zip 条目头被截断")? as usize;
        let extra_len = u16_at(bytes, p + 30).ok_or("zip 条目头被截断")? as usize;
        let comment_len = u16_at(bytes, p + 32).ok_or("zip 条目头被截断")? as usize;
        let local_off = u32_at(bytes, p + 42).ok_or("zip 条目头被截断")? as usize;

        let name_bytes = bytes
            .get(p + 46..p + 46 + name_len)
            .ok_or("zip 文件名越界")?;
        let name = String::from_utf8_lossy(name_bytes).to_string();

        // 本地头的 name/extra 长度可能与中央目录不同，所以必须重读它
        if u32_at(bytes, local_off) != Some(LOCAL_SIG) {
            return Err(format!("条目「{name}」的本地头不对，zip 可能损坏"));
        }
        let l_name = u16_at(bytes, local_off + 26).ok_or("zip 本地头被截断")? as usize;
        let l_extra = u16_at(bytes, local_off + 28).ok_or("zip 本地头被截断")? as usize;
        let data_off = local_off + 30 + l_name + l_extra;
        let raw = bytes
            .get(data_off..data_off.saturating_add(comp_size))
            .ok_or_else(|| format!("条目「{name}」的数据越界"))?;

        let data = match method {
            0 => raw.to_vec(),
            8 => inflate(raw)?,
            m => return Err(format!("条目「{name}」用了不支持的压缩方式 {m}（只支持存储与 deflate）")),
        };
        out.push(Entry {
            name: name.clone(),
            is_dir: name.ends_with('/'),
            data,
        });
        p += 46 + name_len + extra_len + comment_len;
    }

    if out.is_empty() {
        return Err("zip 里没有条目".to_string());
    }
    Ok(out)
}

/// zip 的 method 8 是**裸 deflate**（不是 zlib 包装），所以用 DeflateDecoder
fn inflate(raw: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(raw)
        .read_to_end(&mut out)
        .map_err(|e| format!("解压失败：{e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_relative_keeps_normal_paths() {
        assert_eq!(
            safe_relative("plug/plugin.json"),
            Some(PathBuf::from("plug").join("plugin.json"))
        );
        // 前导 ./ 与重复斜杠都无害
        assert_eq!(safe_relative("./a//b.txt"), Some(PathBuf::from("a").join("b.txt")));
        // 反斜杠按分隔符处理（Windows 打包的 zip 常见）
        assert_eq!(safe_relative("a\\b.txt"), Some(PathBuf::from("a").join("b.txt")));
    }

    /// zip-slip：想往目标目录外写的一律拒绝，而不是「过滤掉 .. 了事」
    #[test]
    fn safe_relative_rejects_escapes() {
        assert_eq!(safe_relative("../etc/passwd"), None);
        assert_eq!(safe_relative("a/../../b"), None);
        assert_eq!(safe_relative("C:/Windows/x.dll"), None);
        assert_eq!(safe_relative("a/b:stream"), None);
        assert_eq!(safe_relative(""), None);
        assert_eq!(safe_relative("./"), None);
    }

    #[test]
    fn not_a_zip_reports_clearly() {
        assert!(read_zip(b"").is_err());
        assert!(read_zip(&[0u8; 64]).unwrap_err().contains("不是 zip"));
    }
}
