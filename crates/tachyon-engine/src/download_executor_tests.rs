use super::*;
use bytes::Bytes;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

fn test_storage(size: usize) -> Arc<StorageSet> {
    Arc::new(StorageSet::single(StorageKind::memory_with_capacity(size)))
}

#[tokio::test]
async fn test_flush_batch_matrix() {
    let storage = test_storage(64);
    let mut control_rx = None;
    let mut hasher = Some(default_blake3_verifier().new_hasher());
    let rate_limiter = Some(Arc::new(RateLimiter::new(u64::MAX)));
    let batch = Bytes::from_static(b"flush-batch");

    let (new_pos, written) = DownloadTask::flush_batch(
        &storage,
        3,
        batch.clone(),
        &mut hasher,
        0,
        0,
        batch.len() as u64,
        &rate_limiter,
        &mut control_rx,
        Duration::ZERO,
        false,
        None,
    )
    .await
    .expect("正常 batch 应写入");
    assert_eq!(
        (new_pos, written),
        (3 + batch.len() as u64, batch.len() as u64)
    );
    assert!(!hasher.unwrap().finalize().is_empty());

    let (new_pos, written) = DownloadTask::flush_batch(
        &storage,
        0,
        Bytes::from_static(b"skip"),
        &mut None,
        1,
        0,
        4,
        &None,
        &mut None,
        Duration::ZERO,
        true,
        None,
    )
    .await
    .expect("skip_write 应推进偏移");
    assert_eq!((new_pos, written), (4, 4));

    let overflow = DownloadTask::flush_batch(
        &storage,
        0,
        Bytes::from_static(b"x"),
        &mut None,
        2,
        u64::MAX,
        u64::MAX,
        &None,
        &mut None,
        Duration::ZERO,
        false,
        None,
    )
    .await
    .expect_err("batch 总长度溢出必须失败");
    assert!(overflow.to_string().contains("溢出"));

    let overlong = DownloadTask::flush_batch(
        &storage,
        0,
        Bytes::from_static(b"12345"),
        &mut None,
        3,
        4,
        4,
        &None,
        &mut None,
        Duration::ZERO,
        false,
        None,
    )
    .await
    .expect_err("batch 超过分片长度必须失败");
    assert!(overlong.to_string().contains("越界"));
}

#[tokio::test]
async fn test_sync_and_progress_helper_matrix() {
    let storage = test_storage(64);
    let loose_bytes = Arc::new(AtomicU64::new(0));
    let loose_completed = Arc::new(AtomicUsize::new(0));

    DownloadTask::maybe_loose_sync_on_written_bytes(
        &storage,
        true,
        tachyon_core::config::CrashConsistencyMode::Loose,
        &loose_bytes,
        1,
    )
    .await
    .unwrap();
    DownloadTask::maybe_loose_sync_on_written_bytes(
        &storage,
        false,
        tachyon_core::config::CrashConsistencyMode::EveryFragment,
        &loose_bytes,
        1,
    )
    .await
    .unwrap();
    DownloadTask::maybe_loose_sync_on_written_bytes(
        &storage,
        false,
        tachyon_core::config::CrashConsistencyMode::Loose,
        &loose_bytes,
        256 * 1024,
    )
    .await
    .unwrap();
    assert_eq!(loose_bytes.load(Ordering::Acquire), 256 * 1024);

    DownloadTask::sync_on_fragment_complete(
        &storage,
        true,
        tachyon_core::config::CrashConsistencyMode::EveryFragment,
        &loose_completed,
    )
    .await
    .unwrap();
    DownloadTask::sync_on_fragment_complete(
        &storage,
        false,
        tachyon_core::config::CrashConsistencyMode::EveryFragment,
        &loose_completed,
    )
    .await
    .unwrap();
    for _ in 0..8 {
        DownloadTask::sync_on_fragment_complete(
            &storage,
            false,
            tachyon_core::config::CrashConsistencyMode::Loose,
            &loose_completed,
        )
        .await
        .unwrap();
    }
    assert_eq!(loose_completed.load(Ordering::Acquire), 8);

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let progress = Some(tx.clone());
    DownloadTask::report_progress(0, 1, &progress);
    DownloadTask::report_progress(0, 2, &progress);
    assert!(matches!(
        rx.recv().await,
        Some(FragmentProgress::Chunk { .. })
    ));
    drop(rx);
    DownloadTask::report_progress(0, 3, &Some(tx));
    DownloadTask::report_progress(0, 4, &None);
}

#[tokio::test]
async fn test_download_single_fragment_already_resumed_and_hashed() {
    let end = 31u64;
    let protocol: Arc<dyn Protocol> = Arc::new(
        MockProto::new(FileMetadata {
            file_name: "resume-direct.bin".into(),
            file_size: Some(end + 1),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        })
        .with_range_data(0, end, Bytes::from(vec![0x55; (end + 1) as usize])),
    );
    let storage = test_storage((end + 1) as usize);
    let shared = FragmentShared {
        effective_end: Arc::new(AtomicU64::new(end)),
        realtime_downloaded: Arc::new(AtomicU64::new(0)),
    };
    let loose_completed = Arc::new(AtomicUsize::new(0));
    let loose_bytes = Arc::new(AtomicU64::new(0));
    let mut write_buf = AlignedBuf::new(WRITE_BATCH_BYTES).unwrap();
    let verifier = default_blake3_verifier();
    let result = DownloadTask::download_single_fragment(
        &protocol,
        &storage,
        &None,
        "example.com",
        "http://example.com/resume-direct.bin",
        0,
        0,
        end,
        end + 1,
        Duration::ZERO,
        None,
        &None,
        &None,
        &verifier,
        true,
        &mut write_buf,
        false,
        tachyon_core::config::CrashConsistencyMode::EveryFragment,
        &loose_completed,
        &loose_bytes,
        &shared,
        None,
        None,
        None,
    )
    .await
    .expect("已续满分片应直接完成");
    assert_eq!(result.0, end + 1);
    assert_eq!(result.1, Duration::ZERO);
    assert!(result.2.is_none());
}

#[tokio::test]
async fn test_download_single_fragment_aligned_large_path() {
    let end = WRITE_BATCH_BYTES as u64 - 1;
    let mut aligned = AlignedBuf::new(WRITE_BATCH_BYTES).unwrap();
    aligned.extend_from_slice(&vec![0x7A; WRITE_BATCH_BYTES]);
    let data = aligned.freeze();
    assert!(tachyon_io::satisfies_no_buffering_alignment(0, &data));
    let protocol: Arc<dyn Protocol> = Arc::new(
        MockProto::new(FileMetadata {
            file_name: "aligned-fragment.bin".into(),
            file_size: Some(end + 1),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        })
        .with_range_data(0, end, data),
    );
    let storage = test_storage(WRITE_BATCH_BYTES);
    let shared = FragmentShared {
        effective_end: Arc::new(AtomicU64::new(end)),
        realtime_downloaded: Arc::new(AtomicU64::new(0)),
    };
    let completed = Arc::new(AtomicUsize::new(0));
    let partial = Arc::new(AtomicU64::new(0));
    let mut write_buf = AlignedBuf::new(WRITE_BATCH_BYTES).unwrap();
    let verifier = default_blake3_verifier();
    let result = DownloadTask::download_single_fragment(
        &protocol,
        &storage,
        &None,
        "example.com",
        "http://example.com/aligned-fragment.bin",
        0,
        0,
        end,
        0,
        Duration::ZERO,
        None,
        &None,
        &None,
        &verifier,
        false,
        &mut write_buf,
        false,
        tachyon_core::config::CrashConsistencyMode::Loose,
        &completed,
        &partial,
        &shared,
        None,
        None,
        None,
    )
    .await
    .expect("对齐大块分片应成功");
    assert_eq!(result.0, end + 1);
}

#[tokio::test]
async fn test_download_single_fragment_rejects_invalid_effective_end() {
    let protocol: Arc<dyn Protocol> = Arc::new(MockProto::new(FileMetadata {
        file_name: "invalid.bin".into(),
        file_size: Some(32),
        content_type: None,
        supports_range: true,
        etag: None,
        last_modified: None,
        file_layout: None,
        protocol_managed_storage: false,
        resolved_host: None,
    }));
    let storage = test_storage(32);
    let shared = FragmentShared {
        effective_end: Arc::new(AtomicU64::new(0)),
        realtime_downloaded: Arc::new(AtomicU64::new(0)),
    };
    let completed = Arc::new(AtomicUsize::new(0));
    let partial = Arc::new(AtomicU64::new(0));
    let mut write_buf = AlignedBuf::new(WRITE_BATCH_BYTES).unwrap();
    let verifier = default_blake3_verifier();
    let error = DownloadTask::download_single_fragment(
        &protocol,
        &storage,
        &None,
        "example.com",
        "http://example.com/invalid.bin",
        0,
        10,
        20,
        0,
        Duration::ZERO,
        None,
        &None,
        &None,
        &verifier,
        false,
        &mut write_buf,
        false,
        tachyon_core::config::CrashConsistencyMode::Loose,
        &completed,
        &partial,
        &shared,
        None,
        None,
        None,
    )
    .await
    .expect_err("effective_end 早于 start 必须失败");
    assert!(error.to_string().contains("范围非法"));
}
