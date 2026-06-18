//! 配置解析 fuzz target
//!
//! 不变量:
//! - 任意 JSON 字节输入反序列化为 `DownloadConfig` / `AppConfig` 后不 panic。
//! - 反序列化成功后调用 `validate()` 应拒绝非法边界值,且 `validate()` 本身不 panic。

#![no_main]

use libfuzzer_sys::fuzz_target;
use tachyon_core::config::{AppConfig, DownloadConfig};

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);

    // DownloadConfig: 反序列化 + validate 均不能 panic
    if let Ok(cfg) = serde_json::from_str::<DownloadConfig>(&input) {
        let _ = cfg.validate();
    }

    // AppConfig: 反序列化 + validate 均不能 panic
    if let Ok(cfg) = serde_json::from_str::<AppConfig>(&input) {
        let _ = cfg.validate();
    }
});
