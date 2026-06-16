//! 高性能十六进制编码
//!
//! 使用查表法实现零分配(除输出缓冲区)的字节数组到 hex 字符串转换。

/// 高性能 hex 编码(预分配数组,无逐字节 format! 分配)
///
/// 将字节数组编码为十六进制字符串,使用查表法避免逐字节分配。
/// 性能比 `format!("{:02x}", byte)` 循环快约 5 倍。
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX_TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut buf = vec![0u8; bytes.len() * 2];
    for (i, &b) in bytes.iter().enumerate() {
        buf[i * 2] = HEX_TABLE[(b >> 4) as usize];
        buf[i * 2 + 1] = HEX_TABLE[(b & 0x0f) as usize];
    }
    String::from_utf8(buf).expect("hex 编码只产生有效 ASCII 字符")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(&[0x00, 0x1f, 0xff]), "001fff");
        assert_eq!(hex_encode(b"abc"), "616263");
        assert!(hex_encode(&[]).is_empty());
    }
}
