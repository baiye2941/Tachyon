//! 文件元数据处理 fuzz target
//!
//! 不变量:
//! - 任意字节输入作为 Content-Disposition / URL / 文件名,`parse_content_disposition` /
//!   `sanitize_filename` / `extract_filename` / `extract_filename_from_url` 不 panic。
//! - `validate_save_path` 在临时目录场景下不 panic(允许返回 Err)。

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;
use tachyon_core::safety::{
    extract_filename, extract_filename_from_url, parse_content_disposition, sanitize_filename,
    validate_save_path,
};

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);

    // 1. Content-Disposition 解析
    let _ = parse_content_disposition(&input);

    // 2. 文件名清洗
    let sanitized = sanitize_filename(&input);
    // 清洗后结果非空(或为 "unknown")
    assert!(!sanitized.is_empty());

    // 3. 从 URL 提取文件名(输入本身作为 URL)
    let extracted = extract_filename_from_url(&input);
    assert!(!extracted.is_empty());

    // 4. 综合提取:Content-Disposition + URL
    let combined = extract_filename(&input, Some(&input));
    assert!(!combined.is_empty());

    // 5. validate_save_path:使用临时目录作为 base,输入作为文件名,
    //    允许返回 Err,但绝不能 panic
    let temp_base = std::env::temp_dir();
    let file_name = sanitize_filename(&input);
    let final_path = temp_base.join(&file_name);
    let _ = validate_save_path(&final_path, &temp_base);

    // 6. 同时测试 PathBuf 直接构造(可能含空组件)
    let raw_path = PathBuf::from(&input);
    let _ = validate_save_path(&raw_path, &temp_base);
});
