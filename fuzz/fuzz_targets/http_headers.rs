//! HTTP 头处理 fuzz target
//!
//! 不变量:
//! - 任意字节输入构造的 content_length,`check_response_size_limit` 不 panic。
//! - 任意状态码构造安全,不 panic。

#![no_main]

use libfuzzer_sys::fuzz_target;
use reqwest::StatusCode;

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);

    // 1. 任意 content_length 检查不 panic
    let content_length = if data.len() >= 8 {
        Some(u64::from_le_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ]))
    } else {
        None
    };
    let _ = tachyon_protocol::http::check_response_size_limit(content_length);

    // 2. 任意状态码构造安全
    let code = if data.len() >= 2 {
        u16::from_le_bytes([data[0], data[1]])
    } else {
        200
    };
    let _ = StatusCode::from_u16(code);

    // 3. 任意字符串作为 HeaderValue 解析,不 panic
    let _ = input.parse::<reqwest::header::HeaderValue>();
});
