use super::*;
use librqbit::spawn_utils::BlockingSpawner;
use librqbit::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, Session, SessionOptions, create_torrent,
};

#[tokio::test(flavor = "multi_thread")]
async fn factory_create_covers_auto_open_and_registry_paths() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let file_path = dir.path().join("data.bin");
    std::fs::write(&file_path, b"factory data").expect("写入种子文件失败");
    let torrent = create_torrent(
        &file_path,
        CreateTorrentOptions {
            name: None,
            piece_length: Some(16),
            trackers: Vec::new(),
        },
        &BlockingSpawner::new(2),
    )
    .await
    .expect("创建 torrent 失败");

    let session = Session::new_with_opts(
        dir.path().to_path_buf(),
        SessionOptions {
            dht: None,
            listen: None,
            persistence: None,
            ..Default::default()
        },
    )
    .await
    .expect("创建 Session 失败");
    let handle = session
        .add_torrent(
            AddTorrent::from_bytes(torrent.as_bytes().expect("torrent bytes")),
            Some(AddTorrentOptions {
                paused: false,
                output_folder: Some(dir.path().to_string_lossy().into_owned()),
                overwrite: true,
                disable_trackers: true,
                ..Default::default()
            }),
        )
        .await
        .expect("加入 torrent 失败")
        .into_handle()
        .expect("应取得 torrent handle");
    handle
        .wait_until_completed()
        .await
        .expect("本地 initial check 应完成");

    let factory = TachyonStorageFactory::new(
        tokio::runtime::Handle::current(),
        tachyon_core::config::IoStrategy::Standard,
        dir.path().to_path_buf(),
    );
    let auto_storage = handle
        .with_metadata(|metadata| factory.create(handle.shared(), metadata))
        .expect("读取 torrent metadata 失败")
        .expect("自动打开 storage 失败");
    assert_eq!(factory.last_open_backend(), Some("Standard"));
    auto_storage
        .pwrite_all(0, 0, b"auto")
        .expect("自动打开 storage 应可写");

    let info_hash = handle.info_hash().as_string();
    let registered = Arc::new(InMemStorage::new()) as Arc<dyn AsyncStorage>;
    factory.register(info_hash.clone(), vec![registered]);
    let registered_storage = handle
        .with_metadata(|metadata| factory.create(handle.shared(), metadata))
        .expect("读取注册路径 metadata 失败")
        .expect("注册 storage 创建失败");
    registered_storage
        .pwrite_all(0, 0, b"registered")
        .expect("注册 storage 应可写");
    factory.unregister(&info_hash);
}
