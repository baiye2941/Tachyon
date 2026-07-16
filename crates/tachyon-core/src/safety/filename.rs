//! 文件名提取与 Content-Disposition 解析
//!
//! 提供统一的文件名提取入口与路径穿越防护:
//! - `extract_filename_from_url` — 从 URL 路径提取文件名(含 percent-decode)
//! - `parse_content_disposition` — 解析 Content-Disposition 头
//! - `extract_filename` — 先尝试 Content-Disposition,再回退到 URL
//! - `sanitize_filename` — 清洗文件名防止路径遍历
//! - `validate_save_path` — 纵深防御的保存路径校验

// ---------------------------------------------------------------------------
// 文件名处理
// ---------------------------------------------------------------------------

/// 清洗文件名,防止路径遍历攻击
///
/// A-01: 合并为两次遍历(路径分隔符替换 + .. 移除 → 一次;trim + 危险字符过滤 → 一次),
/// 消除了中间的 `Vec<char>` 分配和多余的 `String` 创建。
///
/// 安全措施:
/// - 移除所有路径分隔符 (`/`, `\`)
/// - 移除所有 `..` 序列
/// - 移除前导和尾随的空格与点号
/// - 确保结果是纯粹的 basename,不包含任何目录结构信息
///
/// 如果清洗后结果为空,返回 `"unknown"`。
pub fn sanitize_filename(name: &str) -> String {
    // 危险字符集合(位掩码风格内联判断)
    fn is_dangerous(c: char) -> bool {
        matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    }

    // 第一次遍历:替换路径分隔符为空格 + 移除独立 ".." 路径组件
    let mut buf = String::with_capacity(name.len());
    let chars: Vec<char> = name.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        match ch {
            '/' | '\\' => {
                buf.push(' ');
                i += 1;
            }
            '.' if i + 1 < chars.len() && chars[i + 1] == '.' => {
                // 检查 ".." 是否为独立路径组件(两侧为空格或字符串边界)
                let before = buf.chars().last().is_none_or(|c| c == ' ');
                let after_is_boundary = i + 2 >= chars.len()
                    || chars[i + 2] == ' '
                    || chars[i + 2] == '/'
                    || chars[i + 2] == '\\';
                if before && after_is_boundary {
                    i += 2;
                    continue;
                }
                buf.push(ch);
                i += 1;
            }
            _ => {
                buf.push(ch);
                i += 1;
            }
        }
    }

    // 第二次遍历:trim + 过滤危险字符
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return "unknown".to_string();
    }

    let mut filtered = String::with_capacity(trimmed.len());
    for c in trimmed.chars() {
        if !is_dangerous(c) {
            filtered.push(c);
        }
    }

    // 后处理: 移除纯点序列 token (如 "..", "...."),防止变体路径遍历
    let result: String = filtered
        .split_whitespace()
        .filter(|token| !token.chars().all(|c| c == '.'))
        .collect::<Vec<_>>()
        .join(" ");

    if result.is_empty() {
        return "unknown".to_string();
    }

    result
}

/// 从 URL 路径段提取文件名
///
/// 解析 URL 路径的最后一段,并对百分号编码做 UTF-8 解码。
/// 无路径段或解析失败时返回 `"unknown"`。
///
/// **安全特性**: 自动应用路径遍历防护,确保返回的文件名是安全的 basename。
pub fn extract_filename_from_url(url: &str) -> String {
    let raw_name = url::Url::parse(url)
        .ok()
        .and_then(|u| {
            let segment = u.path().rsplit('/').next().unwrap_or("");
            if segment.is_empty() {
                None
            } else {
                percent_decode(segment)
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    sanitize_filename(&raw_name)
}

/// 提取文件名:优先 Content-Disposition,回退到 URL
///
/// 如果 `content_disposition` 为 `None` 或解析失败,
/// 则从 URL 路径提取。
///
/// **安全特性**: 自动应用路径遍历防护,确保返回的文件名是安全的 basename。
pub fn extract_filename(url: &str, content_disposition: Option<&str>) -> String {
    let raw_name = content_disposition
        .and_then(parse_content_disposition)
        .unwrap_or_else(|| extract_filename_from_url(url));

    sanitize_filename(&raw_name)
}

/// 校验保存路径的安全性(纵深防御)
///
/// 确保最终的文件保存路径位于预期的下载目录内,防止:
/// - 符号链接绕过(symlink attack)
/// - 相对路径逃逸(../ 等)
/// - 硬链接指向外部目录
///
/// # 参数
/// - `final_path`: 经过 sanitize_filename 处理后的完整文件路径
/// - `expected_base`: 预期的下载根目录
///
/// # 返回
/// - `Ok(canonical_path)`: 规范化后的绝对路径
/// - `Err(DownloadError::Config)`: 路径校验失败
// 不访问文件系统的逻辑路径规范化：解析 `.` 和 `..` 组件。
fn normalize_logical_path(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut stack: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // 仅弹出普通目录组件，保留根/前缀
                if matches!(stack.last(), Some(Component::Normal(_))) {
                    stack.pop();
                }
            }
            other => stack.push(other),
        }
    }
    stack.iter().collect()
}

/// 审计 SEC-010: 从 `base` 到 `path`(含)逐段检查已存在组件是否为 symlink/reparse。
///
/// 不消除 validate→open 竞态(需 openat2/句柄化);在校验时刻拒绝已观察到的中间目录逃逸。
pub fn reject_symlink_or_reparse_components(
    base: &std::path::Path,
    path: &std::path::Path,
) -> crate::DownloadResult<()> {
    let mut cursor = base.to_path_buf();
    // 始终检查 base 本身
    ensure_path_not_symlink_or_reparse(&cursor)?;

    let Ok(rel) = path.strip_prefix(base) else {
        // path 可能是绝对但尚未以 base 为前缀的逻辑路径;仅检查 path 自身
        return ensure_path_not_symlink_or_reparse(path);
    };

    for component in rel.components() {
        match component {
            std::path::Component::Normal(name) => {
                cursor.push(name);
                if cursor.exists() {
                    ensure_path_not_symlink_or_reparse(&cursor)?;
                }
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if cursor.pop() => {}
            std::path::Component::ParentDir => {
                return Err(crate::DownloadError::Config("路径逃逸基目录".into()));
            }
            _ => {}
        }
    }
    Ok(())
}

fn ensure_path_not_symlink_or_reparse(path: &std::path::Path) -> crate::DownloadResult<()> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| {
        crate::DownloadError::Config(format!("无法读取路径元数据 {}: {e}", path.display()))
    })?;
    if is_symlink_or_reparse_meta(&meta) {
        return Err(crate::DownloadError::Config(format!(
            "路径组件是符号链接/重解析点,拒绝: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn is_symlink_or_reparse_meta(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_symlink_or_reparse_meta(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::{FileTypeExt, MetadataExt};
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let file_type = metadata.file_type();
    file_type.is_symlink_dir()
        || file_type.is_symlink_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

pub fn validate_save_path(
    final_path: &std::path::Path,
    expected_base: &std::path::Path,
) -> crate::DownloadResult<std::path::PathBuf> {
    // 安全模型说明(FIX-09 + SEC-010): canonicalize 基目录与父目录,并对 base→parent 路径上
    // 已存在中间组件做 symlink/reparse 拒绝。路径字符串校验仍不能完全消除 validate→open TOCTOU;
    // 最终组件由 TokioFile/WinFile no-follow 保护。完整句柄化 openat2 仍属残留。
    // 1. 确保基目录存在且可 canonicalize
    let canonical_base = expected_base
        .canonicalize()
        .map_err(|e| crate::DownloadError::Config(format!("下载目录不存在或无法访问: {e}")))?;

    // SEC-010: 拒绝 base 与已存在祖先中的 symlink/reparse(含中间目录)
    reject_symlink_or_reparse_components(&canonical_base, &canonical_base)?;

    // 2. 统一通过父目录 canonicalize + 文件名拼接
    //    避免 TOCTOU 竞态: 不检查 final_path.exists(),防止检查与 canonicalize 之间
    //    被另一进程插入符号链接
    let parent = final_path
        .parent()
        .ok_or_else(|| crate::DownloadError::Config("无效的文件路径: 无父目录".into()))?;

    // SEC-010: 对 final_path 的已存在祖先(相对 expected_base)拒绝中间 symlink
    if let Ok(rel_parent) = parent.strip_prefix(expected_base) {
        let check = expected_base.join(rel_parent);
        reject_symlink_or_reparse_components(expected_base, &check)?;
    } else if parent.exists() {
        // parent 已是绝对存在路径时仍检查其自身 metadata
        ensure_path_not_symlink_or_reparse(parent)?;
    }

    // 3. 先做逻辑路径校验,再创建目录(防止 create_dir_all 副作用在校验失败时残留)
    //    当父目录不存在时,先通过逻辑路径拼接做逃逸检查,再创建目录
    if parent.exists() {
        let canonical_parent = parent
            .canonicalize()
            .map_err(|e| crate::DownloadError::Config(format!("无法解析父目录: {e}")))?;

        reject_symlink_or_reparse_components(&canonical_base, &canonical_parent)?;

        if !canonical_parent.starts_with(&canonical_base) {
            return Err(crate::DownloadError::Config(format!(
                "父目录逃逸检测: {:?} 不在预期目录 {:?} 内",
                canonical_parent, canonical_base
            )));
        }

        // 基于 canonical_parent 构建最终路径,防止 TOCTOU 符号链接攻击
        let file_name = final_path
            .file_name()
            .ok_or_else(|| crate::DownloadError::Config("无效的文件路径: 无文件名".into()))?;
        let result = canonical_parent.join(file_name);

        // 如果路径已存在(例如符号链接),解析实际目标并验证不逃逸出基目录
        if result.exists() {
            let canonical_final = result
                .canonicalize()
                .map_err(|e| crate::DownloadError::Config(format!("无法解析文件路径: {e}")))?;
            if !canonical_final.starts_with(&canonical_base) {
                return Err(crate::DownloadError::Config(format!(
                    "符号链接逃逸检测: {:?} 实际指向 {:?},不在预期目录 {:?} 内",
                    result, canonical_final, canonical_base
                )));
            }
            return Ok(canonical_final);
        }

        Ok(result)
    } else {
        // 父目录不存在: 先做逻辑路径校验, 再创建目录
        let file_name = final_path
            .file_name()
            .ok_or_else(|| crate::DownloadError::Config("无效的文件路径: 无文件名".into()))?;

        // 逻辑校验: parent 必须是 expected_base 的子路径
        let suffix = parent.strip_prefix(expected_base).map_err(|_| {
            crate::DownloadError::Config(format!(
                "父目录逃逸检测(逻辑): {:?} 不在预期目录 {:?} 内",
                parent, expected_base
            ))
        })?;

        // 逻辑规范化 canonical_base + suffix, 解析 . 和 .. 后检查是否仍在 base 内
        let check_path = canonical_base.join(suffix);
        let normalized = normalize_logical_path(&check_path);
        if !normalized.starts_with(&canonical_base) && normalized != canonical_base {
            return Err(crate::DownloadError::Config(format!(
                "父目录逃逸检测(逻辑): {:?} 规范化后不在预期目录 {:?} 内",
                parent, canonical_base
            )));
        }

        // 校验通过, 现在可以安全创建目录
        std::fs::create_dir_all(parent).map_err(crate::DownloadError::Io)?;

        let canonical_parent = parent
            .canonicalize()
            .map_err(|e| crate::DownloadError::Config(format!("创建目录后无法解析父目录: {e}")))?;

        // 创建后再次检查中间组件(SEC-010)
        reject_symlink_or_reparse_components(&canonical_base, &canonical_parent)?;

        Ok(canonical_parent.join(file_name))
    }
}

/// 多文件落盘路径校验
///
/// `file_names` 为各文件的相对路径(可能含子目录,如 "subdir/a.bin"),
/// 顺序对应 file_id。每个文件路径拼到 `base/torrent_name/` 下,
/// 经 `validate_save_path` 做 TOCTOU 防护和逃逸检测,返回各 canonical 路径。
///
/// 安全策略(分组件 sanitize,保留子目录结构):
/// - 按路径分隔符(`/`/`\`)把相对路径拆成组件,逐段 `sanitize_filename`;
///   组件级 sanitize 移除单段内的危险字符,但不再把分隔符压成空格
///   (分隔符在此处用于分割,而非压平),从而保留 "subdir/a.bin" 的目录层级。
/// - 任一组件为 `..` 或 `.` 视为逃逸攻击,直接报错(不接受相对路径穿越)。
/// - 最后 `validate_save_path` 做最终的 canonical 逃逸检测与父目录自动创建,
///   形成纵深防御。
pub fn validate_multi_save_paths(
    base: &std::path::Path,
    torrent_name: &str,
    file_names: &[String],
) -> crate::DownloadResult<Vec<std::path::PathBuf>> {
    let safe_torrent_dir = sanitize_filename(torrent_name);
    let torrent_base = base.join(&safe_torrent_dir);
    let mut paths = Vec::with_capacity(file_names.len());
    for rel in file_names {
        // 按路径分隔符拆成组件,逐段 sanitize,保留子目录结构。
        // OPT-2:旧实现对整条 rel 调 sanitize_filename,把 '/' 压成空格,
        // 导致 "subdir/a.bin" 被扁平化为 "subdir a.bin",丢失目录层级。
        let mut safe_path = torrent_base.clone();
        for raw_component in rel.split(['/', '\\']) {
            // 跳过空组件(连续分隔符 / 首尾分隔符产生的空段)
            if raw_component.is_empty() {
                continue;
            }
            // '.' 与 '..' 在相对路径中是逃逸/无意义段,直接拒绝
            if raw_component == "." || raw_component == ".." {
                return Err(crate::DownloadError::Config(format!(
                    "多文件相对路径含非法组件 '{raw_component}'(逃逸检测): {rel}"
                )));
            }
            // 单组件 sanitize:移除组件内的危险字符(组件内不应再含分隔符,
            // sanitize_filename 会把残留分隔符压成空格,这是期望行为)
            let safe_comp = sanitize_filename(raw_component);
            safe_path.push(safe_comp);
        }
        // 若拆分后无任何有效组件(如 rel 全是分隔符),回退到 unknown 兜底
        if safe_path == torrent_base {
            safe_path.push(sanitize_filename(rel));
        }
        let canonical = validate_save_path(&safe_path, base)?;
        paths.push(canonical);
    }
    Ok(paths)
}

/// 解析 Content-Disposition 头中的文件名
///
/// 支持两种格式:
/// - `filename*=UTF-8''percent_encoded_name` (RFC 5987)
/// - `filename="name"` / `filename=name`
///
/// `filename*` 优先于 `filename`。
pub fn parse_content_disposition(value: &str) -> Option<String> {
    // 使用分号分割参数后精确匹配参数名,避免子串匹配误提取
    // 先遍历查找 filename*= (优先),再回退 filename= (低优先级)
    let mut fallback: Option<String> = None;

    for param in value.split(';') {
        let param = param.trim();
        if let Some(encoded) = param.strip_prefix("filename*=") {
            let mut parts = encoded.splitn(3, '\'');
            let _charset = parts.next(); // 编码名称(如 UTF-8),当前不使用
            let _encoding = parts.next(); // 编码方式(如 '', 当前不使用
            if let Some(encoded_name) = parts.next()
                && let Some(decoded) = percent_decode(encoded_name)
                && !decoded.is_empty()
            {
                return Some(decoded);
            }
        } else if let Some(name) = param.strip_prefix("filename=") {
            let name = name.trim().trim_matches('"');
            if !name.is_empty() && fallback.is_none() {
                fallback = Some(name.to_string());
            }
        }
    }

    fallback
}

fn percent_decode(input: &str) -> Option<String> {
    let mut output = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(byte) = parse_hex_pair(bytes[i + 1], bytes[i + 2]) {
                output.push(byte);
                i += 3;
            } else {
                output.push(bytes[i]);
                i += 1;
            }
        } else {
            output.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn parse_hex_pair(high: u8, low: u8) -> Option<u8> {
    let h = hex_digit(high)?;
    let l = hex_digit(low)?;
    Some(h * 16 + l)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_filename_from_url_basic() {
        assert_eq!(
            extract_filename_from_url("https://example.com/path/to/file.zip"),
            "file.zip"
        );
    }

    #[test]
    fn test_extract_filename_from_url_with_query() {
        assert_eq!(
            extract_filename_from_url("https://example.com/file.zip?v=2&token=abc"),
            "file.zip"
        );
    }

    #[test]
    fn test_extract_filename_from_url_root_path() {
        assert_eq!(extract_filename_from_url("https://example.com/"), "unknown");
    }

    #[test]
    fn test_extract_filename_from_url_no_path() {
        assert_eq!(extract_filename_from_url("https://example.com"), "unknown");
    }

    #[test]
    fn test_extract_filename_from_url_percent_space() {
        assert_eq!(
            extract_filename_from_url("https://example.com/my%20file.txt"),
            "my file.txt"
        );
    }

    #[test]
    fn test_extract_filename_from_url_invalid_hex_preserves_literal() {
        assert_eq!(
            extract_filename_from_url("https://example.com/file%GG.txt"),
            "file%GG.txt"
        );
    }

    #[test]
    fn test_extract_filename_from_url_chinese_percent_encoded() {
        assert_eq!(
            extract_filename_from_url("https://example.com/%E4%B8%AD%E6%96%87.txt"),
            "中文.txt"
        );
    }

    #[test]
    fn test_extract_filename_from_url_invalid_url() {
        assert_eq!(extract_filename_from_url("not a url"), "unknown");
    }

    #[test]
    fn test_extract_filename_from_url_empty() {
        assert_eq!(extract_filename_from_url(""), "unknown");
    }

    #[test]
    fn test_extract_filename_prefers_content_disposition() {
        assert_eq!(
            extract_filename(
                "https://example.com/path/file.zip",
                Some("attachment; filename=\"report.pdf\"")
            ),
            "report.pdf"
        );
    }

    #[test]
    fn test_extract_filename_falls_back_to_url() {
        assert_eq!(
            extract_filename("https://example.com/file.zip", None),
            "file.zip"
        );
    }

    #[test]
    fn test_extract_filename_falls_back_when_disposition_empty() {
        assert_eq!(
            extract_filename("https://example.com/file.zip", Some("inline")),
            "file.zip"
        );
    }

    #[test]
    fn test_parse_content_disposition_with_quotes() {
        assert_eq!(
            parse_content_disposition(r#"attachment; filename="file.zip""#),
            Some("file.zip".to_string())
        );
    }

    #[test]
    fn test_parse_content_disposition_without_quotes() {
        assert_eq!(
            parse_content_disposition("attachment; filename=file.zip"),
            Some("file.zip".to_string())
        );
    }

    #[test]
    fn test_parse_content_disposition_filename_star_utf8() {
        assert_eq!(
            parse_content_disposition("attachment; filename*=UTF-8''my%20file.zip"),
            Some("my file.zip".to_string())
        );
    }

    #[test]
    fn test_parse_content_disposition_filename_star_chinese() {
        assert_eq!(
            parse_content_disposition("attachment; filename*=UTF-8''%E4%B8%AD%E6%96%87.pdf"),
            Some("中文.pdf".to_string())
        );
    }

    #[test]
    fn test_parse_content_disposition_filename_star_priority() {
        assert_eq!(
            parse_content_disposition(
                "attachment; filename=fallback.txt; filename*=UTF-8''real%20name.txt"
            ),
            Some("real name.txt".to_string())
        );
    }

    #[test]
    fn test_parse_content_disposition_empty() {
        assert_eq!(parse_content_disposition(""), None);
    }

    #[test]
    fn test_parse_content_disposition_no_filename() {
        assert_eq!(parse_content_disposition("inline"), None);
    }

    #[test]
    fn test_parse_content_disposition_empty_filename() {
        assert_eq!(
            parse_content_disposition(r#"attachment; filename="""#),
            None
        );
    }

    #[test]
    fn test_parse_content_disposition_trailing_semicolon() {
        assert_eq!(
            parse_content_disposition("attachment; filename=test.zip;"),
            Some("test.zip".to_string())
        );
    }

    #[test]
    fn test_parse_content_disposition_rejects_substring_attack() {
        assert_eq!(
            parse_content_disposition("attachment; xfilename*=UTF-8''evil.exe; filename=safe.pdf"),
            Some("safe.pdf".to_string())
        );
        assert_eq!(
            parse_content_disposition("attachment; xfilename=evil.exe; filename=safe.pdf"),
            Some("safe.pdf".to_string())
        );
    }

    #[test]
    fn test_percent_decode_multi_byte_utf8() {
        assert_eq!(
            percent_decode("%E4%B8%AD%E6%96%87"),
            Some("中文".to_string())
        );
    }

    #[test]
    fn test_percent_decode_no_encoding() {
        assert_eq!(
            percent_decode("filename.zip"),
            Some("filename.zip".to_string())
        );
    }

    #[test]
    fn test_percent_decode_invalid_utf8_returns_none() {
        assert_eq!(percent_decode("%FF%FE"), None);
    }

    #[test]
    fn test_sanitize_filename_basic() {
        assert_eq!(sanitize_filename("file.zip"), "file.zip");
    }

    #[test]
    fn test_sanitize_filename_path_traversal_dotdot() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "etc passwd");
    }

    #[test]
    fn test_sanitize_filename_path_traversal_slash() {
        assert_eq!(sanitize_filename("foo/bar/baz.txt"), "foo bar baz.txt");
    }

    #[test]
    fn test_sanitize_filename_path_traversal_backslash() {
        assert_eq!(sanitize_filename("foo\\bar\\baz.txt"), "foo bar baz.txt");
    }

    #[test]
    fn test_sanitize_filename_mixed_traversal() {
        assert_eq!(
            sanitize_filename("../..\\windows/system32"),
            "windows system32"
        );
    }

    #[test]
    fn test_sanitize_filename_only_dots() {
        assert_eq!(sanitize_filename("..."), "unknown");
    }

    #[test]
    fn test_sanitize_filename_only_slashes() {
        assert_eq!(sanitize_filename("///"), "unknown");
    }

    #[test]
    fn test_sanitize_filename_empty() {
        assert_eq!(sanitize_filename(""), "unknown");
    }

    #[test]
    fn test_sanitize_filename_complex_traversal() {
        assert_eq!(
            sanitize_filename("....//....//....//etc/passwd"),
            "etc passwd"
        );
    }

    #[test]
    fn test_sanitize_filename_windows_reserved_chars() {
        assert_eq!(sanitize_filename("file:name*test?.txt"), "filenametest.txt");
    }

    #[test]
    fn test_extract_filename_from_url_traversal() {
        assert_eq!(
            extract_filename_from_url("https://example.com/../../etc/passwd"),
            "passwd"
        );
    }

    #[test]
    fn test_extract_filename_content_disposition_traversal() {
        assert_eq!(
            extract_filename(
                "https://example.com/file.zip",
                Some("attachment; filename=\"../../etc/passwd\"")
            ),
            "etc passwd"
        );
    }

    #[test]
    fn test_extract_filename_normal_file() {
        assert_eq!(
            extract_filename("https://example.com/document.pdf", None),
            "document.pdf"
        );
    }

    #[test]
    fn test_extract_filename_with_spaces() {
        assert_eq!(
            extract_filename("https://example.com/my%20document.pdf", None),
            "my document.pdf"
        );
    }

    #[test]
    fn test_extract_filename_complex_traversal() {
        assert_eq!(
            extract_filename(
                "https://example.com/safe.txt",
                Some("attachment; filename=\"../../../Windows/System32/config/sam\"")
            ),
            "Windows System32 config sam"
        );
    }

    #[test]
    fn test_validate_save_path_normal_file() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();
        let final_path = base.join("document.pdf");
        std::fs::write(&final_path, b"test").unwrap();

        let result = validate_save_path(&final_path, &base);
        assert!(result.is_ok(), "正常文件应通过校验");
    }

    #[test]
    fn test_validate_save_path_new_file_in_existing_dir() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();
        let final_path = base.join("new_file.txt");

        let result = validate_save_path(&final_path, &base);
        assert!(result.is_ok(), "新文件在合法目录内应通过校验");
    }

    #[test]
    fn test_validate_save_path_creates_parent_dir() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();
        let final_path = base.join("subdir").join("file.txt");

        let result = validate_save_path(&final_path, &base);
        assert!(result.is_ok(), "应自动创建子目录");
        assert!(base.join("subdir").exists(), "子目录应被创建");
    }

    #[test]
    fn test_validate_save_path_dotdot_escape() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();

        let malicious = base
            .join("..")
            .join("..")
            .join("..")
            .join("etc")
            .join("passwd");
        let result = validate_save_path(&malicious, &base);
        assert!(result.is_err(), "应阻止父目录逃逸");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("逃逸检测") || err_msg.contains("不在预期目录"),
            "错误信息应说明逃逸检测: {err_msg}"
        );
    }

    #[test]
    fn test_validate_save_path_deep_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();

        let malicious = base
            .join("..")
            .join("..")
            .join("..")
            .join("..")
            .join("etc")
            .join("shadow");
        let result = validate_save_path(&malicious, &base);
        assert!(result.is_err(), "深层遍历应被阻止");
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_save_path_symlink_to_outside() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();

        let outside_file = temp.path().join("secret.txt");
        std::fs::write(&outside_file, b"secret data").unwrap();
        let evil_link = base.join("innocent.txt");
        symlink(&outside_file, &evil_link).unwrap();

        let result = validate_save_path(&evil_link, &base);
        assert!(result.is_err(), "符号链接指向外部应被阻止");
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_save_path_symlink_inside_allowed() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();

        let real_file = base.join("real.txt");
        std::fs::write(&real_file, b"data").unwrap();
        let link = base.join("link.txt");
        symlink(&real_file, &link).unwrap();

        let result = validate_save_path(&link, &base);
        assert!(result.is_ok(), "内部符号链接应被允许");
    }

    #[cfg(windows)]
    #[test]
    fn test_validate_save_path_symlink_to_outside_windows() {
        let base_temp = tempfile::tempdir().unwrap();
        let base = base_temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();

        let outside_temp = tempfile::tempdir().unwrap();
        let outside_file = outside_temp.path().join("secret.txt");
        std::fs::write(&outside_file, b"secret data").unwrap();
        let evil_link = base.join("innocent.txt");

        if std::os::windows::fs::symlink_file(&outside_file, &evil_link).is_ok()
            && evil_link.is_symlink()
        {
            let result = validate_save_path(&evil_link, &base);
            assert!(result.is_err(), "符号链接指向外部应被阻止");
        }
    }

    #[test]
    fn test_validate_save_path_nonexistent_base() {
        let base = std::path::Path::new("/nonexistent/directory/that/does/not/exist");
        let final_path = base.join("file.txt");
        let result = validate_save_path(&final_path, base);
        assert!(result.is_err(), "不存在的基目录应报错");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("不存在") || err_msg.contains("无法访问"),
            "错误信息应说明目录问题: {err_msg}"
        );
    }

    #[test]
    fn test_validate_save_path_unicode_filename() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();
        let final_path = base.join("中文文件.zip");
        std::fs::write(&final_path, b"test").unwrap();

        let result = validate_save_path(&final_path, &base);
        assert!(result.is_ok(), "Unicode 文件名应通过校验");
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_save_path_rejects_middle_symlink() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let mid_link = base.join("mid");
        symlink(&outside, &mid_link).unwrap();
        // 中间目录是 symlink,目标文件在 symlink 下
        let final_path = mid_link.join("file.bin");
        let result = validate_save_path(&final_path, &base);
        assert!(result.is_err(), "中间目录 symlink 应被拒绝: {result:?}");
    }

    #[cfg(unix)]
    #[test]
    fn test_reject_symlink_or_reparse_components_ok_on_normal() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();
        let sub = base.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        reject_symlink_or_reparse_components(&base, &sub).unwrap();
    }

    #[test]
    fn test_validate_save_path_filename_with_spaces() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();
        let final_path = base.join("my document file.txt");
        std::fs::write(&final_path, b"test").unwrap();

        let result = validate_save_path(&final_path, &base);
        assert!(result.is_ok(), "带空格的文件名应通过校验");
    }

    #[test]
    fn test_validate_save_path_returns_canonical() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();
        let final_path = base.join("file.txt");
        std::fs::write(&final_path, b"test").unwrap();

        let result = validate_save_path(&final_path, &base).unwrap();
        assert!(result.is_absolute(), "返回路径应为绝对路径: {:?}", result);
    }

    #[tokio::test]
    async fn test_validate_save_path_concurrent_access() {
        use std::sync::Arc;

        let temp = Arc::new(tempfile::tempdir().unwrap());
        let base = Arc::new(temp.path().join("downloads"));
        std::fs::create_dir(&*base).unwrap();

        let mut handles = Vec::new();
        for i in 0..20 {
            let base = Arc::clone(&base);
            handles.push(tokio::spawn(async move {
                let final_path = base.join(format!("file_{i}.txt"));
                validate_save_path(&final_path, &base)
            }));
        }

        for handle in handles {
            let result = handle.await.unwrap();
            assert!(result.is_ok(), "并发访问不应失败");
        }
    }

    // -----------------------------------------------------------------------
    // P1: validate_save_path / parse_content_disposition / percent_decode 边界
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_save_path_nonexistent_parent_under_base() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();
        let final_path = base.join("deep").join("nested").join("file.txt");

        assert!(!final_path.parent().unwrap().exists());
        let result = validate_save_path(&final_path, &base);
        assert!(result.is_ok(), "基目录内的不存在的父目录应被自动创建");
        assert!(base.join("deep").join("nested").exists());
    }

    #[test]
    fn test_validate_save_path_logical_escape_nonexistent_parent() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();

        let malicious = base
            .join("..")
            .join("outside")
            .join("nonexistent")
            .join("file.txt");
        let result = validate_save_path(&malicious, &base);
        assert!(result.is_err(), "父目录不在 base 内时应被逻辑逃逸检测阻止");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("逃逸检测") || err_msg.contains("不在预期目录"),
            "错误信息应说明逃逸检测: {err_msg}"
        );
    }

    #[test]
    fn test_validate_save_path_no_file_name() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();
        let subdir = base.join("subdir");
        std::fs::create_dir(&subdir).unwrap();

        // final_path 以 .. 结尾,没有有意义的 file_name
        let final_path = subdir.join("..");
        let result = validate_save_path(&final_path, &base);
        assert!(result.is_err(), "无 file_name 时应报错");
        assert!(
            result.unwrap_err().to_string().contains("无文件名"),
            "应提示无文件名"
        );
    }

    #[test]
    fn test_parse_content_disposition_non_utf8_charset() {
        // ISO-8859-1 编码的 é 字节 0xE9,不是合法 UTF-8
        assert_eq!(
            parse_content_disposition("attachment; filename*=ISO-8859-1''%E9"),
            None
        );
    }

    #[test]
    fn test_parse_content_disposition_empty_decoded() {
        assert_eq!(
            parse_content_disposition("attachment; filename*=UTF-8''"),
            None
        );
        assert_eq!(parse_content_disposition("attachment; filename=\"\""), None);
    }

    #[test]
    fn test_percent_decode_boundaries() {
        assert_eq!(percent_decode(""), Some(String::new()));
        assert_eq!(percent_decode("%41%42"), Some("AB".to_string()));
        assert_eq!(percent_decode("%61%62"), Some("ab".to_string()));
        // 不完整的 % 序列按字面保留
        assert_eq!(percent_decode("%"), Some("%".to_string()));
        assert_eq!(percent_decode("%2"), Some("%2".to_string()));
        // 非法十六进制字符按字面保留
        assert_eq!(percent_decode("%GG"), Some("%GG".to_string()));
        assert_eq!(percent_decode("%G1"), Some("%G1".to_string()));
        assert_eq!(percent_decode("%1G"), Some("%1G".to_string()));
    }

    // ===== validate_multi_save_paths:多文件 torrent 子目录结构保留 =====
    //
    // OPT-2:多文件 torrent 的 file_names 可能含子目录(如 "subdir/a.bin"),
    // validate_multi_save_paths 必须把子目录段当作目录保留,而非 sanitize 成空格。
    // 否则多文件 torrent 的嵌套目录结构在落盘时丢失。

    #[test]
    fn test_validate_multi_save_paths_preserves_subdir() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();

        let paths = validate_multi_save_paths(
            &base,
            "mytorrent",
            &["subdir/a.bin".to_string(), "subdir/b.bin".to_string()],
        )
        .expect("合法子目录路径应通过校验");

        // 两个文件应落到 mytorrent/subdir/ 下,而非被扁平化成 "subdir a.bin"
        // validate_save_path 返回 canonical 路径(Windows 上带 \\?\ 前缀),
        // 故比较 file_name 与 parent 末段而非整路径字符串。
        let p0 = &paths[0];
        assert_eq!(p0.file_name().unwrap(), "a.bin", "文件名应保留");
        assert_eq!(
            p0.parent().unwrap().file_name().unwrap(),
            "subdir",
            "子目录应被保留,而非扁平化成空格"
        );
        assert_eq!(paths[1].file_name().unwrap(), "b.bin", "第二个文件也应保留");
        assert_eq!(
            paths[1].parent().unwrap().file_name().unwrap(),
            "subdir",
            "第二个文件的子目录也应保留"
        );
    }

    #[test]
    fn test_validate_multi_save_paths_blocks_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();

        // 含 .. 的相对路径应被阻止(无法逃逸出 torrent 基目录)
        let result =
            validate_multi_save_paths(&base, "mytorrent", &["../../../etc/passwd".to_string()]);
        assert!(
            result.is_err(),
            "含 .. 的逃逸路径应被阻止,实际返回: {:?}",
            result.ok()
        );
    }

    #[test]
    fn test_validate_multi_save_paths_flat_file() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();

        // 无子目录的简单文件名应直接落到 torrent 基目录下
        let paths = validate_multi_save_paths(&base, "mytorrent", &["data.bin".to_string()])
            .expect("简单文件名应通过校验");
        assert_eq!(paths[0].file_name().unwrap(), "data.bin");
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    // sanitize_filename 对任意字符串不 panic,且结果非空
    proptest! {
        #[test]
        fn test_sanitize_filename_no_panic_and_nonempty(name in ".*") {
            let sanitized = sanitize_filename(&name);
            prop_assert!(!sanitized.is_empty(), "sanitize 结果不应为空");
            prop_assert!(!sanitized.contains('/'), "结果不应含路径分隔符 /");
            prop_assert!(!sanitized.contains('\\'), "结果不应含路径分隔符 \\");
        }

        // extract_filename_from_url 对任意字符串不 panic 且返回非空
        #[test]
        fn test_extract_filename_from_url_no_panic(url in ".*") {
            let name = extract_filename_from_url(&url);
            prop_assert!(!name.is_empty(), "提取的文件名不应为空");
        }

        // parse_content_disposition 对任意字符串不 panic
        #[test]
        fn test_parse_content_disposition_no_panic(value in ".*") {
            let _ = parse_content_disposition(&value);
        }

        // validate_save_path 在临时目录下对任意 sanitize 后的文件名不 panic
        #[test]
        fn test_validate_save_path_no_panic(name in "[a-zA-Z0-9_.\\-\\/\\\\]{0,50}") {
            let temp = tempfile::tempdir().unwrap();
            let base = temp.path();
            let file_name = sanitize_filename(&name);
            let final_path = base.join(&file_name);
            let _ = validate_save_path(&final_path, base);
        }
    }
}
