use tachyon_io::iocp::{IoCpState, IoCpStorage};

#[cfg(target_os = "windows")]
use bytes::Bytes;
#[cfg(not(target_os = "windows"))]
use tachyon_core::DownloadError;
#[cfg(target_os = "windows")]
use tachyon_io::AsyncStorage;

fn create_temp_file(
    file_name: &str,
) -> Result<(tempfile::TempDir, std::path::PathBuf), std::io::Error> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(file_name);
    std::fs::File::create(&path)?;
    Ok((dir, path))
}

#[cfg(not(target_os = "windows"))]
#[test]
fn test_iocp_contract_non_windows_init_returns_unsupported_and_preserves_basic_state()
-> Result<(), Box<dyn std::error::Error>> {
    let (_dir, path) = create_temp_file("iocp_contract_non_windows.bin")?;
    let mut storage = IoCpStorage::new(&path);

    assert_eq!(storage.path(), path.as_path());
    assert_eq!(storage.state(), IoCpState::Created);

    let err = storage
        .init()
        .expect_err("非 Windows 初始化必须返回 Unsupported");

    match err {
        DownloadError::Io(io_error) => {
            assert_eq!(io_error.kind(), std::io::ErrorKind::Unsupported);
        }
        other => panic!("非 Windows 初始化应返回 I/O Unsupported 错误,实际: {other}"),
    }
    assert_eq!(storage.path(), path.as_path());
    assert_eq!(storage.state(), IoCpState::Created);

    Ok(())
}

#[cfg(target_os = "windows")]
#[tokio::test]
async fn test_iocp_contract_windows_async_storage_core_download_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let (_dir, path) = create_temp_file("iocp_contract_windows.bin")?;
    let mut storage = IoCpStorage::new(&path);

    assert_eq!(storage.path(), path.as_path());
    assert_eq!(storage.state(), IoCpState::Created);

    storage.init()?;
    assert_eq!(storage.state(), IoCpState::Ready);

    let allocated_size = 8192;
    storage.allocate(allocated_size).await?;
    assert_eq!(storage.file_size().await?, allocated_size);

    let head = Bytes::from_static(b"tachyon-iocp-head");
    let written = storage.write_at(0, head.clone()).await?;
    assert_eq!(written, head.len());

    let mut head_buf = vec![0; head.len()];
    let read = storage.read_at(0, &mut head_buf).await?;
    assert_eq!(read, head.len());
    assert_eq!(head_buf.as_slice(), head.as_ref());

    let offset_payload = Bytes::from_static(b"offset-contract-payload");
    let offset = 4096;
    let offset_written = storage.write_at(offset, offset_payload.clone()).await?;
    assert_eq!(offset_written, offset_payload.len());

    let mut offset_buf = vec![0; offset_payload.len()];
    let offset_read = storage.read_at(offset, &mut offset_buf).await?;
    assert_eq!(offset_read, offset_payload.len());
    assert_eq!(offset_buf.as_slice(), offset_payload.as_ref());

    storage.sync().await?;
    storage.close().await?;

    Ok(())
}

#[cfg(target_os = "windows")]
#[tokio::test]
async fn test_iocp_contract_windows_concurrent_offset_writes()
-> Result<(), Box<dyn std::error::Error>> {
    let (_dir, path) = create_temp_file("iocp_contract_concurrent.bin")?;
    let mut storage = IoCpStorage::new(&path);
    storage.init()?;
    storage.allocate(16 * 512).await?;

    let storage = std::sync::Arc::new(storage);
    let mut handles = Vec::new();
    for index in 0u8..16 {
        let storage = storage.clone();
        handles.push(tokio::spawn(async move {
            let offset = index as u64 * 512;
            let payload = Bytes::from(vec![index; 512]);
            let written = storage.write_at(offset, payload).await?;
            assert_eq!(written, 512);
            Ok::<_, tachyon_core::DownloadError>(())
        }));
    }

    for handle in handles {
        handle.await??;
    }

    for index in 0u8..16 {
        let offset = index as u64 * 512;
        let mut buf = vec![0u8; 512];
        let read = storage.read_at(offset, &mut buf).await?;
        assert_eq!(read, 512);
        assert!(
            buf.iter().all(|&byte| byte == index),
            "并发写入区域 {offset} 数据不一致"
        );
    }

    storage.sync().await?;
    storage.close().await?;

    Ok(())
}
