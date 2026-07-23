//! 下载会话
//!
//! 将 `task_commands::task_fn` 中的 11 步事务脚本封装为单一职责的会话对象。
//! 每个下载任务对应一个 [`DownloadSession`]，其 `run()` 方法负责完整生命周期：
//! URL 校验、目录准备、任务构建、断点续传注入、元数据探测、进度通道绑定、
//! 下载执行、终态处理、资源清理与快照持久化。

use std::sync::Arc;
use std::time::Duration;

use tachyon_core::config::DownloadConfig;
use tachyon_core::traits::TaskRunner;
use tachyon_engine::BufferPool;
use tachyon_engine::ConnectionPool;
use tokio::sync::{Mutex, watch};

use crate::commands::task_commands::{
    PreRunCheck, ResumeOrCancel, build_download_task, ensure_download_dir, extract_fail_reason,
    finalize_task_state, inject_resume_snapshot, mark_task_failed_and_cleanup,
    probe_and_save_metadata, should_stop_before_run, validate_and_prepare_url,
    wait_chunk_reader_done, wait_for_resume_or_cancel,
};
use crate::commands::{
    AppState, TaskCommand, cleanup_runtime, hf_race_counterpart_url, persist_task_snapshot,
    rewrite_hf_url, update_task_status,
};
use crate::runtime::ChunkReaderJob;

/// 一次完整的下载会话
pub struct DownloadSession {
    state: Arc<AppState>,
    task_id: String,
    url: String,
    download_dir: String,
    download_config: DownloadConfig,
    connection_pool: Arc<ConnectionPool>,
    buffer_pool: Arc<BufferPool>,
    control_rx: watch::Receiver<TaskCommand>,
    mirror_urls: Option<Vec<String>>,
    /// 用户在「新建任务」中显式输入的重命名(已 sanitize)。
    /// 在 `build_download_task` 之后透传给引擎,
    /// 由 `DownloadTask::probe()` 在拿到元数据后用此名覆盖,
    /// 保证磁盘文件名 = 列表显示名 = 快照路径。
    preferred_file_name: Option<String>,
}

impl DownloadSession {
    /// 创建新的下载会话
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state: Arc<AppState>,
        task_id: String,
        url: String,
        download_dir: String,
        download_config: DownloadConfig,
        connection_pool: Arc<ConnectionPool>,
        buffer_pool: Arc<BufferPool>,
        control_rx: watch::Receiver<TaskCommand>,
        mirror_urls: Option<Vec<String>>,
        preferred_file_name: Option<String>,
    ) -> Self {
        Self {
            state,
            task_id,
            url,
            download_dir,
            download_config,
            connection_pool,
            buffer_pool,
            control_rx,
            mirror_urls,
            preferred_file_name,
        }
    }

    /// 执行前检查控制信号(取消/暂停),返回 true 表示可以继续
    ///
    /// 独立函数而非方法,避免借用整个 self 导致 move 后无法调用
    async fn check_pause_or_cancel(
        state: &AppState,
        task_id: &str,
        control_rx: &mut watch::Receiver<TaskCommand>,
        pause_timeout_secs: u64,
    ) -> bool {
        match should_stop_before_run(control_rx) {
            PreRunCheck::Continue => true,
            PreRunCheck::Cancelled => {
                update_task_status(
                    &state.domain.task_repository,
                    task_id,
                    tachyon_core::types::DownloadState::Cancelled,
                );
                // cleanup_runtime 内部会调 broadcast_all,确保前端收到终态
                cleanup_runtime(state, task_id);
                false
            }
            PreRunCheck::Paused => {
                tracing::info!(task_id = %task_id, "执行前检测到暂停信号,等待恢复");
                let pause_timeout = Duration::from_secs(pause_timeout_secs);
                match wait_for_resume_or_cancel(control_rx, pause_timeout).await {
                    ResumeOrCancel::Resume => {
                        tracing::info!(task_id = %task_id, "暂停已恢复,继续");
                        true
                    }
                    ResumeOrCancel::Cancel => {
                        update_task_status(
                            &state.domain.task_repository,
                            task_id,
                            tachyon_core::types::DownloadState::Cancelled,
                        );
                        cleanup_runtime(state, task_id);
                        false
                    }
                    // 审计 M-05:执行前 pause 超时 → 保持 Paused(可后续 Resume 重启),
                    // 不再误映射为 Cancelled。cleanup 掉本代 channel,resume 走 restart 路径。
                    ResumeOrCancel::Timeout => {
                        update_task_status(
                            &state.domain.task_repository,
                            task_id,
                            tachyon_core::types::DownloadState::Paused,
                        );
                        cleanup_runtime(state, task_id);
                        false
                    }
                }
            }
        }
    }

    /// 执行完整下载生命周期
    pub async fn run(mut self) {
        // 读取 HF 源模式 + token(配置驱动)。锁失败降级为默认 Mirror。
        let (hf_mode, hub_token) = {
            let cfg = self.state.domain.config.lock().await;
            (cfg.hub.source_mode, cfg.hub.token.clone())
        };
        // 官方 resolve URL 下载注入 Bearer;镜像 URL 由 HttpClient host 门禁不发送
        if self.url.contains("huggingface.co") || self.url.contains("hf-mirror.com") {
            self.download_config.auth_bearer = hub_token;
        }

        // 按模式改写 URL:Official 不改写,Mirror/Race 替换为 hf-mirror.com
        // (Race 浏览与主源均走镜像保证国内可达,官方作为竞速源在下方注入)
        self.url = rewrite_hf_url(&self.url, hf_mode);

        // Race 模式:注入对立源做竞速(主源镜像则注入官方,主源官方则注入镜像)
        if matches!(hf_mode, tachyon_core::config::HfSourceMode::Race)
            && let Some(counterpart) = hf_race_counterpart_url(&self.url)
        {
            let mut urls = self.mirror_urls.take().unwrap_or_default();
            if !urls.iter().any(|u| u == &counterpart) {
                urls.push(counterpart);
            }
            tracing::info!(task_id = %self.task_id, mirrors = urls.len(), "Race 模式注入 HF 竞速源");
            self.mirror_urls = Some(urls);
        }

        // 1. URL 校验与启动前状态守卫
        let pause_timeout_secs = self.download_config.pause_timeout_secs;

        // 1+2. 并行: URL 校验 + 目录准备
        let (host_result, dir_result) = tokio::join!(
            validate_and_prepare_url(
                &self.url,
                &self.state,
                &self.task_id,
                &mut self.control_rx,
                pause_timeout_secs,
            ),
            ensure_download_dir(&self.download_dir, &self.state, &self.task_id),
        );

        let host = match host_result {
            Some(h) => h,
            None => return,
        };
        tracing::info!(
            task_id = %self.task_id,
            host = %host,
            download_dir = %self.download_dir,
            "开始真实下载"
        );

        if dir_result.is_err() {
            return;
        }

        // 构建前检查取消/暂停信号
        if !Self::check_pause_or_cancel(
            &self.state,
            &self.task_id,
            &mut self.control_rx,
            pause_timeout_secs,
        )
        .await
        {
            return;
        }

        // 3. 构造下载任务
        let scheduler_config = {
            let cfg = self.state.domain.config.lock().await;
            cfg.scheduler.clone()
        };
        let mut download_task: Box<dyn TaskRunner> = match build_download_task(
            &self.task_id,
            &self.url,
            self.download_config,
            self.connection_pool,
            self.buffer_pool.clone(),
            self.state.infra.global_rate_limiter.clone(),
            scheduler_config,
            self.mirror_urls,
            #[cfg(feature = "magnet")]
            self.state.infra.bt_session.lock().await.clone(),
        )
        .await
        {
            Ok(t) => t,
            Err(()) => {
                mark_task_failed_and_cleanup(&self.state, &self.task_id).await;
                return;
            }
        };

        // 构建后再检查(构建是异步操作,期间可能收到暂停信号)
        if !Self::check_pause_or_cancel(
            &self.state,
            &self.task_id,
            &mut self.control_rx,
            pause_timeout_secs,
        )
        .await
        {
            return;
        }

        // 4. 注入控制通道
        download_task.set_control_rx(self.control_rx.clone());

        // 4.1 注入用户重命名(若有):必须在 probe() 之前设置,
        // probe() 拿到协议侧元数据后会以此名覆盖 metadata.file_name,
        // 使下游 init_storage / 快照 / UI 全部读到统一文件名。
        if let Some(name) = self.preferred_file_name.take() {
            download_task.set_preferred_file_name(name);
        }

        // 5. 断点续传快照注入
        inject_resume_snapshot(&mut download_task, &self.state, &self.task_id).await;

        if !Self::check_pause_or_cancel(
            &self.state,
            &self.task_id,
            &mut self.control_rx,
            pause_timeout_secs,
        )
        .await
        {
            return;
        }

        // 6. 探测远程文件元数据并持久化初始快照(支持探测期间暂停)
        let _metadata = match probe_and_save_metadata(
            &mut download_task,
            &self.state,
            &self.task_id,
            &self.download_dir,
            &mut self.control_rx,
            pause_timeout_secs,
        )
        .await
        {
            Some(m) => m,
            None => return,
        };

        // 探测完成后、执行前检查暂停信号
        if !Self::check_pause_or_cancel(
            &self.state,
            &self.task_id,
            &mut self.control_rx,
            pause_timeout_secs,
        )
        .await
        {
            return;
        }

        // 7. 设置进度通道并提交到共享 ChunkReaderPool
        let (chunk_progress_tx, chunk_progress_rx) =
            tokio::sync::mpsc::channel::<tachyon_core::FragmentProgress>(4096);
        // 容量 4096:多分片 + 每 5 chunk 上报时 256 极易 Full,
        // 增量 try_send 可丢但会刷屏 WARN;完成事件用 send().await 仍可靠
        download_task.set_progress_sender(chunk_progress_tx);

        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let broker = self.state.runtime.progress_broker.clone();
        let on_progress: crate::runtime::chunk_reader_pool::ProgressCallback =
            Arc::new(move |task_id, delta, bytes| {
                let fb: Vec<crate::commands::FragmentByteProgress> = bytes
                    .iter()
                    .map(|e| crate::commands::FragmentByteProgress {
                        index: e.index,
                        downloaded: e.downloaded,
                    })
                    .collect();
                broker.mark_dirty_with_delta(task_id, delta, fb);
            });
        let job = ChunkReaderJob {
            task_id: self.task_id.clone(),
            progress_rx: chunk_progress_rx,
            task_repository: self.state.domain.task_repository.clone(),
            task_store: self.state.infra.task_store.clone(),
            done_tx,
            on_progress: Some(on_progress),
            fragment_state_store: self.state.fragment_state_store.clone(),
        };
        if let Err(e) = self.state.infra.chunk_reader_pool.submit_async(job).await {
            tracing::error!(task_id = %self.task_id, error = %e, "提交 chunk reader job 失败");
            mark_task_failed_and_cleanup(&self.state, &self.task_id).await;
            return;
        }

        // 8. 执行下载
        let download_task = Arc::new(Mutex::new(download_task));
        let (result, final_file_size) = {
            let mut dt = download_task.lock().await;
            let result = dt.run().await;
            let final_file_size = dt.metadata().and_then(|m| m.file_size);
            (result, final_file_size)
        };

        // 9. 处理下载结果
        finalize_task_state(&self.state, &self.task_id, result.as_ref(), final_file_size).await;

        // 10. 等待 chunk reader 完成
        let _ = wait_chunk_reader_done(done_rx, &self.task_id).await;

        // cleanup_runtime 内部调 broadcast_all,确保前端收到终态 status
        // 以触发 clearTaskFragmentDownloading 清理残留 downloading 格子
        cleanup_runtime(&self.state, &self.task_id);

        // 11. 持久化最终快照
        let fail_reason = extract_fail_reason(&self.state, &self.task_id, result.as_ref());
        persist_task_snapshot(&self.state, &self.task_id, fail_reason).await;
    }
}
