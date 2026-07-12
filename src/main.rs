//! Tachyon 磁力链接下载测试工具
//!
//! 用于验证真实 BT 网络下的磁力链接下载路径。
//! 用法: cargo run --bin tachyon -- <magnet_url> <output_dir>

use std::sync::Arc;
use tachyon_core::config::DownloadConfig;
use tachyon_engine::bt_session::BtSession;
use tachyon_engine::downloader::DownloadTask;
use tachyon_scheduler::AdaptiveDownloadScheduler;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 启用 tracing(看 BT 下载详细日志)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tachyon_engine=debug,tachyon_protocol=debug".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("用法: {} <magnet_url> <output_dir>", args[0]);
        std::process::exit(1);
    }
    let magnet_url = &args[1];
    let output_dir = std::path::PathBuf::from(&args[2]);

    println!("磁力链接: {magnet_url}");
    println!("输出目录: {}", output_dir.display());

    // 创建 BT Session(配置 SOCKS5 代理,BT tracker+peer 走代理)
    let bt_config = tachyon_core::config::MagnetConfig {
        socks_proxy_url: Some("socks5://127.0.0.1:7897".to_string()),
        metadata_timeout_secs: 180,
        enable_dht: true,
        disable_dht_when_socks: false,
        stall_timeout_secs: 300,
        peer_wait_timeout_secs: 120,
        ..Default::default()
    };
    let bt_session = Arc::new(BtSession::new(output_dir.clone(), bt_config).await?);

    // 创建下载任务(自动选择 MagnetProtocol)
    let scheduler = Arc::new(AdaptiveDownloadScheduler::default_config());
    let mut config = DownloadConfig::default();
    config.download_dir = output_dir.to_string_lossy().to_string();
    config.authorized_dirs = vec![config.download_dir.clone()];

    let mut task = DownloadTask::with_pool_and_scheduler(
        magnet_url.clone(),
        config,
        None,
        scheduler,
        Some(bt_session),
    )
    .await?;

    println!("开始 probe...");
    task.probe().await?;
    let metadata = task.metadata().expect("probe 后应有 metadata");
    println!(
        "文件名: {}, 大小: {:?}, supports_range: {}, protocol_managed_storage: {}",
        metadata.file_name,
        metadata.file_size,
        metadata.supports_range,
        metadata.protocol_managed_storage
    );

    println!("开始下载...");
    let start = std::time::Instant::now();
    match task.run().await {
        Ok(()) => {
            let elapsed = start.elapsed();
            println!("下载完成! 耗时: {:.2}s", elapsed.as_secs_f64());
        }
        Err(e) => {
            eprintln!("下载失败: {e:?}");
            return Err(e.into());
        }
    }
    Ok(())
}
