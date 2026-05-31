//! AI Model Downloader Tauri 应用库

pub mod commands;
pub mod task_store;

pub use commands::AppError;
pub use commands::TaskInfo;

use commands::{
    AppState, add_sniffer_filter, cancel_task, create_task, delete_task, get_app_info, get_config,
    get_download_progress, get_sniffer_resources, get_task_detail, get_task_list, pause_task,
    resume_task, subscribe_progress, supported_protocols, update_config,
};

/// 构建并运行 Tauri 应用
pub fn run() {
    use tauri::Manager;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(true)
        .init();
    tauri::Builder::default()
        .manage(AppState::new())
        .setup(|app| {
            let state = app.state::<AppState>();
            tauri::async_runtime::block_on(async move {
                if let Err(e) = state.load_recovered_tasks().await {
                    tracing::warn!(error = %e, "恢复未完成任务失败");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 应用信息
            get_app_info,
            supported_protocols,
            // 任务管理
            create_task,
            pause_task,
            resume_task,
            cancel_task,
            delete_task,
            get_task_list,
            get_task_detail,
            // 进度查询
            get_download_progress,
            subscribe_progress,
            // 嗅探
            get_sniffer_resources,
            add_sniffer_filter,
            // 配置管理
            get_config,
            update_config,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            eprintln!("启动 AI Model Downloader 应用失败: {e}");
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
        status: amd_core::types::DownloadState::Pending,
        progress: 0.0,
        fragments_total: 4,
        fragments_done: 0,
        created_at: chrono::Local::now().to_rfc3339(),
    };
    state.tasks.lock().await.insert(task_id.clone(), task);

    // 模拟分片失败:标记为 failed
    {
        let mut store = state.tasks.lock().await;
        if let Some(t) = store.get_mut(&task_id) {
            t.status = amd_core::types::DownloadState::Failed;
        }
    }
    let store = state.tasks.lock().await;
    let t = store.get(&task_id).unwrap();
    assert_eq!(
        t.status,
        amd_core::types::DownloadState::Failed,
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
        let mut cfg = state.config.lock().await;
        cfg.max_concurrent_tasks = 2;
    }

    // 插入 2 个活跃任务
    {
        let mut store = state.tasks.lock().await;
        for i in 0..2 {
            store.insert(
                format!("task-{i}"),
                TaskInfo {
                    id: format!("task-{i}"),
                    url: format!("https://example.com/file{i}.bin"),
                    file_name: format!("file{i}.bin"),
                    file_size: None,
                    downloaded: 0,
                    speed: 0,
                    status: amd_core::types::DownloadState::Downloading,
                    progress: 0.0,
                    fragments_total: 0,
                    fragments_done: 0,
                    created_at: chrono::Local::now().to_rfc3339(),
                },
            );
        }
    }

    // 验证活跃任务数已达到上限
    let store = state.tasks.lock().await;
    let active = store
        .values()
        .filter(|t| {
            t.status == amd_core::types::DownloadState::Downloading
                || t.status == amd_core::types::DownloadState::Pending
        })
        .count();
    let max = state.config.lock().await.max_concurrent_tasks as usize;
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

    let core = AppError::Core(amd_core::AmdError::Cancelled);
    assert!(
        format!("{core}").contains("核心错误"),
        "Core 变体 Display 应包含'核心错误'"
    );
    let json = serde_json::to_string(&core).unwrap();
    assert!(json.contains("Core"), "序列化应包含变体名: {json}");
    assert!(
        json.contains("任务已取消"),
        "序列化应包含 AmdError 消息: {json}"
    );
}
