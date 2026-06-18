//! HTTP 头处理 fuzz target
//!
//! 不变量:
//! - 任意字节输入构造的 HTTP 头值,`parse_retry_after` / `parse_content_range_total`
//!   不 panic。
//! - `classify_http_error` 对任意状态码 + 任意头映射返回确定错误变体,不 panic。

#![no_main]

use libfuzzer_sys::fuzz_target;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use tachyon_protocol::http;

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);

    // 1. Retry-After 解析:任意字符串应安全返回 Some/None,不 panic
    let _ = http::parse_retry_after(&input);

    // 2. Content-Range 解析:任意字符串应安全返回 Some/None,不 panic
    let _ = http::parse_content_range_total(&input);

    // 3. 构造任意 StatusCode 与 HeaderMap,调用 classify_http_error 不应 panic
    //    状态码取输入前两个字节的 u16,Header 值直接复用输入字符串
    let code = if data.len() >= 2 {
        u16::from_le_bytes([data[0], data[1]])
    } else {
        200
    };
    if let Ok(status) = StatusCode::from_u16(code) {
        let mut headers = HeaderMap::new();
        if let Ok(value) = input.parse() {
            headers.insert("retry-after", value);
        }
        let _ = http::classify_http_error(status, &headers);
    }
});
