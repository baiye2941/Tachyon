//! URL 安全校验 fuzz target
//!
//! 不变量:
//! - 任意字节输入经 UTF-8 解析后,`validate_public_http_url` / `redact_url_for_log`
//!   不 panic、不进入死循环、不触发 OOM。
//! - 非法 URL 字符串应被安全拒绝或返回占位符。

#![no_main]

use libfuzzer_sys::fuzz_target;
use tachyon_core::{redact_url_for_log, validate_public_http_url};

fuzz_target!(|data: &[u8]| {
    // 1. 将任意字节作为 UTF-8 字符串(替换无效字符)处理
    let input = String::from_utf8_lossy(data);

    // 2. 尝试解析为 URL;解析失败属于正常路径,不应 panic
    if let Ok(url) = url::Url::parse(&input) {
        // validate_public_http_url 仅做本地字符串/IP 检查,不触发网络
        let _ = validate_public_http_url(&url);
    }

    // 3. 日志脱敏函数对任意字符串都应安全返回
    let _redacted = redact_url_for_log(&input);
});
