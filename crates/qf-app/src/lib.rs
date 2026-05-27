//! QuantumFetch Tauri 应用库

pub mod commands;

use commands::{
    cancel_task, create_task, delete_task, get_app_info, get_config, get_task_detail,
    get_task_list, pause_task, resume_task, supported_protocols, update_config,
};

/// 构建并运行 Tauri 应用
pub fn run() {
    tauri::Builder::default()
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
            // 配置管理
            get_config,
            update_config,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            eprintln!("启动 QuantumFetch 应用失败: {e}");
            std::process::exit(1);
        });
}
