//! KV 存储 key 序列化/反序列化 fuzz target
//!
//! 不变量:
//! - 任意字节输入经 UTF-8 解析后,`FileStore::safe_key` / `FileStore::unsafe_key`
//!   对原始字符串可逆,不 panic、不进入死循环。
//! - `TaskSnapshot` / `TaskRecord` 的 JSON 反序列化不 panic,不 OOM。
//! - 反序列化失败属于正常路径,允许返回错误。

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);

    // 1. safe_key / unsafe_key roundtrip
    let safe = tachyon_store::FileStore::safe_key(&input);
    let decoded = tachyon_store::FileStore::unsafe_key(&safe);
    assert_eq!(decoded, input, "safe_key -> unsafe_key roundtrip 失败");

    // 2. 任意 JSON 反序列化为 TaskSnapshot
    let _: Result<tachyon_store::TaskSnapshot, _> = serde_json::from_str(&input);

    // 3. 任意 JSON 反序列化为 TaskRecord(旧接口兼容)
    let _: Result<tachyon_store::TaskRecord, _> = serde_json::from_str(&input);
});
