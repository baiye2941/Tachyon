//! CPU 哈希校验实现
//!
//! 基于 blake3 和 sha2 的哈希计算与校验。

use tachyon_core::error::DownloadResult;
use tachyon_core::traits::Verifier;

/// 哈希算法类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum HashAlgorithm {
    #[default]
    Blake3,
    /// SHA-256 哈希
    Sha256,
}

/// CPU 校验器,支持 blake3 和 sha256
#[derive(Clone)]
pub struct CpuVerifier {
    algorithm: HashAlgorithm,
}

impl CpuVerifier {
    /// 创建 Blake3 校验器
    pub fn blake3() -> Self {
        Self {
            algorithm: HashAlgorithm::Blake3,
        }
    }

    /// 创建 SHA-256 校验器
    pub fn sha256() -> Self {
        Self {
            algorithm: HashAlgorithm::Sha256,
        }
    }

    /// 获取当前使用的哈希算法
    pub fn algorithm(&self) -> HashAlgorithm {
        self.algorithm
    }
}

impl Default for CpuVerifier {
    fn default() -> Self {
        Self::blake3()
    }
}

impl Verifier for CpuVerifier {
    fn compute_hash(&self, data: &[u8]) -> DownloadResult<String> {
        match self.algorithm {
            HashAlgorithm::Blake3 => {
                let hash = blake3::hash(data);
                Ok(hash.to_hex().to_string())
            }
            HashAlgorithm::Sha256 => {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(data);
                let result = hasher.finalize();
                Ok(hex_encode(&result))
            }
        }
    }
}

/// 十六进制编码
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// 根据数据大小自动选择最优校验算法
///
/// Blake3 在所有大小下都优于 SHA-256,因此默认使用 Blake3。
pub fn auto_select_verifier(_data_size: u64) -> CpuVerifier {
    CpuVerifier::blake3()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blake3_hash() {
        let verifier = CpuVerifier::blake3();
        let hash = verifier.compute_hash(b"hello").unwrap();
        // blake3("hello") 的已知值
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // 256 bits = 64 hex chars
    }

    #[test]
    fn test_sha256_hash() {
        let verifier = CpuVerifier::sha256();
        let hash = verifier.compute_hash(b"hello").unwrap();
        // sha256("hello") = 2cf24dba...
        assert_eq!(hash.len(), 64);
        assert!(hash.starts_with("2cf24dba"));
    }

    #[test]
    fn test_blake3_deterministic() {
        let verifier = CpuVerifier::blake3();
        let hash1 = verifier.compute_hash(b"test data").unwrap();
        let hash2 = verifier.compute_hash(b"test data").unwrap();
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_sha256_deterministic() {
        let verifier = CpuVerifier::sha256();
        let hash1 = verifier.compute_hash(b"test data").unwrap();
        let hash2 = verifier.compute_hash(b"test data").unwrap();
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_different_data_different_hash() {
        let verifier = CpuVerifier::blake3();
        let hash1 = verifier.compute_hash(b"data1").unwrap();
        let hash2 = verifier.compute_hash(b"data2").unwrap();
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_empty_data() {
        let verifier = CpuVerifier::blake3();
        let hash = verifier.compute_hash(b"").unwrap();
        assert!(!hash.is_empty());
    }

    #[test]
    fn test_verify_match() {
        let verifier = CpuVerifier::blake3();
        let data = b"verify me";
        let hash = verifier.compute_hash(data).unwrap();
        verifier.verify(data, &hash).unwrap();
    }

    #[test]
    fn test_verify_mismatch() {
        let verifier = CpuVerifier::blake3();
        let hash = verifier.compute_hash(b"original").unwrap();
        let result = verifier.verify(b"tampered", &hash);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            tachyon_core::DownloadError::ChecksumMismatch { .. }
        ));
    }

    #[test]
    fn test_algorithm_type() {
        let blake = CpuVerifier::blake3();
        assert_eq!(blake.algorithm(), HashAlgorithm::Blake3);

        let sha = CpuVerifier::sha256();
        assert_eq!(sha.algorithm(), HashAlgorithm::Sha256);
    }

    #[test]
    fn test_default_is_blake3() {
        let verifier = CpuVerifier::default();
        assert_eq!(verifier.algorithm(), HashAlgorithm::Blake3);
    }

    #[test]
    fn test_auto_select_small() {
        let verifier = auto_select_verifier(1024);
        assert_eq!(verifier.algorithm(), HashAlgorithm::Blake3);
    }

    #[test]
    fn test_auto_select_large() {
        let verifier = auto_select_verifier(128 * 1024 * 1024);
        assert_eq!(verifier.algorithm(), HashAlgorithm::Blake3);
    }

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(&[0x0a, 0xff, 0x00]), "0aff00");
    }
}
