//! 磁力链接 URI 解析 fuzz target
//!
//! 不变量:
//! - 任意字节输入经 UTF-8 解析后,`validate_magnet_uri` 不 panic、不进入死循环、不 OOM,
//!   且始终返回 Ok 或 Err(非法输入应被安全拒绝)。
//! - 仅在 `validate_magnet_uri` 通过时调用 `parse_pe_from_magnet`,模拟生产调用序列
//!   (probe / download_range_stream 均先校验后解析);后者内部对 `uri[8..]` 切片,
//!   短于 8 字节的输入会越界,故必须先经校验保证长度足够再进入 pe 解析。
//!
//! magnet URI 接受任意来源(用户粘贴、剪贴板、网页传入),解析健壮性是安全核心面:
//! 解析器对畸形输入只能返回 Err,绝不能 panic 导致进程崩溃。

#![no_main]

use libfuzzer_sys::fuzz_target;
use tachyon_protocol::magnet::{parse_pe_from_magnet, validate_magnet_uri};

fuzz_target!(|data: &[u8]| {
    // 1. 任意字节流转 String(可能含非法 UTF-8,用 lossy 转换,绝不 panic)
    let input = String::from_utf8_lossy(data);

    // 2. 校验磁力链接格式:确认对任意输入不 panic,始终返回 Ok 或 Err
    //    非法输入(无 magnet:? 前缀、缺 xt=urn:btih:、空 hash 等)应被安全拒绝
    if validate_magnet_uri(&input).is_ok() {
        // 3. 仅在校验通过时解析 &pe= 参数(模拟生产调用序列,保证 uri 长度足够)
        //    合法 magnet 的 &pe= 变异地址应被解析或跳过,不 panic
        let _addrs = parse_pe_from_magnet(&input);
    }
});
