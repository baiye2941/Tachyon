//! 剪贴板监听服务
//!
//! 后台轮询系统剪贴板,检测到可下载 URL(http/https/magnet)时
//! 向前端推送 `clipboard://url-detected` 事件,前端弹 Toast 让用户确认下载。
//!
//! 设计要点:
//! - `clipboard-manager` 插件只支持 `read_text()`,无事件监听,只能轮询
//! - `read_text()` 不可在主线程调用(Linux 可能死锁),必须在 tokio spawn 的异步任务中调用
//! - 去重:与上次 `read_text()` 结果比对,相同内容不重复提示
//! - 过滤:复用 sniffer 的 `should_capture` + `CaptureConfig`(类型白名单 + URL 过滤器)
//! - 校验:复用 `validate_download_url`(http/https/magnet 合法性 + SSRF 防护)
//! - 默认关闭,用户需在设置中开启 `clipboard.enable_watch`

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tachyon_core::config::AppConfig;
use tachyon_sniffer::capture::{identify_resource, should_capture};
use tauri::{AppHandle, Emitter};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tokio::sync::Mutex;
use tracing::debug;

use crate::commands::resource_type_to_string;
use crate::commands::validate_download_url;
use crate::service::SnifferService;

/// 审计 A-14:轮询 start 幂等 CAS;true=首次占用。
fn claim_start_slot(flag: &AtomicBool) -> bool {
    flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// 剪贴板 URL 检测事件 payload
///
/// 序列化为 camelCase 供前端 IPC 消费。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardUrlDetected {
    /// 检测到的 URL
    pub url: String,
    /// 资源类型(video/audio/document/archive/...)
    pub resource_type: String,
}

/// 剪贴板监听器
///
/// 在 Tauri setup 中调用 `start()` 启动后台轮询任务。
/// 持有 `AppHandle` 用于读剪贴板和 emit 事件,
/// 持有 `AppConfig` 的引用用于读取 `clipboard.enable_watch` 开关,
/// 持有 `SnifferService` 用于读取 `CaptureConfig`(类型白名单 + URL 过滤器)。
pub struct ClipboardWatcher {
    app_handle: AppHandle,
    config: Arc<Mutex<AppConfig>>,
    sniffer_service: Arc<SnifferService>,
    /// 上次读取的剪贴板内容,用于去重
    last_text: Arc<Mutex<Option<String>>>,
    /// 审计 A-14:start 幂等,避免双 spawn 轮询
    loop_started: AtomicBool,
}

impl ClipboardWatcher {
    /// 创建新的剪贴板监听器(不启动轮询)
    pub fn new(
        app_handle: AppHandle,
        config: Arc<Mutex<AppConfig>>,
        sniffer_service: Arc<SnifferService>,
    ) -> Self {
        Self {
            app_handle,
            config,
            sniffer_service,
            last_text: Arc::new(Mutex::new(None)),
            loop_started: AtomicBool::new(false),
        }
    }

    /// 是否已 spawn 轮询循环(测试/观测)
    pub fn is_loop_started(&self) -> bool {
        self.loop_started.load(Ordering::Acquire)
    }

    /// 尝试占用 start 槽位(幂等 CAS)。true=首次,false=已启动。
    fn claim_loop_start(&self) -> bool {
        claim_start_slot(&self.loop_started)
    }

    /// 启动后台剪贴板轮询任务(幂等)
    ///
    /// 必须在 Tokio reactor 上下文中调用(如 Tauri `setup` 钩子的 `block_on` 内)。
    /// 审计 A-14:即使 `enable_watch` 初始为 false 也 spawn 循环;
    /// 循环内按配置门禁,`update_config` 将 false→true 无需重启即可生效。
    /// 轮询间隔取启动时 `poll_interval_ms`(默认 1000ms,最小 100);
    /// 间隔热改仍为诚实非目标。
    pub async fn start(&self) {
        if !self.claim_loop_start() {
            debug!("剪贴板轮询已启动,跳过重复 start");
            return;
        }

        let interval_ms = {
            let cfg = self.config.lock().await;
            cfg.clipboard.poll_interval_ms.max(100)
        };

        let handle = self.app_handle.clone();
        let config = self.config.clone();
        let sniffer = self.sniffer_service.clone();
        let last_text = self.last_text.clone();

        crate::runtime::panic_isolation::spawn_isolated("clipboard_watcher", async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(interval_ms));
            // 首次 tick 不读(避免启动时把已有内容当新检测)
            interval.tick().await;

            loop {
                interval.tick().await;

                // 读取开关状态(可能被 update_config 动态修改)
                let enabled = config.lock().await.clipboard.enable_watch;
                if !enabled {
                    continue;
                }

                // 读取剪贴板(read_text 不可在主线程,此处在 tokio worker 线程)
                let text = match handle.clipboard().read_text() {
                    Ok(t) => t,
                    Err(e) => {
                        debug!(error = %e, "读取剪贴板失败(可能被其他进程占用)");
                        continue;
                    }
                };

                // 去重:与上次内容相同则跳过
                let mut last = last_text.lock().await;
                if last.as_deref() == Some(text.as_str()) {
                    continue;
                }
                *last = Some(text.clone());
                drop(last);

                // 评估:校验 + 识别 + 过滤
                let capture_config = sniffer.capture_config().await;
                if let Some(detected) = evaluate_clipboard_text(&text, &capture_config) {
                    debug!(url = %tachyon_core::redact_url_for_log(&detected.url), r#type = %detected.resource_type, "剪贴板检测到可下载 URL");
                    let _ = handle.emit("clipboard://url-detected", &detected);
                }
            }
        });

        debug!(interval_ms, "剪贴板轮询循环已启动(enable_watch 运行时门禁)");
    }
}

/// 评估剪贴板文本是否为可下载 URL
///
/// 纯函数,可独立测试。逻辑:
/// 1. `validate_download_url` 校验合法性(http/https/magnet + SSRF 防护)
/// 2. `identify_resource` 识别资源类型
/// 3. `should_capture` 过滤(类型白名单 + URL 过滤器)
///
/// 返回 `Some(ClipboardUrlDetected)` 表示应提示用户,`None` 表示忽略。
pub fn evaluate_clipboard_text(
    text: &str,
    config: &tachyon_sniffer::capture::CaptureConfig,
) -> Option<ClipboardUrlDetected> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    // 校验 URL 合法性(复用现有校验逻辑)
    if validate_download_url(trimmed).is_err() {
        return None;
    }

    // 资源类型识别
    let resource_type = identify_resource(trimmed);

    // 类型白名单 + URL 过滤器过滤
    if !should_capture(trimmed, config) {
        return None;
    }

    Some(ClipboardUrlDetected {
        url: trimmed.to_string(),
        resource_type: resource_type_to_string(resource_type).to_string(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tachyon_sniffer::capture::{CaptureConfig, ResourceType};

    fn default_config() -> CaptureConfig {
        CaptureConfig::default()
    }

    #[test]
    fn test_evaluate_http_video_url() {
        let cfg = default_config();
        let result = evaluate_clipboard_text("https://cdn.example.com/movie.mp4", &cfg);
        assert!(result.is_some());
        let detected = result.unwrap();
        assert_eq!(detected.url, "https://cdn.example.com/movie.mp4");
        assert_eq!(detected.resource_type, "video");
    }

    #[test]
    fn test_evaluate_magnet_link() {
        let cfg = default_config();
        let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=test";
        let result = evaluate_clipboard_text(magnet, &cfg);
        // magnet 被识别为 Other,默认 enabled_types 不含 Other,故被过滤
        // 但 magnet 是合法下载链接,should_capture 会拦截 Other 类型
        // 这个测试验证 magnet 被 validate 接受但被 should_capture 过滤的行为
        // (magnet 在实际产品中应特殊处理,当前走 Other 被过滤是预期行为)
        assert!(
            result.is_none(),
            "magnet 默认被类型白名单过滤(Other 不在白名单)"
        );
    }

    #[test]
    fn test_evaluate_empty_text() {
        let cfg = default_config();
        assert!(evaluate_clipboard_text("", &cfg).is_none());
        assert!(evaluate_clipboard_text("   ", &cfg).is_none());
    }

    #[test]
    fn test_evaluate_non_url_text() {
        let cfg = default_config();
        assert!(evaluate_clipboard_text("hello world", &cfg).is_none());
        assert!(evaluate_clipboard_text("这是一段普通文本", &cfg).is_none());
    }

    #[test]
    fn test_evaluate_unsupported_protocol() {
        let cfg = default_config();
        assert!(evaluate_clipboard_text("ftp://example.com/file.zip", &cfg).is_none());
        assert!(evaluate_clipboard_text("ed2k://|file|test", &cfg).is_none());
    }

    #[test]
    fn test_evaluate_disabled_type_filtered() {
        let mut enabled = HashSet::new();
        enabled.insert(ResourceType::Archive);
        let cfg = CaptureConfig {
            enabled_types: enabled,
            min_size: 0,
            url_filters: vec![],
        };
        // video 被禁用
        assert!(evaluate_clipboard_text("https://example.com/movie.mp4", &cfg).is_none());
        // archive 仍可检测
        assert!(evaluate_clipboard_text("https://example.com/file.zip", &cfg).is_some());
    }

    #[test]
    fn test_evaluate_trims_whitespace() {
        let cfg = default_config();
        let result = evaluate_clipboard_text("  https://example.com/file.zip\n", &cfg);
        assert!(result.is_some());
        assert_eq!(result.unwrap().url, "https://example.com/file.zip");
    }

    #[test]
    fn test_evaluate_url_with_query_params() {
        let cfg = default_config();
        let result = evaluate_clipboard_text(
            "https://cdn.example.com/video.mp4?token=abc&quality=1080p",
            &cfg,
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().resource_type, "video");
    }

    /// 审计 A-14:start 槽位 CAS 幂等
    #[test]
    fn test_a14_claim_start_slot_is_idempotent() {
        let flag = AtomicBool::new(false);
        assert!(claim_start_slot(&flag), "首次 claim 应成功");
        assert!(!claim_start_slot(&flag), "二次 claim 应失败");
        assert!(flag.load(Ordering::Acquire));
    }
}
