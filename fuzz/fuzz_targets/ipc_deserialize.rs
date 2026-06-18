//! IPC 输入序列化 fuzz target
//!
//! 不变量:
//! - 任意字节输入作为 JSON 反序列化为事件/类型时,不 panic、不 OOM。
//! - 反序列化失败是正常路径,允许返回错误。

#![no_main]

use libfuzzer_sys::fuzz_target;
use tachyon_core::event::{DownloadEvent, FragmentEvent, PeerEvent};
use tachyon_core::types::{
    DownloadState, DownloadStateChange, FileMetadata, FragmentInfo, TaskCommand, TaskProgress,
};

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);

    // 事件类型反序列化
    let _: Result<DownloadEvent, _> = serde_json::from_str(&input);
    let _: Result<FragmentEvent, _> = serde_json::from_str(&input);
    let _: Result<PeerEvent, _> = serde_json::from_str(&input);

    // 核心类型反序列化
    let _: Result<FileMetadata, _> = serde_json::from_str(&input);
    let _: Result<FragmentInfo, _> = serde_json::from_str(&input);
    let _: Result<DownloadStateChange, _> = serde_json::from_str(&input);

    let _: Result<TaskProgress, _> = serde_json::from_str(&input);
    let _: Result<DownloadState, _> = serde_json::from_str(&input);
    let _: Result<TaskCommand, _> = serde_json::from_str(&input);
});
