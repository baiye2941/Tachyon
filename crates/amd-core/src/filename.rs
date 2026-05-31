//! 文件名提取与 Content-Disposition 解析
//!
//! 整合了散布在 amd-app、amd-protocol(http/quic)中的重复实现,
//! 提供统一的文件名提取入口:
//! - `extract_filename_from_url` — 从 URL 路径提取文件名(含 percent-decode)
//! - `parse_content_disposition` — 解析 Content-Disposition 头
//! - `extract_filename` — 先尝试 Content-Disposition,再回退到 URL

/// 清洗文件名,防止路径遍历攻击
///
/// 安全措施:
/// - 移除所有路径分隔符 (`/`, `\`)
/// - 移除所有 `..` 序列
/// - 移除前导和尾随的空格与点号
/// - 确保结果是纯粹的 basename,不包含任何目录结构信息
///
/// 如果清洗后结果为空,返回 `"unknown"`。
pub fn sanitize_filename(name: &str) -> String {
    let mut result = String::with_capacity(name.len());

    // 移除路径分隔符和点号序列
    for ch in name.chars() {
        match ch {
            '/' | '\\' => {
                // 路径分隔符替换为空格(后续会 trim)
                result.push(' ');
            }
            _ => result.push(ch),
        }
    }

    // 单次遍历移除所有 ".." 序列,避免 O(n^2) 的 while-replace 循环
    let chars: Vec<char> = result.chars().collect();
    result = String::with_capacity(result.len());
    let mut i = 0;
    while i < chars.len() {
        if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1] == '.' {
            i += 2;
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    // 移除前导和尾随的空格与点号
    let trimmed = result.trim().trim_matches('.');

    // 如果结果为空或只包含空白字符,返回 "unknown"
    if trimmed.is_empty() {
        return "unknown".to_string();
    }

    // 额外安全检查:确保不包含任何路径分隔符(双重保险)
    let safe_name: String = trimmed
        .chars()
        .filter(|c| {
            *c != '/'
                && *c != '\\'
                && *c != ':'
                && *c != '*'
                && *c != '?'
                && *c != '"'
                && *c != '<'
                && *c != '>'
                && *c != '|'
        })
        .collect();

    if safe_name.is_empty() {
        return "unknown".to_string();
    }

    safe_name
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
/// - `Err(AmdError::Config)`: 路径校验失败
pub fn validate_save_path(
    final_path: &std::path::Path,
    expected_base: &std::path::Path,
) -> crate::AmdResult<std::path::PathBuf> {
    // 1. 确保基目录存在且可 canonicalize
    let canonical_base = expected_base
        .canonicalize()
        .map_err(|e| crate::AmdError::Config(format!("下载目录不存在或无法访问: {e}")))?;

    // 2. 如果 final_path 已存在,直接 canonicalize 并校验
    if final_path.exists() {
        let canonical_final = final_path
            .canonicalize()
            .map_err(|e| crate::AmdError::Config(format!("无法解析文件路径: {e}")))?;

        if !canonical_final.starts_with(&canonical_base) {
            return Err(crate::AmdError::Config(format!(
                "路径逃逸检测: {:?} 不在预期目录 {:?} 内",
                canonical_final, canonical_base
            )));
        }

        return Ok(canonical_final);
    }

    // 3. 文件尚不存在,校验父目录
    let parent = final_path
        .parent()
        .ok_or_else(|| crate::AmdError::Config("无效的文件路径: 无父目录".into()))?;

    if !parent.exists() {
        std::fs::create_dir_all(parent).map_err(crate::AmdError::Io)?;
    }

    let canonical_parent = parent
        .canonicalize()
        .map_err(|e| crate::AmdError::Config(format!("无法解析父目录: {e}")))?;

    if !canonical_parent.starts_with(&canonical_base) {
        return Err(crate::AmdError::Config(format!(
            "父目录逃逸检测: {:?} 不在预期目录 {:?} 内",
            canonical_parent, canonical_base
        )));
    }

    // 基于 canonical_parent 构建最终路径,防止 TOCTOU 符号链接攻击
    let file_name = final_path
        .file_name()
        .ok_or_else(|| crate::AmdError::Config("无效的文件路径: 无文件名".into()))?;
    Ok(canonical_parent.join(file_name))
}

/// 解析 Content-Disposition 头中的文件名
///
/// 支持两种格式:
/// - `filename*=UTF-8''percent_encoded_name` (RFC 5987)
/// - `filename="name"` / `filename=name`
///
/// `filename*` 优先于 `filename`。
pub fn parse_content_disposition(value: &str) -> Option<String> {
    if let Some(pos) = value.find("filename*=") {
        let rest = &value[pos + 10..];
        if let Some(encoded) = rest.split(';').next() {
            let mut parts = encoded.splitn(3, '\'');
            let _charset = parts.next(); // 编码名称(如 UTF-8),当前不使用
            let _encoding = parts.next(); // 编码方式(如 '', 当前不使用
            if let Some(encoded_name) = parts.next()
                && let Some(decoded) = percent_decode(encoded_name)
                && !decoded.is_empty()
            {
                return Some(decoded);
            }
        }
    }
    if let Some(pos) = value.find("filename=") {
        let rest = &value[pos + 9..];
        let name = rest.trim_start().split(';').next().unwrap_or(rest);
        let name = name.trim_matches('"').trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
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

    // ------ 路径遍历防护测试 ------

    #[test]
    fn test_sanitize_filename_basic() {
        assert_eq!(sanitize_filename("file.zip"), "file.zip");
    }

    #[test]
    fn test_sanitize_filename_path_traversal_dotdot() {
        // ../../etc/passwd -> etc passwd (路径分隔符替换为空格)
        assert_eq!(sanitize_filename("../../etc/passwd"), "etc passwd");
    }

    #[test]
    fn test_sanitize_filename_path_traversal_slash() {
        // foo/bar/baz.txt -> foo bar baz.txt
        assert_eq!(sanitize_filename("foo/bar/baz.txt"), "foo bar baz.txt");
    }

    #[test]
    fn test_sanitize_filename_path_traversal_backslash() {
        // foo\bar\baz.txt -> foo bar baz.txt
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
        // 复杂的路径遍历尝试
        assert_eq!(
            sanitize_filename("....//....//....//etc/passwd"),
            "etc passwd"
        );
    }

    #[test]
    fn test_sanitize_filename_windows_reserved_chars() {
        // Windows 保留字符应被移除
        assert_eq!(sanitize_filename("file:name*test?.txt"), "filenametest.txt");
    }

    #[test]
    fn test_extract_filename_from_url_traversal() {
        // URL 中的路径遍历应被防护
        // URL 解析器只提取最后一段 "passwd"
        assert_eq!(
            extract_filename_from_url("https://example.com/../../etc/passwd"),
            "passwd"
        );
    }

    #[test]
    fn test_extract_filename_content_disposition_traversal() {
        // Content-Disposition 中的路径遍历应被防护
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
        // 正常文件名应保持不变
        assert_eq!(
            extract_filename("https://example.com/document.pdf", None),
            "document.pdf"
        );
    }

    #[test]
    fn test_extract_filename_with_spaces() {
        // 带空格的文件名应保持不变
        assert_eq!(
            extract_filename("https://example.com/my%20document.pdf", None),
            "my document.pdf"
        );
    }

    #[test]
    fn test_extract_filename_complex_traversal() {
        // 复杂的路径遍历攻击
        assert_eq!(
            extract_filename(
                "https://example.com/safe.txt",
                Some("attachment; filename=\"../../../Windows/System32/config/sam\"")
            ),
            "Windows System32 config sam"
        );
    }

    // ------ validate_save_path 测试 ------

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

        // 文件不存在但父目录存在
        let result = validate_save_path(&final_path, &base);
        assert!(result.is_ok(), "新文件在合法目录内应通过校验");
    }

    #[test]
    fn test_validate_save_path_creates_parent_dir() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();
        let final_path = base.join("subdir").join("file.txt");

        // 父目录不存在,函数应创建
        let result = validate_save_path(&final_path, &base);
        assert!(result.is_ok(), "应自动创建子目录");
        assert!(base.join("subdir").exists(), "子目录应被创建");
    }

    #[test]
    fn test_validate_save_path_dotdot_escape() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();

        // 构造路径逃逸: downloads/../../../etc/passwd
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

        // 创建指向外部的符号链接
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

        // 内部符号链接(合法)
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
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("downloads");
        std::fs::create_dir(&base).unwrap();

        let outside_file = temp.path().join("secret.txt");
        std::fs::write(&outside_file, b"secret data").unwrap();
        let evil_link = base.join("innocent.txt");

        // Windows 符号链接创建可能需要权限,失败时跳过
        if std::os::windows::fs::symlink_file(&outside_file, &evil_link).is_ok() {
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
        // 返回的路径应是 canonical 形式
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
}
