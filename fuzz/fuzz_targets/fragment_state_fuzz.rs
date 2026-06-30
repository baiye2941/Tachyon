//! 分片状态与分片规划 fuzz target
//!
//! 不变量:
//! - 任意输入构造 `FragmentInfo::new` 不 panic,返回 `Result`。
//! - 任意输入传入 `plan_fragments` 不 panic,返回 `Result`。
//! - 溢出、越界等异常条件返回 `Err` 而非 panic。

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() >= 28 {
        // 1. FragmentInfo::new 溢出/边界检测
        let index = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let start = u64::from_le_bytes(data[4..12].try_into().unwrap());
        let end = u64::from_le_bytes(data[12..20].try_into().unwrap());
        let size = u64::from_le_bytes(data[20..28].try_into().unwrap());

        // FragmentInfo::new 返回 Result,不应 panic
        let _ = tachyon_core::types::FragmentInfo::new(index, start, end, size);
    }

    // 2. plan_fragments 输入 fuzz
    if data.len() >= 32 {
        let file_size = u64::from_le_bytes(data[0..8].try_into().unwrap());
        let suggested_frag_size = if data[8] == 0 {
            None
        } else {
            Some(u64::from_le_bytes(data[9..17].try_into().unwrap()))
        };
        let supports_range = data[31] & 1 == 1;

        let config = tachyon_core::config::SchedulerConfig::default();
        let _ = tachyon_engine::fragment::plan_fragments(
            file_size,
            supports_range,
            suggested_frag_size,
            &config,
        );
    }
});
