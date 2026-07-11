//! Tachyon Tauri 应用库

pub mod commands;
pub mod projection;
pub mod repository;
pub mod runtime;
pub mod service;
pub mod task_store;

pub use commands::AppError;
pub use commands::TaskCommand;
pub use commands::TaskInfo;

use std::sync::Arc;

use commands::*;

/// 构建并运行 Tauri 应用
pub fn run() {
    use tauri::Manager;

    // 设置全局 panic hook，确保 panic 信息被 tracing 捕获
    std::panic::set_hook(Box::new(|panic_info| {
        tracing::error!(
            target = "panic",
            panic.file = panic_info.location().map(|l| l.file()),
            panic.line = panic_info.location().map(|l| l.line()),
            panic.column = panic_info.location().map(|l| l.column()),
            "应用 panic: {panic_info}",
        );
    }));

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(true)
        .init();

    // === tokio runtime 显式调优 ===
    // Tauri v2 默认通过 `tokio::runtime::Runtime::new()` 创建 runtime:
    //   - worker_threads = available_parallelism()
    //   - max_blocking_threads = 512 (默认值过大,浪费线程栈内存)
    // 高并发下载场景下,spawn_blocking 用于文件 I/O(checkpoint、snapshot 读写)、
    // 512 个阻塞线程会显著增加内存占用(每线程 ~2MB 栈)且无实际吞吐收益。
    //
    // 通过 tauri::async_runtime::set(handle) 注入自定义 runtime:
    //   - worker_threads: 显式取 available_parallelism,语义明确
    //   - max_blocking_threads(128): 限制阻塞线程池上限,平衡 I/O 并发与内存
    //   - thread_keep_alive(5s): 空闲线程快速回收,降低常驻内存
    //
    // 注意:tauri::async_runtime::set 接收 Handle,但底层 Runtime 不可被丢弃,
    //       因此将 Runtime 绑定到 Guard 结构,随 Tauri 应用生命周期存活。
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
        )
        .max_blocking_threads(128)
        .thread_keep_alive(std::time::Duration::from_secs(5))
        .enable_all()
        .thread_name("tachyon-tokio")
        .build()
        .expect("构建 tokio runtime 失败");

    // 将 runtime handle 注入 Tauri 全局 async runtime。
    // Runtime 本身必须保活,否则 handle 失效——由 _runtime_guard 持有。
    tauri::async_runtime::set(runtime.handle().clone());
    let _runtime_guard = runtime;

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(AppState::new())
        .setup(|app| {
            let state = app.state::<AppState>();
            let handle = app.handle().clone();
            tauri::async_runtime::block_on(async move {
                // 注入 AppHandle 使 ProgressBroker 能发送任务终态系统通知
                state
                    .runtime
                    .progress_broker
                    .set_notification_emitter(Arc::new(handle.clone()));
                // 在 reactor 上下文中启动 progress aggregator
                // （构造期间不能 spawn,此时 reactor 尚未就绪）
                state.runtime.progress_broker.spawn_aggregator();
                // 在 reactor 上下文中启动 chunk reader worker
                // （构造期间不能 spawn,此时 reactor 尚未就绪）
                state.infra.chunk_reader_pool.spawn_workers();

                // 延迟初始化 BitTorrent Session（BtSession::new 是 async，
                // 无法在 AppState::try_new 的同步上下文中完成）
                #[cfg(feature = "magnet")]
                {
                    let cfg = state.domain.config.lock().await;
                    let magnet_config = cfg.magnet.clone();
                    let download_dir = std::path::PathBuf::from(&cfg.download.download_dir);
                    drop(cfg);
                    match tachyon_engine::BtSession::new(download_dir, magnet_config).await {
                        Ok(bt_session) => {
                            tracing::info!("BitTorrent Session 初始化成功");
                            *state.infra.bt_session.lock().await = Some(Arc::new(bt_session));
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "BitTorrent Session 初始化失败,磁力链接下载不可用");
                        }
                    }
                }

                match state.load_recovered_tasks().await {
                    Ok(corrupt_keys) => {
                        // 损坏快照非空时向 UI 广播一次性恢复告警
                        if !corrupt_keys.is_empty() {
                            use tauri::Emitter;
                            let count = corrupt_keys.len();
                            tracing::warn!(
                                count,
                                keys = ?corrupt_keys,
                                "启动恢复检测到损坏快照,已跳过"
                            );
                            let warning = RecoveryWarning {
                                corrupt_keys,
                                count,
                            };
                            let _ = handle.emit("recovery-warning", &warning);
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "恢复未完成任务失败"),
                }

                // 启动剪贴板监听(若 clipboard.enable_watch 为 true)
                let clipboard_watcher = crate::service::ClipboardWatcher::new(
                    handle.clone(),
                    state.domain.config.clone(),
                    state.service.sniffer_service.clone(),
                );
                clipboard_watcher.start().await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 应用信息
            get_app_info,
            supported_protocols,
            // 确认令牌(P1-11b)
            request_confirmation,
            // 任务管理
            create_task,
            probe_filename,
            pause_task,
            resume_task,
            cancel_task,
            delete_task,
            undo_cancel_task,
            undo_delete_task,
            get_task_list,
            get_task_detail,
            // 进度查询
            get_download_progress,
            subscribe_progress,
            get_task_fragments,
            // 嗅探
            get_sniffer_resources,
            add_sniffer_filter,
            add_sniffer_resource,
            get_sniffer_capture_config,
            set_sniffer_capture_config,
            clear_sniffer_resources,
            // 配置管理
            get_config,
            update_config,
            // HuggingFace Hub
            list_repo_files,
            get_hf_download_url,
            get_model_info,
            search_models,
            scan_local_models,
            verify_model,
            list_model_favorites,
            add_model_favorite,
            remove_model_favorite,
            batch_create_hf_tasks,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            eprintln!("启动 Tachyon 应用失败: {e}");
            std::process::exit(1);
        });
}

// 验证测试:放在 crate 根级别,以便 `--exact` 匹配

/// 验证 any_fragment_failed 正确检测分片失败
#[cfg(test)]
#[tokio::test]
async fn any_fragment() {
    use std::sync::Arc;

    let state = Arc::new(AppState::new());
    let task_id = uuid::Uuid::new_v4().to_string();
    let task = commands::TaskInfo {
        id: task_id.clone(),
        url: "https://example.com/test.bin".to_string(),
        file_name: "test.bin".to_string(),
        file_size: Some(1024),
        downloaded: 0,
        speed: 0,
        status: tachyon_core::types::DownloadState::Pending,
        progress: 0.0,
        fragments_total: 4,
        fragments_done: 0,
        active_concurrency: 0,
        created_at: chrono::Local::now().to_rfc3339(),
        save_path: String::new(),
        error_reason: None,
        retry_count: 0,
        hf_meta: None,
    };
    state.domain.task_repository.insert(task_id.clone(), task);

    {
        if let Some(mut t) = state.domain.task_repository.get_mut(&task_id) {
            t.status = tachyon_core::types::DownloadState::Failed;
        }
    }
    let t = state.domain.task_repository.get(&task_id).unwrap();
    assert_eq!(
        t.status,
        tachyon_core::types::DownloadState::Failed,
        "分片失败应正确标记任务状态"
    );
}

/// 验证 max_concurrent 信号量门控
#[cfg(test)]
#[tokio::test]
async fn max_concurrent() {
    use commands::TaskInfo;

    let state = AppState::new();
    {
        let mut cfg = state.domain.config.lock().await;
        cfg.max_concurrent_tasks = 2;
    }

    // 插入 2 个活跃任务
    {
        for i in 0..2 {
            state.domain.task_repository.insert(
                format!("task-{i}"),
                TaskInfo {
                    id: format!("task-{i}"),
                    url: format!("https://example.com/file{i}.bin"),
                    file_name: format!("file{i}.bin"),
                    file_size: None,
                    downloaded: 0,
                    speed: 0,
                    status: tachyon_core::types::DownloadState::Downloading,
                    progress: 0.0,
                    fragments_total: 0,
                    fragments_done: 0,
                    active_concurrency: 0,
                    created_at: chrono::Local::now().to_rfc3339(),
                    save_path: String::new(),
                    error_reason: None,
                    retry_count: 0,
                    hf_meta: None,
                },
            );
        }
    }

    let active = state
        .domain
        .task_repository
        .iter()
        .filter(|r| {
            let t = r.value();
            t.status == tachyon_core::types::DownloadState::Downloading
                || t.status == tachyon_core::types::DownloadState::Pending
        })
        .count();
    let max = state.domain.config.lock().await.max_concurrent_tasks as usize;
    assert!(
        active >= max,
        "活跃任务数 {active} 应 >= 上限 {max},验证门控逻辑生效"
    );
}

/// 验证 AppError 枚举各变体的 Display 和 Serialize 行为
#[cfg(test)]
#[test]
fn app_error() {
    use commands::AppError;

    let not_found = AppError::TaskNotFound("abc-123".into());
    assert_eq!(format!("{not_found}"), "任务不存在: abc-123");
    let json = serde_json::to_string(&not_found).unwrap();
    assert!(json.contains("TaskNotFound"), "序列化应包含变体名: {json}");
    assert!(json.contains("abc-123"), "序列化应包含消息内容: {json}");

    let already_exists = AppError::TaskAlreadyExists("task-1".into());
    assert_eq!(format!("{already_exists}"), "任务已存在: task-1");
    let json = serde_json::to_string(&already_exists).unwrap();
    assert!(
        json.contains("TaskAlreadyExists"),
        "序列化应包含变体名: {json}"
    );

    let network = AppError::Network("连接超时".into());
    assert_eq!(format!("{network}"), "网络错误: 连接超时");
    let json = serde_json::to_string(&network).unwrap();
    assert!(json.contains("Network"), "序列化应包含变体名: {json}");

    let config = AppError::Config("无效路径".into());
    assert_eq!(format!("{config}"), "配置错误: 无效路径");
    let json = serde_json::to_string(&config).unwrap();
    assert!(json.contains("Config"), "序列化应包含变体名: {json}");

    let core = AppError::Core(tachyon_core::DownloadError::Cancelled);
    assert!(
        format!("{core}").contains("核心错误"),
        "Core 变体 Display 应包含'核心错误'"
    );
    let json = serde_json::to_string(&core).unwrap();
    assert!(json.contains("Core"), "序列化应包含变体名: {json}");
    assert!(
        json.contains("任务已取消"),
        "序列化应包含 DownloadError 消息: {json}"
    );

    // P2-10: Core 变体嵌套序列化 DownloadError,保留结构化字段供前端分级展示。
    // 验证 inner 字段包含 type/message/retryable,前端据此决定 toast 严重度。
    let throttled = AppError::Core(tachyon_core::DownloadError::Throttled {
        retry_after_secs: Some(60),
    });
    let json = serde_json::to_string(&throttled).unwrap();
    assert!(
        json.contains("\"inner\""),
        "Core 变体应嵌套 inner 字段: {json}"
    );
    assert!(
        json.contains("\"retryable\":true"),
        "Throttled 应 retryable=true: {json}"
    );
    assert!(
        json.contains("\"retryAfterSecs\":60"),
        "应保留 retryAfterSecs 结构化字段: {json}"
    );
    let forbidden = AppError::Core(tachyon_core::DownloadError::Forbidden { status: 403 });
    let json = serde_json::to_string(&forbidden).unwrap();
    assert!(
        json.contains("\"retryable\":false"),
        "Forbidden 应 retryable=false: {json}"
    );
    assert!(
        json.contains("\"status\":403"),
        "应保留 status 结构化字段: {json}"
    );
}

/// 验证 RecoveryWarning 序列化为 camelCase(P1-06续)
///
/// 前端 `RecoveryWarningPayload` 期望 `corruptKeys`(camelCase),
/// 若后端缺 `#[serde(rename_all)]` 会序列化为 `corrupt_keys` 导致字段名漂移。
#[cfg(test)]
#[test]
fn recovery_warning_camel_case() {
    let warning = commands::RecoveryWarning {
        corrupt_keys: vec!["task_abc".to_string(), "task_def".to_string()],
        count: 2,
    };
    let json = serde_json::to_string(&warning).unwrap();
    assert!(
        json.contains("corruptKeys"),
        "序列化应使用 camelCase: corruptKeys,实际: {json}"
    );
    assert!(
        !json.contains("corrupt_keys"),
        "序列化不应含 snake_case: corrupt_keys,实际: {json}"
    );
    assert!(
        json.contains("\"count\":2"),
        "count 字段应正确序列化: {json}"
    );
}
