//! Git LFS 指针解析
//!
//! 解析 HF Hub 返回的 LFS 指针文件,提取 oid 和 size。
//!
//! LFS 指针格式:
//! ```text
//! version https://git-lfs.github.com/spec/v1
//! oid sha256:abc123...
//! size 12345
//! ```

/// Git LFS 指针信息
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LfsPointer {
    /// LFS oid (sha256:<hex>)
    pub oid: String,
    /// 文件实际大小(字节)
    pub size: u64,
}

/// 判断数据是否是 Git LFS 指针格式
///
/// 检查第一行是否以 "version https://git-lfs.github.com/spec/v1" 开头。
pub fn is_lfs_pointer(data: &[u8]) -> bool {
    if data.len() < 50 {
        return false;
    }
    data.starts_with(b"version https://git-lfs.github.com/spec/v1")
}

/// 解析 Git LFS 指针
///
/// 从形如 `oid sha256:<hash>\nsize <bytes>\n` 的内容中提取信息。
/// 如果不是有效的 LFS 指针,返回 `None`。
pub fn parse_lfs_pointer(data: &[u8]) -> Option<LfsPointer> {
    if !is_lfs_pointer(data) {
        return None;
    }

    let text = String::from_utf8_lossy(data);
    let mut oid = None;
    let mut size = None;

    for line in text.lines() {
        if let Some(hash) = line.strip_prefix("oid sha256:") {
            oid = Some(hash.trim().to_string());
        } else if let Some(s) = line.strip_prefix("size ") {
            if let Ok(n) = s.trim().parse::<u64>() {
                size = Some(n);
            }
        }
    }

    match (oid, size) {
        (Some(o), Some(s)) => Some(LfsPointer { oid: o, size: s }),
        _ => None,
    }
}

/// 构建 LFS 对象下载 URL
///
/// 格式: `{endpoint}/{repo_id}/resolve/{revision}/{file_path}`
/// 注意: 对于真实的 LFS 文件,HF Hub 会透明处理指针并返回实际内容。
pub fn build_resolve_url(endpoint: &str, repo_id: &str, revision: &str, file_path: &str) -> String {
    format!("{endpoint}/{repo_id}/resolve/{revision}/{file_path}")
}

/// 构建 API 文件树 URL
///
/// 格式: `{endpoint}/api/models/{repo_id}/tree/{revision}?recursive=true`
pub fn build_tree_url(endpoint: &str, repo_id: &str, revision: &str) -> String {
    format!("{endpoint}/api/models/{repo_id}/tree/{revision}?recursive=true")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_lfs_pointer_true() {
        let data = b"version https://git-lfs.github.com/spec/v1\noid sha256:abc123\nsize 1024\n";
        assert!(is_lfs_pointer(data));
    }

    #[test]
    fn test_is_lfs_pointer_short_data() {
        assert!(!is_lfs_pointer(b"short"));
        assert!(!is_lfs_pointer(b""));
    }

    #[test]
    fn test_parse_lfs_pointer_normal() {
        let data =
            b"version https://git-lfs.github.com/spec/v1\noid sha256:abc123def456\nsize 4096\n";
        let result = parse_lfs_pointer(data).unwrap();
        assert_eq!(result.oid, "abc123def456");
        assert_eq!(result.size, 4096);
    }

    #[test]
    fn test_parse_lfs_pointer_missing_fields() {
        let data = b"version https://git-lfs.github.com/spec/v1\noid sha256:abc\n";
        assert!(parse_lfs_pointer(data).is_none());
    }

    #[test]
    fn test_parse_lfs_pointer_not_lfs() {
        assert!(parse_lfs_pointer(b"{\"key\": \"value\"}").is_none());
    }

    #[test]
    fn test_build_resolve_url() {
        let url = build_resolve_url(
            "https://huggingface.co",
            "bert-base-uncased",
            "main",
            "config.json",
        );
        assert_eq!(
            url,
            "https://huggingface.co/bert-base-uncased/resolve/main/config.json"
        );
    }

    #[test]
    fn test_build_tree_url() {
        let url = build_tree_url("https://huggingface.co", "bert-base-uncased", "main");
        assert_eq!(
            url,
            "https://huggingface.co/api/models/bert-base-uncased/tree/main?recursive=true"
        );
    }
}
