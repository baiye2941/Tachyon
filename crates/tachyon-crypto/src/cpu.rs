//! CPU 哈希校验实现
//!
//! 基于 blake3 和 sha2 的哈希计算与校验。

use std::path::Path;

use tachyon_core::error::DownloadResult;
use tachyon_core::hex_encode;
use tachyon_core::traits::{StreamingHasher, Verifier};
use tokio::io::{AsyncRead, AsyncReadExt};

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

impl CpuVerifier {
    /// 流式计算哈希值
    ///
    /// 从异步读取器中逐块读取数据并增量更新哈希器,
    /// 避免将整个文件加载到内存中。
    ///
    /// # 参数
    /// - `reader`: 实现 `AsyncRead` 的异步读取器
    /// - `chunk_size`: 每次读取的字节数,建议 64KB ~ 1MB
    ///
    /// # 示例
    /// ```rust,ignore
    /// let verifier = CpuVerifier::blake3();
    /// let file = tokio::fs::File::open("model.bin").await.unwrap();
    /// let hash = verifier.compute_hash_streaming(&mut file, 65536).await.unwrap();
    /// ```
    pub async fn compute_hash_streaming<R: AsyncRead + Unpin>(
        &self,
        reader: &mut R,
        chunk_size: usize,
    ) -> DownloadResult<String> {
        match self.algorithm {
            HashAlgorithm::Blake3 => {
                let mut hasher = blake3::Hasher::new();
                let mut buf = vec![0u8; chunk_size];
                loop {
                    let n = reader
                        .read(&mut buf)
                        .await
                        .map_err(tachyon_core::error::DownloadError::Io)?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                }
                Ok(hasher.finalize().to_hex().to_string())
            }
            HashAlgorithm::Sha256 => {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                let mut buf = vec![0u8; chunk_size];
                loop {
                    let n = reader
                        .read(&mut buf)
                        .await
                        .map_err(tachyon_core::error::DownloadError::Io)?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                }
                let result = hasher.finalize();
                Ok(hex_encode(&result))
            }
        }
    }

    /// 从文件路径流式计算哈希值
    ///
    /// 打开文件并使用 `compute_hash_streaming` 逐块计算哈希,
    /// 适用于大文件(如 50GB 模型文件)的校验场景。
    ///
    /// # 参数
    /// - `path`: 文件路径
    /// - `chunk_size`: 每次读取的字节数
    pub async fn compute_hash_from_path(
        &self,
        path: &Path,
        chunk_size: usize,
    ) -> DownloadResult<String> {
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(tachyon_core::error::DownloadError::Io)?;
        let hash = self.compute_hash_streaming(&mut file, chunk_size).await?;
        Ok(hash)
    }
}

/// CPU 流式哈希句柄,包装 blake3::Hasher 或 sha2::Sha256
///
/// 由 `CpuVerifier::new_hasher()` 创建,供下载管线"边下边 update、写完再 finalize"。
/// 避免一次性 `compute_hash(&[u8])` 将整个分片加载进内存。
pub struct CpuStreamingHasher {
    algorithm: HashAlgorithm,
    blake3: Option<blake3::Hasher>,
    sha256: Option<sha2::Sha256>,
}

impl CpuStreamingHasher {
    pub fn new(algorithm: HashAlgorithm) -> Self {
        use sha2::Digest;
        match algorithm {
            HashAlgorithm::Blake3 => Self {
                algorithm,
                blake3: Some(blake3::Hasher::new()),
                sha256: None,
            },
            HashAlgorithm::Sha256 => Self {
                algorithm,
                blake3: None,
                sha256: Some(sha2::Sha256::new()),
            },
        }
    }
}

impl StreamingHasher for CpuStreamingHasher {
    fn update(&mut self, data: &[u8]) {
        match self.algorithm {
            HashAlgorithm::Blake3 => {
                self.blake3.as_mut().unwrap().update(data);
            }
            HashAlgorithm::Sha256 => {
                use sha2::Digest;
                self.sha256.as_mut().unwrap().update(data);
            }
        }
    }

    fn finalize(mut self: Box<Self>) -> String {
        match self.algorithm {
            HashAlgorithm::Blake3 => self.blake3.take().unwrap().finalize().to_hex().to_string(),
            HashAlgorithm::Sha256 => {
                use sha2::Digest;
                hex_encode(&self.sha256.take().unwrap().finalize())
            }
        }
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

    fn new_hasher(&self) -> Box<dyn StreamingHasher> {
        Box::new(CpuStreamingHasher::new(self.algorithm))
    }
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
    fn test_hex_encode() {
        assert_eq!(hex_encode(&[0x0a, 0xff, 0x00]), "0aff00");
    }

    // Streaming API 测试 --------------------------------------------------

    #[tokio::test]
    async fn test_blake3_streaming_matches_compute_hash() {
        let data = b"streaming hash test data";
        let verifier = CpuVerifier::blake3();

        let expected = verifier.compute_hash(data).unwrap();
        let mut cursor = std::io::Cursor::new(data.as_slice());
        let actual = verifier
            .compute_hash_streaming(&mut cursor, 8)
            .await
            .unwrap();

        assert_eq!(expected, actual, "流式计算结果必须与全量计算一致");
    }

    #[tokio::test]
    async fn test_sha256_streaming_matches_compute_hash() {
        let data = b"streaming sha256 test data";
        let verifier = CpuVerifier::sha256();

        let expected = verifier.compute_hash(data).unwrap();
        let mut cursor = std::io::Cursor::new(data.as_slice());
        let actual = verifier
            .compute_hash_streaming(&mut cursor, 10)
            .await
            .unwrap();

        assert_eq!(expected, actual, "流式计算结果必须与全量计算一致");
    }

    #[tokio::test]
    async fn test_blake3_streaming_large_chunk() {
        let data = vec![0xABu8; 1024];
        let verifier = CpuVerifier::blake3();

        let expected = verifier.compute_hash(&data).unwrap();
        let mut cursor = std::io::Cursor::new(data.as_slice());
        // chunk_size 大于数据长度,应一次读完
        let actual = verifier
            .compute_hash_streaming(&mut cursor, 4096)
            .await
            .unwrap();

        assert_eq!(expected, actual);
    }

    #[tokio::test]
    async fn test_blake3_streaming_small_chunks() {
        let data = vec![0xCDu8; 256];
        let verifier = CpuVerifier::blake3();

        let expected = verifier.compute_hash(&data).unwrap();
        let mut cursor = std::io::Cursor::new(data.as_slice());
        // 每次只读 7 字节,测试多次循环路径
        let actual = verifier
            .compute_hash_streaming(&mut cursor, 7)
            .await
            .unwrap();

        assert_eq!(expected, actual);
    }

    #[tokio::test]
    async fn test_blake3_streaming_empty_data() {
        let verifier = CpuVerifier::blake3();
        let mut cursor = std::io::Cursor::new(&[] as &[u8]);
        let hash = verifier
            .compute_hash_streaming(&mut cursor, 64)
            .await
            .unwrap();

        let expected = verifier.compute_hash(b"").unwrap();
        assert_eq!(hash, expected);
        assert_eq!(hash.len(), 64);
    }

    #[tokio::test]
    async fn test_compute_hash_from_path_blake3() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_model.bin");
        let data = b"file path hash test payload";
        tokio::fs::write(&path, data).await.unwrap();

        let verifier = CpuVerifier::blake3();
        let hash = verifier.compute_hash_from_path(&path, 64).await.unwrap();
        let expected = verifier.compute_hash(data).unwrap();

        assert_eq!(hash, expected);
    }

    #[tokio::test]
    async fn test_compute_hash_from_path_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_model_sha256.bin");
        let data = b"sha256 file path test";
        tokio::fs::write(&path, data).await.unwrap();

        let verifier = CpuVerifier::sha256();
        let hash = verifier.compute_hash_from_path(&path, 64).await.unwrap();
        let expected = verifier.compute_hash(data).unwrap();

        assert_eq!(hash, expected);
    }

    #[tokio::test]
    async fn test_compute_hash_from_path_not_found() {
        let verifier = CpuVerifier::blake3();
        let path = Path::new("/nonexistent/path/file.bin");
        let result = verifier.compute_hash_from_path(path, 64).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_streaming_different_data_different_hash() {
        let verifier = CpuVerifier::blake3();
        let mut cursor1 = std::io::Cursor::new(b"data1");
        let mut cursor2 = std::io::Cursor::new(b"data2");

        let hash1 = verifier
            .compute_hash_streaming(&mut cursor1, 4)
            .await
            .unwrap();
        let hash2 = verifier
            .compute_hash_streaming(&mut cursor2, 4)
            .await
            .unwrap();

        assert_ne!(hash1, hash2);
    }

    // ── StreamingHasher 正确性测试 ──────────────────────────────────

    #[test]
    fn test_streaming_hasher_blake3_matches_oneshot() {
        use tachyon_core::traits::StreamingHasher;
        let data = b"streaming hasher blake3 consistency test payload";
        let verifier = CpuVerifier::blake3();

        // 一次性计算
        let oneshot = verifier.compute_hash(data).unwrap();

        // 流式分块计算(模拟下载管线:多段 update + finalize)
        let mut hasher = verifier.new_hasher();
        hasher.update(&data[..10]);
        hasher.update(&data[10..25]);
        hasher.update(&data[25..]);
        let streaming = hasher.finalize();

        assert_eq!(oneshot, streaming, "流式哈希应与一次性哈希一致");
    }

    #[test]
    fn test_streaming_hasher_sha256_matches_oneshot() {
        use tachyon_core::traits::StreamingHasher;
        let data = b"streaming hasher sha256 consistency test payload";
        let verifier = CpuVerifier::sha256();

        let oneshot = verifier.compute_hash(data).unwrap();

        let mut hasher = verifier.new_hasher();
        hasher.update(&data[..8]);
        hasher.update(&data[8..20]);
        hasher.update(&data[20..]);
        let streaming = hasher.finalize();

        assert_eq!(oneshot, streaming, "sha256 流式哈希应与一次性一致");
    }

    #[test]
    fn test_streaming_hasher_empty_input() {
        use tachyon_core::traits::StreamingHasher;
        let verifier = CpuVerifier::blake3();
        let oneshot = verifier.compute_hash(b"").unwrap();
        let streaming = verifier.new_hasher().finalize();
        assert_eq!(oneshot, streaming, "空输入流式哈希应与一次性一致");
    }

    #[test]
    fn test_streaming_hasher_single_chunk() {
        use tachyon_core::traits::StreamingHasher;
        let data = b"single chunk no split";
        let verifier = CpuVerifier::blake3();
        let oneshot = verifier.compute_hash(data).unwrap();
        let mut hasher = verifier.new_hasher();
        hasher.update(data);
        let streaming = hasher.finalize();
        assert_eq!(oneshot, streaming);
    }
}
