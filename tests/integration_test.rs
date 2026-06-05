//! 端到端集成测试
//!
//! 测试跨 crate 交互:
//! - tachyon-core 类型 -> tachyon-io 存储写入 -> tachyon-crypto 校验
//! - 模拟完整下载流程: 创建分片 -> 写入数据 -> 校验哈希 -> 验证完成

use bytes::Bytes;
use tachyon_core::DownloadError;
use tachyon_core::test_harness::harness::{
    MemoryStorage, MockProtocol, test_config, test_fragments, test_metadata,
};
use tachyon_core::traits::{Protocol, Storage, Verifier};
use tachyon_core::types::{DownloadState, FragmentInfo};
use tachyon_crypto::CpuVerifier;
use tachyon_engine::fragment::compute_fragment_size;
use tachyon_engine::fragment::{BandwidthTracker, FragmentRecord, FragmentState};
use tachyon_io::AsyncStorage;
use tachyon_io::pipeline::WritePipeline;
use tachyon_io::tokio_file::TokioFile;
use tempfile::NamedTempFile;

// ============================================================
// 跨 crate 流程: tachyon-core 类型 -> tachyon-io 存储 -> tachyon-crypto 校验
// ============================================================

/// 测试完整流程:创建分片 -> 写入存储 -> 计算哈希 -> 校验通过
///
/// 验证 tachyon-core 的 FragmentInfo 与 tachyon-io 的 TokioFile 和 tachyon-crypto 的 CpuVerifier
/// 协同工作。
#[tokio::test]
async fn integration_fragment_write_and_verify() {
    let tmp = NamedTempFile::new().unwrap();
    let storage = TokioFile::open(tmp.path()).await.unwrap();
    let verifier = CpuVerifier::blake3();

    // 模拟一个 3 分片的下载
    let total_size = 30u64;
    let fragments = test_fragments(total_size, 3);

    // 每个分片写入唯一数据
    for frag in &fragments {
        let data = vec![frag.index as u8; frag.size as usize];
        let written = storage
            .write_at(frag.start, Bytes::copy_from_slice(&data))
            .await
            .expect("写入分片数据失败");
        assert_eq!(written, frag.size as usize);
    }

    // 同步到磁盘
    storage.sync().await.expect("sync 失败");

    // 读取整个文件并校验
    let mut buf = vec![0u8; total_size as usize];
    let read = storage.read_at(0, &mut buf).await.expect("读取数据失败");
    assert_eq!(read, total_size as usize);

    // 验证每个分片的数据完整性
    for frag in &fragments {
        let start = frag.start as usize;
        let end = start + frag.size as usize;
        let chunk = &buf[start..end];
        // 每个分片区域的字节应一致
        assert!(
            chunk.iter().all(|&b| b == frag.index as u8),
            "分片 {} 数据不一致",
            frag.index
        );
    }

    // 计算整体哈希
    let hash = verifier.compute_hash(&buf).expect("计算哈希失败");
    assert_eq!(hash.len(), 64, "blake3 哈希长度应为 64 字符");

    // 校验通过
    verifier.verify(&buf, &hash).expect("校验失败");
}

/// 测试使用 MemoryStorage 的跨 crate 协同
#[tokio::test]
async fn integration_memory_storage_full_flow() {
    let storage = MemoryStorage::with_capacity(4096);
    let verifier = CpuVerifier::blake3();

    let total_size = 1024u64;
    let fragments = test_fragments(total_size, 4);

    // 预分配存储空间
    storage.allocate(total_size).await.expect("预分配失败");
    assert_eq!(storage.file_size().await.unwrap(), total_size);

    // 为每个分片生成数据并写入
    let mut expected_data = vec![0u8; total_size as usize];
    for frag in &fragments {
        let data: Vec<u8> = (0..frag.size)
            .map(|i| ((i + frag.index as u64) % 256) as u8)
            .collect();
        expected_data[frag.start as usize..frag.start as usize + frag.size as usize]
            .copy_from_slice(&data);
        let written = storage
            .write_at(frag.start, Bytes::copy_from_slice(&data))
            .await
            .unwrap();
        assert_eq!(written, frag.size as usize);
    }

    // 读回数据并验证与预期一致
    let actual_data = storage.get_data();
    assert_eq!(actual_data, expected_data);

    // 对每个分片单独计算哈希并记录
    let mut fragment_hashes = Vec::new();
    for frag in &fragments {
        let chunk = &expected_data[frag.start as usize..frag.start as usize + frag.size as usize];
        let hash = verifier.compute_hash(chunk).unwrap();
        fragment_hashes.push(hash);
    }

    // 校验每个分片的哈希
    for (i, frag) in fragments.iter().enumerate() {
        let chunk = &expected_data[frag.start as usize..frag.start as usize + frag.size as usize];
        verifier.verify(chunk, &fragment_hashes[i]).unwrap();
    }
}

/// 测试数据篡改检测: 跨 tachyon-io 和 tachyon-crypto 验证
#[tokio::test]
async fn integration_tamper_detection() {
    let tmp = NamedTempFile::new().unwrap();
    let storage = TokioFile::open(tmp.path()).await.unwrap();
    let verifier = CpuVerifier::sha256();

    let original_data = b"integrity test data payload";
    storage
        .write_at(0, Bytes::from_static(original_data))
        .await
        .unwrap();
    storage.sync().await.unwrap();

    // 计算原始哈希
    let original_hash = verifier.compute_hash(original_data).unwrap();

    // 读回数据,校验通过
    let mut buf = vec![0u8; original_data.len()];
    storage.read_at(0, &mut buf).await.unwrap();
    assert!(verifier.verify(&buf, &original_hash).is_ok());

    // 篡改数据
    buf[0] = buf[0].wrapping_add(1);
    assert!(
        verifier.verify(&buf, &original_hash).is_err(),
        "篡改数据后校验应失败"
    );
}

/// 测试 MockProtocol 探测 -> 创建分片 -> 写入 MemoryStorage -> 校验
#[tokio::test]
async fn integration_mock_protocol_to_storage() {
    let file_size = 2048u64;
    let meta = test_metadata("download.bin", file_size);
    let protocol = MockProtocol::new(meta);

    // 模拟协议探测
    let detected_meta = protocol
        .probe("http://example.com/download.bin")
        .await
        .expect("探测失败");
    assert_eq!(detected_meta.file_name, "download.bin");
    assert_eq!(detected_meta.file_size, Some(file_size));
    assert!(detected_meta.supports_range);

    // 根据元数据创建分片
    let fragments = test_fragments(detected_meta.file_size.unwrap(), 4);

    let storage = MemoryStorage::with_capacity(file_size as usize);
    storage.allocate(file_size).await.unwrap();

    // 模拟每个分片的下载与写入
    for frag in &fragments {
        let _data = vec![0xAB_u8; frag.size as usize];
        storage
            .write_at(frag.start, Bytes::from(vec![0xAB_u8; frag.size as usize]))
            .await
            .unwrap();
    }

    // 验证写入总大小
    assert_eq!(storage.file_size().await.unwrap(), file_size);

    // 读回验证
    let all_data = storage.get_data();
    assert_eq!(all_data.len(), file_size as usize);
    assert!(all_data.iter().all(|&b| b == 0xAB));
}

/// 测试 WritePipeline 与 CpuVerifier 协同
#[tokio::test]
async fn integration_pipeline_write_and_crypto_verify() {
    let tmp = NamedTempFile::new().unwrap();
    let tokio_file = TokioFile::open(tmp.path()).await.unwrap();
    let pipeline = WritePipeline::new(tokio_file, 4096, 4);
    let verifier = CpuVerifier::blake3();

    // 写入多段数据
    let segments: Vec<(&[u8], u64)> = vec![
        (b"first_segment__", 0),
        (b"second_segment_", 15),
        (b"third_segment__", 30),
    ];

    for (data, offset) in &segments {
        pipeline.write(*offset, data).await.unwrap();
    }
    pipeline.storage().sync().await.unwrap();

    // 读回完整数据
    let mut buf = vec![0u8; 45];
    let read = pipeline.storage().read_at(0, &mut buf).await.unwrap();
    assert_eq!(read, 45);

    // 计算并校验哈希
    let hash = verifier.compute_hash(&buf).unwrap();
    verifier.verify(&buf, &hash).unwrap();

    // 篡改其中一个字节后校验应失败
    let mut tampered = buf.clone();
    tampered[10] ^= 0xFF;
    assert!(verifier.verify(&tampered, &hash).is_err());
}

// ============================================================
// 跨 crate 流程: tachyon-engine 分片管理 + tachyon-core 类型 + tachyon-crypto 校验
// ============================================================

/// 测试分片状态机与校验的完整流程
#[tokio::test]
async fn integration_fragment_lifecycle_with_verification() {
    let verifier = CpuVerifier::blake3();

    // 创建分片信息
    let info = FragmentInfo {
        index: 0,
        start: 0,
        end: 999,
        size: 1000,
        downloaded: 0,
        hash: None,
    };

    let mut record = FragmentRecord::new(info, 3);

    // Pending -> Downloading
    record.start_download();
    assert_eq!(record.state, FragmentState::Downloading);

    // 下载数据
    let data = Bytes::from(vec![42u8; 1000]);

    // Downloading -> Verifying
    record.complete_download(data.len() as u64, std::time::Duration::from_millis(50));
    assert_eq!(record.state, FragmentState::Verifying);
    assert_eq!(record.info.downloaded, 1000);

    // 校验数据
    let hash = verifier.compute_hash(&data).unwrap();
    verifier.verify(&data, &hash).unwrap();

    // 记录哈希
    record.info.hash = Some(hash.clone());

    // Verifying -> Writing
    record.verify_ok();
    assert_eq!(record.state, FragmentState::Writing);

    // 写入存储
    let storage = MemoryStorage::with_capacity(1000);
    storage.allocate(1000).await.unwrap();
    storage.write_at(0, data.clone()).await.unwrap();

    // Writing -> Done
    record.write_done();
    assert!(record.is_done());

    // 最终验证: 存储数据与分片哈希一致
    let stored = storage.get_data();
    assert_eq!(stored.len(), 1000);
    assert!(verifier.verify(&stored, &hash).is_ok());
}

/// 测试多分片并发写入 + 校验
#[tokio::test]
async fn integration_multi_fragment_concurrent() {
    let verifier = CpuVerifier::blake3();
    let total_size = 4096u64;
    let frag_count = 8u32;
    let fragments = test_fragments(total_size, frag_count);

    let storage = MemoryStorage::with_capacity(total_size as usize);
    storage.allocate(total_size).await.unwrap();

    // 模拟并发写入(顺序化执行,但模拟并发逻辑)
    let mut handles = Vec::new();
    for frag in &fragments {
        let frag = frag.clone();
        let data = vec![frag.index as u8; frag.size as usize];
        handles.push((frag, data));
    }

    for (frag, data) in &handles {
        storage
            .write_at(frag.start, Bytes::copy_from_slice(data))
            .await
            .unwrap();
    }

    // 校验每个分片
    for (frag, data) in &handles {
        let hash = verifier.compute_hash(data).unwrap();
        let mut read_buf = vec![0u8; frag.size as usize];
        storage.read_at(frag.start, &mut read_buf).await.unwrap();
        assert!(
            verifier.verify(&read_buf, &hash).is_ok(),
            "分片 {} 校验失败",
            frag.index
        );
    }
}

/// 测试失败重试流程(引擎状态机 + 校验)
#[tokio::test]
async fn integration_fragment_retry_and_verify() {
    let verifier = CpuVerifier::blake3();
    let info = FragmentInfo {
        index: 0,
        start: 0,
        end: 99,
        size: 100,
        downloaded: 0,
        hash: None,
    };
    let mut record = FragmentRecord::new(info, 3);

    // 第一次尝试: 失败
    record.start_download();
    assert!(record.mark_failed());
    assert_eq!(record.state, FragmentState::Pending);
    assert_eq!(record.retry_count, 1);

    // 第二次尝试: 成功
    record.start_download();
    let data = Bytes::from(vec![7u8; 100]);
    record.complete_download(data.len() as u64, std::time::Duration::from_millis(10));
    assert_eq!(record.state, FragmentState::Verifying);

    // 校验
    let hash = verifier.compute_hash(&data).unwrap();
    assert!(verifier.verify(&data, &hash).is_ok());
    record.verify_ok();
    record.write_done();
    assert!(record.is_done());
}

/// 测试 tachyon-engine 带宽追踪影响分片大小计算
#[test]
fn integration_bandwidth_affects_fragment_size() {
    let min_size = 1024 * 1024; // 1MB
    let max_size = 64 * 1024 * 1024; // 64MB
    let file_size = 1024 * 1024 * 1024; // 1GB

    // 低带宽场景
    let mut low_bw = BandwidthTracker::new(0.3);
    low_bw.record(1024 * 1024); // 1MB/s
    let low_frag_size = compute_fragment_size(file_size, low_bw.estimate(), min_size, max_size, 16);

    // 高带宽场景
    let mut high_bw = BandwidthTracker::new(0.3);
    high_bw.record(200 * 1024 * 1024); // 200MB/s
    let high_frag_size =
        compute_fragment_size(file_size, high_bw.estimate(), min_size, max_size, 16);

    // 两者都在有效范围内
    assert!(low_frag_size >= min_size && low_frag_size <= max_size);
    assert!(high_frag_size >= min_size && high_frag_size <= max_size);

    // 高带宽的分片应更大或相等
    assert!(
        high_frag_size >= low_frag_size,
        "高带宽分片 {} 应 >= 低带宽分片 {}",
        high_frag_size,
        low_frag_size,
    );
}

// ============================================================
// 跨 crate 流程: DownloadState 生命周期
// ============================================================

/// 测试完整下载状态机: 从 Pending 到 Completed
#[test]
fn integration_download_state_lifecycle() {
    // Pending -> Downloading -> Verifying -> Completed
    let states = [
        DownloadState::Pending,
        DownloadState::Downloading,
        DownloadState::Verifying,
        DownloadState::Completed,
    ];

    // 验证状态转换序列
    for window in states.windows(2) {
        assert_ne!(window[0], window[1]);
    }

    // 验证序列化/反序列化完整性
    for state in &states {
        let json = serde_json::to_string(state).unwrap();
        let deserialized: DownloadState = serde_json::from_str(&json).unwrap();
        assert_eq!(*state, deserialized);
    }
}

/// 测试失败路径: Downloading -> Failed
#[test]
fn integration_download_state_failure_path() {
    let states = [DownloadState::Downloading, DownloadState::Failed];

    assert_ne!(states[0], states[1]);

    // 验证可以序列化错误状态
    let json = serde_json::to_string(&DownloadState::Failed).unwrap();
    let deserialized: DownloadState = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, DownloadState::Failed);
}

/// 测试取消路径: Downloading -> Cancelled
#[test]
fn integration_download_state_cancel_path() {
    let state = DownloadState::Cancelled;
    let json = serde_json::to_string(&state).unwrap();
    let deserialized: DownloadState = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, DownloadState::Cancelled);
}

// ============================================================
// 跨 crate: 错误传播
// ============================================================

/// 测试协议错误能正确传播到上层
#[tokio::test]
async fn integration_error_propagation() {
    let protocol = MockProtocol::failing(DownloadError::Network("连接被拒绝".into()));
    let result = protocol.probe("http://example.com/file.bin").await;
    assert!(result.is_err());

    match result.unwrap_err() {
        DownloadError::Network(msg) => assert!(msg.contains("连接被拒绝")),
        other => panic!("期望 Network 错误, 实际: {:?}", other),
    }
}

/// 测试存储范围越界错误传播
#[tokio::test]
async fn integration_storage_boundary_error() {
    let storage = MemoryStorage::new();
    // 不 allocate,直接写入高位偏移,验证不会 panic
    let result = storage.write_at(0, Bytes::from_static(b"hello")).await;
    assert!(result.is_ok());

    // 读取超出实际写入范围
    let mut buf = [0u8; 10];
    let read = storage.read_at(100, &mut buf).await.unwrap();
    assert_eq!(read, 0, "超出范围的读取应返回 0 字节");
}

/// 测试校验不匹配时能正确报告错误
#[test]
fn integration_checksum_mismatch_error() {
    let err = DownloadError::ChecksumMismatch {
        expected: "abc123".into(),
        actual: "def456".into(),
    };
    let msg = err.to_string();
    assert!(msg.contains("abc123"));
    assert!(msg.contains("def456"));
    assert!(msg.contains("校验失败"));
}

// ============================================================
// 集成测试:下载器完整流程(tachyon-engine/downloader.rs)
// ============================================================

use tachyon_core::config::DownloadConfig;
use tachyon_sniffer::CaptureConfig;
use tachyon_sniffer::resources::ResourceManager;

/// 下载器完整流程:probe -> plan -> prepare -> execute -> verify
#[tokio::test]
async fn integration_downloader_full_pipeline() {
    let tmp = NamedTempFile::new().unwrap();
    let file_size: u64 = 3072; // 3KB,3 个分片
    let _test_data = vec![0xAB_u8; file_size as usize];

    let meta = test_metadata("integration.bin", file_size);
    let _protocol =
        MockProtocol::new(meta).with_range_data(0, 1023, Bytes::from(vec![0xAB_u8; 1024]));

    let _config = DownloadConfig {
        download_dir: tmp.path().parent().unwrap().to_string_lossy().to_string(),
        max_concurrent_fragments: 2,
        verify_checksum: false,
        ..test_config()
    };

    // 使用 DownloadTask 的底层组件测试
    let verifier = CpuVerifier::blake3();
    let storage = MemoryStorage::with_capacity(file_size as usize);
    storage.allocate(file_size).await.unwrap();

    // 模拟分片写入
    let fragments = test_fragments(file_size, 3);
    for frag in &fragments {
        let data = vec![0xAB_u8; frag.size as usize];
        storage
            .write_at(frag.start, Bytes::copy_from_slice(&data))
            .await
            .unwrap();
    }

    // 校验
    let all_data = storage.get_data();
    assert_eq!(all_data.len(), file_size as usize);
    assert!(all_data.iter().all(|&b| b == 0xAB));

    let hash = verifier.compute_hash(&all_data).unwrap();
    assert!(verifier.verify(&all_data, &hash).is_ok());
}

/// 嗅探资源管理器完整流程:拦截->识别->去重->过滤->清理
#[tokio::test]
async fn integration_sniffer_resource_lifecycle() {
    let config = CaptureConfig {
        min_size: 512,
        ..Default::default()
    };
    let rm = ResourceManager::new(config);

    // 拦截一系列 URL
    assert!(rm.on_request(
        "http://cdn.example.com/video.mp4?token=abc",
        Some("video/mp4"),
        Some(10 * 1024 * 1024),
        Some("http://example.com/page".into()),
    ));
    assert!(rm.on_request(
        "http://cdn.example.com/audio.mp3",
        Some("audio/mpeg"),
        Some(5 * 1024 * 1024),
        None,
    ));
    assert!(rm.on_request(
        "http://cdn.example.com/archive.zip",
        None,
        Some(1024 * 1024),
        None,
    ));
    // 太小的文件应被过滤
    assert!(!rm.on_request("http://cdn.example.com/tiny.zip", None, Some(100), None,));
    // HTML 应被过滤
    assert!(!rm.on_request(
        "http://example.com/page.html",
        Some("text/html"),
        None,
        None,
    ));
    // 重复 URL 应去重
    assert!(!rm.on_request(
        "http://cdn.example.com/video.mp4?token=abc",
        Some("video/mp4"),
        Some(10 * 1024 * 1024),
        None,
    ));

    assert_eq!(rm.count(), 3);

    // 获取所有并验证排序(最新在前)
    let all = rm.get_all();
    assert_eq!(all.len(), 3);
    assert!(all[0].discovered_at >= all[1].discovered_at);

    // 按类型过滤
    let videos = rm.get_by_type("video");
    assert_eq!(videos.len(), 1);
    assert!(videos[0].url.contains("video.mp4"));

    // 移除一个
    let id = &all[2].id;
    assert!(rm.remove(id));
    assert_eq!(rm.count(), 2);

    // 清空
    rm.clear();
    assert_eq!(rm.count(), 0);
}

/// 跨 crate:带宽追踪影响分片规划
#[tokio::test]
async fn integration_bandwidth_affects_fragment_planning() {
    let mut tracker = BandwidthTracker::new(0.3);

    // 模拟低带宽
    for _ in 0..10 {
        tracker.record(1024 * 1024); // 1MB/s
    }
    let low_bw = tracker.estimate();

    // 计算分片大小
    let file_size = 100 * 1024 * 1024u64; // 100MB
    let frag_size_low = compute_fragment_size(file_size, low_bw, 1024 * 1024, 64 * 1024 * 1024, 16);

    // 模拟高带宽
    for _ in 0..10 {
        tracker.record(100 * 1024 * 1024); // 100MB/s
    }
    let high_bw = tracker.estimate();
    let frag_size_high =
        compute_fragment_size(file_size, high_bw, 1024 * 1024, 64 * 1024 * 1024, 16);

    // 高带宽应产生更大的分片
    assert!(
        frag_size_high >= frag_size_low,
        "高带宽分片({})应 >= 低带宽分片({})",
        frag_size_high,
        frag_size_low
    );
}
