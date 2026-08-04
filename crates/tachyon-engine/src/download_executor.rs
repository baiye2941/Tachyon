//! 并发分片下载执行器
//!
//! 从 downloader.rs 拆分:execute / execute_full_download /
//! execute_fragmented_download / spawn_fragment_task / download_single_fragment /
//! try_rebalance_slowest_fragment / write helpers / goodput tracking。
//!
//! 核心下载循环(download_single_fragment)使用 tokio::select! + watch_for_interrupt
//! 竞速实现取消安全,流错误前刷 write_buf 实现字节级续传。

use super::*;

impl DownloadTask {
    // ----- 步骤 4: 并发执行下载 -----

    /// 执行全部分片下载
    ///
    /// 根据配置的最大并发数使用信号量控制并发,每个分片独立下载并写入存储。
    /// 不支持 Range 请求时退化为整块下载。
    #[tracing::instrument(skip(self), fields(task_id = %self.id))]
    pub async fn execute(&mut self) -> DownloadResult<()> {
        self.state = DownloadState::Downloading;
        debug!("开始执行下载任务");

        let metadata = self
            .metadata
            .as_ref()
            .ok_or_else(|| DownloadError::Config("必须先调用 probe()".into()))?;

        let supports_range = metadata.supports_range;
        let file_size = metadata.file_size;

        // 空文件无需下载
        if file_size == Some(0) {
            self.state = DownloadState::Completed;
            info!("文件大小为 0,跳过下载");
            return Ok(());
        }

        // 不支持 Range:整块下载
        if !supports_range || self.fragments.len() <= 1 {
            return self.execute_full_download().await;
        }

        // 支持 Range:并发分片下载
        self.execute_fragmented_download().await
    }

    /// 整块下载(不支持 Range 或单分片)
    ///
    /// 以流式方式逐块写入存储,峰值内存仅含单个 chunk,避免大文件整块进内存。
    ///
    /// 审计 HTTP-09:与分片路径同构,可重试错误按 `max_retries` 退避重试;
    /// 每次 attempt 从 offset 0 重写,并用 `allocate` 重置存储长度,避免半写污染。
    async fn execute_full_download(&mut self) -> DownloadResult<()> {
        let pause_timeout = Duration::from_secs(self.config.pause_timeout_secs);
        let max_retries = self.config.max_retries;
        let mut attempt = 0u32;
        loop {
            match self.execute_full_download_once(pause_timeout).await {
                Ok(()) => {
                    // 整块成功同样解除软压力冷却(与分片成功对称)
                    Self::clear_soft_pressure_cooldown_on_success(&self.soft_pressure_until);
                    break;
                }
                Err(e) => {
                    // 用户暂停:等 Resume 后重试本 attempt,不计入 max_retries
                    if matches!(e, DownloadError::Paused) {
                        Self::wait_control(&mut self.control_rx, pause_timeout).await?;
                        continue;
                    }
                    // 暂停超时是控制语义,不是瞬态网络故障;禁止纳入 max_retries 退避
                    // (否则 1s 暂停超时 × 默认 3 次重试会远超调用方等待窗口)。
                    if e.is_retryable()
                        && !Self::is_pause_timeout_error(&e)
                        && attempt < max_retries
                    {
                        let next_attempt = attempt + 1;
                        let backoff = match &e {
                            DownloadError::Throttled {
                                retry_after_secs: Some(secs),
                            } => Duration::from_secs((*secs).min(1024)),
                            _ => {
                                let base = Duration::from_secs((1u64 << attempt.min(10)).max(1));
                                if Self::is_connection_soft_pressure(&e) {
                                    Self::soft_pressure_backoff_secs(attempt, base)
                                } else {
                                    base
                                }
                            }
                        };
                        // 整块路径无 concurrency_ctrl,但仍延长全局冷却,避免随后分片路径立刻抬升
                        Self::extend_soft_pressure_cooldown(
                            &self.soft_pressure_until,
                            Duration::from_secs(30),
                        );
                        warn!(
                            attempt = next_attempt,
                            max_retries,
                            ?backoff,
                            error = %e,
                            "整块下载可重试失败,退避后重试"
                        );
                        // 整块路径 fragment_index=0,与任务级 retry_count 聚合对齐
                        if let Some(tx) = &self.progress_tx {
                            let _ = tx.try_send(FragmentProgress::Retry {
                                fragment_index: 0,
                                attempt: next_attempt,
                            });
                        }
                        // 重置存储,防止半写残留污染下次 attempt
                        if let Some(storage) = self.storage.as_ref() {
                            let size = self
                                .metadata
                                .as_ref()
                                .and_then(|m| m.file_size)
                                .unwrap_or(0);
                            let _ = storage.allocate(size).await;
                        }
                        self.protocol.clear_selected().await;
                        tokio::time::sleep(backoff).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        // 审计 BT-17:单分片 BT 文件走 full-stream 路径时,FileStream 读完 ≠ piece
        // truth 完成。标 Completed 前同样需要等待 librqbit wait_until_completed。
        #[cfg(feature = "magnet")]
        self.wait_bt_piece_truth_if_protocol_managed().await?;

        Ok(())
    }

    /// 审计 BT-17:protocol_managed_storage 时等待 librqbit piece truth 完成。
    ///
    /// 单分片与多分片 BT 路径共用。仅在 `protocol_managed_storage` 且持有
    /// `bt_magnet`/`bt_fallback` 时等待,否则空操作。
    #[cfg(feature = "magnet")]
    async fn wait_bt_piece_truth_if_protocol_managed(&self) -> DownloadResult<()> {
        if self
            .metadata
            .as_ref()
            .is_some_and(|m| m.protocol_managed_storage)
            && let Some(magnet) = self.bt_magnet.as_ref().or(self.bt_fallback.as_ref())
        {
            info!("BT protocol_managed:等待 piece truth 完成(BT-17)");
            magnet.wait_torrent_completed(&self.url).await?;
        }
        Ok(())
    }

    /// 控制通道「暂停超过 N 秒」超时(非网络 Timeout)
    fn is_pause_timeout_error(err: &DownloadError) -> bool {
        matches!(err, DownloadError::Timeout(msg) if msg.starts_with("暂停超过"))
    }

    /// 对端/中间盒掐连接、TLS 异常 EOF、网关 502/504 等“软压力”信号。
    ///
    /// 这类错误可重试,但继续高并发往往会加剧掐断/网关过载;应在中间重试时
    /// 下调目标并发并拉长退避,让存活连接完成,而不是立刻熔断整源。
    pub(crate) fn is_connection_soft_pressure(err: &DownloadError) -> bool {
        match err {
            // 网关/限流/超时:继续高并发只会加重失败
            // 403:部分 CDN/WAF 对突发多连接直接拒绝,降并发后重试常可恢复
            DownloadError::Http { status, .. } => {
                matches!(*status, 403 | 408 | 429 | 502 | 503 | 504)
            }
            DownloadError::Throttled { .. } => true,
            DownloadError::Timeout(_) => true,
            DownloadError::Forbidden { .. } => true,
            DownloadError::Network(msg) | DownloadError::Protocol(msg) => {
                let s = msg.to_ascii_lowercase();
                s.contains("tls close_notify")
                    || s.contains("unexpected eof")
                    // reqwest/rustls: "tls handshake eof" / "handshake eof" 无 close_notify 字样
                    || s.contains("tls handshake eof")
                    || s.contains("handshake eof")
                    || s.contains("connection reset")
                    || s.contains("broken pipe")
                    || s.contains("connection closed")
                    || s.contains("error reading a body from connection")
                    || s.contains("decoding response body")
                    || s.contains("client error (connect)")
                    || s.contains("gateway timeout")
                    || s.contains("bad gateway")
                    || s.contains("service unavailable")
                    || s.contains("too many requests")
                    || s.contains("forbidden")
            }
            _ => {
                let s = err.to_string().to_ascii_lowercase();
                s.contains("tls close_notify")
                    || s.contains("unexpected eof")
                    || s.contains("tls handshake eof")
                    || s.contains("handshake eof")
                    || s.contains("connection reset")
                    || s.contains("decoding response body")
                    || s.contains("client error (connect)")
                    || s.contains("403")
                    || s.contains("429")
            }
        }
    }

    /// 软压力时下调目标并发,并延长全局冷却截止时间。
    ///
    /// - `mild=false`(零进度): target 减半,冷却 15s
    /// - `mild=true`(已有落盘进度): **不降 target**,仅冷却 5s 挡住 scale-up。
    ///   中途 TLS EOF + partial 多半是代理/对端掐长连接,不是“并发过高”。
    ///   再砍并发只会把 2 路健康会话串行化(实测 c=1 ≈ 一半吞吐,aria2 无此自伤)。
    ///
    /// 冷却期内不滑动续期、不连砍。
    pub(crate) fn apply_soft_pressure_backoff_ex(
        ctrl: &ConcurrencyController,
        err: &DownloadError,
        mild: bool,
        soft_pressure_until: &std::sync::atomic::AtomicU64,
    ) {
        if !Self::is_connection_soft_pressure(err) {
            return;
        }
        if Self::soft_pressure_blocks_scale_up(soft_pressure_until) {
            return;
        }
        let cool = if mild {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(15)
        };
        Self::extend_soft_pressure_cooldown(soft_pressure_until, cool);
        if mild {
            // 有进度:只挡抬升,保持当前并发让其它存活片继续吐数据
            return;
        }
        let old = ctrl.target();
        // 零进度:减半,但下限 2(若当前已是多连接)。
        // 代理下 c=2 是健康稳态;单片 handshake eof 不该把整任务串行化到 1。
        let floor = if old >= 2 { 2 } else { 1 };
        let new_target = (old / 2).max(floor);
        if new_target < old {
            ctrl.set_target(new_target);
            warn!(
                old_concurrency = old,
                new_concurrency = new_target,
                mild = false,
                error = %err,
                "检测到连接软压力,降低目标并发"
            );
        }
    }

    pub(crate) fn soft_pressure_epoch() -> std::time::Instant {
        use std::sync::LazyLock;
        static EPOCH: LazyLock<std::time::Instant> = LazyLock::new(std::time::Instant::now);
        *EPOCH
    }

    /// 进程全局重连时间线(epoch 毫秒):片间错开仍跨任务,减轻代理 TLS 风暴。
    /// 冷却截止 soft_pressure_until 已改为 per-task,避免多任务互串。
    pub(crate) fn soft_reconnect_last_ms() -> &'static std::sync::atomic::AtomicU64 {
        use std::sync::LazyLock;
        use std::sync::atomic::AtomicU64;
        static LAST: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));
        &LAST
    }

    pub(crate) fn soft_pressure_now_ms() -> u64 {
        std::time::Instant::now()
            .checked_duration_since(Self::soft_pressure_epoch())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// soft-pressure 重连最小片间间隔(Full Jitter 后仍可能撞车)。
    /// 返回额外需要 sleep 的时长;调用方应在退避后再等这段。
    /// 注意:时间线仍进程全局——多任务交错重连是有意的。
    pub(crate) fn soft_reconnect_spacing_delay(min_gap_ms: u64) -> Duration {
        let now = Self::soft_pressure_now_ms();
        let gap = min_gap_ms.max(1);
        loop {
            let last = Self::soft_reconnect_last_ms().load(std::sync::atomic::Ordering::Acquire);
            let earliest = last.saturating_add(gap);
            if now >= earliest {
                if Self::soft_reconnect_last_ms()
                    .compare_exchange(
                        last,
                        now,
                        std::sync::atomic::Ordering::AcqRel,
                        std::sync::atomic::Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return Duration::ZERO;
                }
                continue;
            }
            if Self::soft_reconnect_last_ms()
                .compare_exchange(
                    last,
                    earliest,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .is_ok()
            {
                return Duration::from_millis(earliest.saturating_sub(now));
            }
        }
    }

    pub(crate) fn extend_soft_pressure_cooldown(
        until: &std::sync::atomic::AtomicU64,
        extra: Duration,
    ) {
        let now = std::time::Instant::now()
            .checked_duration_since(Self::soft_pressure_epoch())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let new_until = now.saturating_add(extra.as_secs().max(1));
        let _ = until.fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |cur| Some(cur.max(new_until)),
        );
    }

    pub(crate) fn soft_pressure_blocks_scale_up(until: &std::sync::atomic::AtomicU64) -> bool {
        let now = std::time::Instant::now()
            .checked_duration_since(Self::soft_pressure_epoch())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now < until.load(std::sync::atomic::Ordering::Acquire)
    }

    /// 分片成功时**半衰**本任务软压力冷却,而非瞬间清零。
    pub(crate) fn clear_soft_pressure_cooldown_on_success(until: &std::sync::atomic::AtomicU64) {
        let now = std::time::Instant::now()
            .checked_duration_since(Self::soft_pressure_epoch())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = until.fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |u| {
                if u <= now {
                    Some(0)
                } else {
                    let remain = u.saturating_sub(now);
                    let half = remain.div_ceil(2).max(1);
                    Some(now.saturating_add(half))
                }
            },
        );
    }

    /// 并发抬升步进限制。
    ///
    /// - 直连:`conservative=false` → 每次最多翻倍(至少 +1)
    /// - 代理:`conservative=true` → 每次最多 +1,避免 2→4 一步打满
    ///
    /// 降并发不受限。
    pub(crate) fn clamp_concurrency_scale_up(old: u32, new: u32) -> u32 {
        Self::clamp_concurrency_scale_up_ex(old, new, false)
    }

    pub(crate) fn clamp_concurrency_scale_up_ex(old: u32, new: u32, conservative: bool) -> u32 {
        if new <= old {
            return new.max(1);
        }
        let step_cap = if conservative {
            old.saturating_add(1).max(1)
        } else {
            old.saturating_mul(2).max(old.saturating_add(1)).max(1)
        };
        new.min(step_cap).max(1)
    }

    #[cfg(test)]
    pub(crate) fn fresh_soft_until() -> Arc<std::sync::atomic::AtomicU64> {
        Arc::new(std::sync::atomic::AtomicU64::new(0))
    }

    /// 软压力退避:在基础 jitter 之上至少 2s,并随 attempt 指数放大(上限 60s)。
    pub(crate) fn soft_pressure_backoff_secs(attempt: u32, base: Duration) -> Duration {
        let min_secs = 2u64.saturating_mul(1u64 << attempt.min(4)).min(60);
        let base_secs = base.as_secs().max(1);
        Duration::from_secs(base_secs.max(min_secs))
    }

    /// 单次整块流式下载 attempt(无重试)
    async fn execute_full_download_once(&mut self, pause_timeout: Duration) -> DownloadResult<()> {
        Self::wait_control(&mut self.control_rx, pause_timeout).await?;
        self.refresh_resolved_host_from_protocol();
        let host = self.request_host()?;
        // P1:镜像路径跳过主 host 的 pool.acquire,改由 MirrorProtocol
        // (已注入同一 pool)按真实命中镜像 host acquire,使各镜像能各自
        // 占满自己的 per-host 配额。单源路径保持 engine 层 acquire 不变。
        let _pool_permit = match (&self.pool, self.has_mirrors) {
            (Some(pool), false) => Some(pool.acquire(&host).await?),
            _ => None,
        };
        let start_instant = std::time::Instant::now();

        // 优先使用外部共享限速器(跨任务全局限速),否则从配置创建 per-task 限速器
        let rate_limiter: Option<Arc<RateLimiter>> = self.rate_limiter.clone().or_else(|| {
            self.config
                .rate_limit_bytes_per_sec
                .filter(|&bps| bps > 0)
                .map(|bps| Arc::new(RateLimiter::new(bps)))
        });

        // 获取流式响应(控制信号可在建立连接阶段中断)
        let stream = if let Some(rx) = self.control_rx.as_mut() {
            tokio::select! {
                result = self.protocol.download_full_stream(&self.url) => result?,
                control = Self::watch_for_interrupt(rx, pause_timeout) => {
                    control?;
                    return Err(DownloadError::Other("控制信号异常结束".into()));
                }
            }
        } else {
            self.protocol.download_full_stream(&self.url).await?
        };

        // clone Arc 后释放 self 的不可变借用,便于循环内 note_goodput_bytes(&mut self)
        let storage = self
            .storage
            .clone()
            .ok_or_else(|| DownloadError::Config("存储未初始化".into()))?;
        let expected_size = self.metadata.as_ref().and_then(|md| md.file_size);

        // 与分片路径一致:用 512 对齐 AlignedBuf 聚合小 chunk,再 write_all_at。
        // 避免 reqwest 未对齐 Bytes 每个 chunk 都 ensure_aligned 临时分配。
        let mut write_buf = if let Some(ref pool) = self.buffer_pool {
            WriteBuf::Guard(pool.alloc_guarded().await)
        } else {
            WriteBuf::Owned(
                AlignedBuf::new(WRITE_BATCH_BYTES).expect("AlignedBuf 分配失败(内存不足)"),
            )
        };
        write_buf.as_mut().clear();

        // 逐块消费并写入,顺序追加偏移
        let mut pos: u64 = 0;
        // 与分片路径同一节流模式:每 PROGRESS_REPORT_CHUNK_INTERVAL 个 chunk
        // 上报一次增量,避免高频上报放大下游 checkpoint(fsync)开销
        let mut progress_report_countdown = PROGRESS_REPORT_CHUNK_INTERVAL;
        tokio::pin!(stream);
        // B11:改裸 `while let stream.next().await` 为 `loop { select!{...} }`,
        // 使取消信号能在"无 chunk 到达"时(如死连接静默挂起)穿透到检查点。
        loop {
            let chunk_result = if let Some(rx) = self.control_rx.as_mut() {
                tokio::select! {
                    biased;
                    interrupt = Self::watch_for_interrupt(rx, pause_timeout) => {
                        interrupt?;
                        return Err(DownloadError::Other("控制信号异常结束".into()));
                    }
                    chunk = tokio_stream::StreamExt::next(&mut stream) => match chunk {
                        Some(r) => r,
                        None => break, // EOF:正常退出循环
                    },
                }
            } else {
                match tokio_stream::StreamExt::next(&mut stream).await {
                    Some(r) => r,
                    None => break,
                }
            };
            // chunk 间隙:Pause 立即停,不挂起等 Resume
            Self::check_control_interrupt(&mut self.control_rx)?;
            let chunk = chunk_result?;
            let chunk_len = u64::try_from(chunk.len())
                .map_err(|_| DownloadError::Config("整块下载 chunk 长度溢出".into()))?;
            let attempted = pos
                .checked_add(write_buf.as_mut().len() as u64)
                .and_then(|p| p.checked_add(chunk_len))
                .ok_or_else(|| {
                    DownloadError::Config(format!(
                        "整块下载长度溢出: written={pos}, buffered={}, chunk={chunk_len}",
                        write_buf.as_mut().len()
                    ))
                })?;
            // 审计 HTTP-15:已知长度也必须写前拒绝越界,避免先扩文件后才报错
            if let Some(expected) = expected_size {
                if attempted > expected {
                    return Err(DownloadError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "整块下载响应超过声明长度: expected={expected}, 将写入到 {attempted}"
                        ),
                    )));
                }
            } else if attempted > self.config.max_full_stream_bytes {
                return Err(DownloadError::Config(format!(
                    "未知大小整块下载超过上限: 上限 {} 字节, 本次将写入 {} 字节",
                    self.config.max_full_stream_bytes, attempted
                )));
            }

            // 大 chunk:先冲刷缓冲;已对齐则直写,未对齐则切块装入 write_buf(复用对齐内存,避免 ensure_aligned 每块新分配)
            if chunk.len() >= WRITE_BATCH_BYTES {
                if !write_buf.as_mut().is_empty() {
                    let batch = write_buf.as_mut().split().freeze();
                    let written = Self::write_all_at(
                        storage.as_ref(),
                        pos,
                        batch,
                        &mut self.control_rx,
                        pause_timeout,
                        self.metrics.as_deref(),
                    )
                    .await?;
                    pos += written;
                    if let Some(ref limiter) = rate_limiter {
                        limiter.acquire(written).await;
                    }
                    if let Some(bps) = self.note_goodput_bytes(written) {
                        self.scheduler.observe_bandwidth(bps);
                    }
                }
                let ptr_aligned = (chunk.as_ptr() as usize).is_multiple_of(512);
                if ptr_aligned {
                    let written = Self::write_all_at(
                        storage.as_ref(),
                        pos,
                        chunk,
                        &mut self.control_rx,
                        pause_timeout,
                        self.metrics.as_deref(),
                    )
                    .await?;
                    if written != chunk_len {
                        return Err(DownloadError::Fragment(format!(
                            "整块下载短写未完成: offset={pos}, expected={chunk_len}, written={written}"
                        )));
                    }
                    pos += written;
                    if let Some(ref limiter) = rate_limiter {
                        limiter.acquire(written).await;
                    }
                    if let Some(bps) = self.note_goodput_bytes(written) {
                        self.scheduler.observe_bandwidth(bps);
                    }
                } else {
                    // 未对齐大块:按 write_buf 剩余容量切片装入,满批刷写(freeze 后指针 512 对齐 → passthrough)
                    let mut rest = chunk;
                    while !rest.is_empty() {
                        let space = WRITE_BATCH_BYTES.saturating_sub(write_buf.as_mut().len());
                        let take = rest.len().min(space.max(1));
                        let piece = rest.slice(..take);
                        rest = rest.slice(take..);
                        write_buf.as_mut().extend_from_slice(&piece);
                        if write_buf.as_mut().len() >= WRITE_BATCH_BYTES {
                            let batch = write_buf.as_mut().split().freeze();
                            let written = Self::write_all_at(
                                storage.as_ref(),
                                pos,
                                batch,
                                &mut self.control_rx,
                                pause_timeout,
                                self.metrics.as_deref(),
                            )
                            .await?;
                            pos += written;
                            if let Some(ref limiter) = rate_limiter {
                                limiter.acquire(written).await;
                            }
                            if let Some(bps) = self.note_goodput_bytes(written) {
                                self.scheduler.observe_bandwidth(bps);
                            }
                        }
                    }
                }
                progress_report_countdown = progress_report_countdown.saturating_sub(1);
                if progress_report_countdown == 0 {
                    let shown = pos.saturating_add(write_buf.as_mut().len() as u64);
                    Self::report_progress(0, shown, &self.progress_tx);
                    progress_report_countdown = PROGRESS_REPORT_CHUNK_INTERVAL;
                }
                continue;
            }

            // 小 chunk 聚入对齐缓冲
            if !write_buf.as_mut().is_empty()
                && write_buf.as_mut().len() + chunk.len() > WRITE_BATCH_BYTES
            {
                let batch = write_buf.as_mut().split().freeze();
                let written = Self::write_all_at(
                    storage.as_ref(),
                    pos,
                    batch,
                    &mut self.control_rx,
                    pause_timeout,
                    self.metrics.as_deref(),
                )
                .await?;
                pos += written;
                if let Some(ref limiter) = rate_limiter {
                    limiter.acquire(written).await;
                }
                if let Some(bps) = self.note_goodput_bytes(written) {
                    self.scheduler.observe_bandwidth(bps);
                }
            }
            write_buf.as_mut().extend_from_slice(&chunk);
            progress_report_countdown = progress_report_countdown.saturating_sub(1);
            if write_buf.as_mut().len() >= WRITE_BATCH_BYTES {
                let batch = write_buf.as_mut().split().freeze();
                let written = Self::write_all_at(
                    storage.as_ref(),
                    pos,
                    batch,
                    &mut self.control_rx,
                    pause_timeout,
                    self.metrics.as_deref(),
                )
                .await?;
                pos += written;
                if let Some(ref limiter) = rate_limiter {
                    limiter.acquire(written).await;
                }
                if let Some(bps) = self.note_goodput_bytes(written) {
                    self.scheduler.observe_bandwidth(bps);
                }
            }
            if progress_report_countdown == 0 {
                // 进度含已缓冲未刷部分,避免 UI 卡顿;最终 completed 用落盘 pos
                let shown = pos.saturating_add(write_buf.as_mut().len() as u64);
                Self::report_progress(0, shown, &self.progress_tx);
                progress_report_countdown = PROGRESS_REPORT_CHUNK_INTERVAL;
            }
        }

        // 尾刷
        if !write_buf.as_mut().is_empty() {
            let batch = write_buf.as_mut().split().freeze();
            let written = Self::write_all_at(
                storage.as_ref(),
                pos,
                batch,
                &mut self.control_rx,
                pause_timeout,
                self.metrics.as_deref(),
            )
            .await?;
            pos += written;
            if let Some(ref limiter) = rate_limiter {
                limiter.acquire(written).await;
            }
            if let Some(bps) = self.note_goodput_bytes(written) {
                self.scheduler.observe_bandwidth(bps);
            }
        }

        // 冲刷未满窗口,避免短文件零样本
        if let Some(bps) = self.flush_goodput_window() {
            self.scheduler.observe_bandwidth(bps);
        }
        debug!(written = pos, "整块流式下载写入完成");

        if let Some(expected_size) = expected_size
            && pos != expected_size
        {
            return Err(DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("下载数据不完整: 预期 {expected_size} 字节, 实际写入 {pos} 字节"),
            )));
        }

        // 审计 P0-3:整块路径在标 Completed 前 durable sync,避免快照/状态领先于落盘
        storage.as_ref().sync().await?;

        // 成功路径：durable 后发 completed:true，错误返回路径不发送
        if let Some(tx) = &self.progress_tx {
            let _ = tx.try_send(FragmentProgress::Chunk {
                fragment_index: 0,
                completed: true,
                fragment_downloaded: pos,
            });
        }

        if let Some(frag) = self.fragments.first_mut() {
            if frag.state == crate::fragment::FragmentState::Pending {
                frag.start_download()?;
            }
            frag.complete_download_fast(pos, start_instant.elapsed())?;
        }
        if let Some(ref metrics) = self.metrics {
            metrics.add_bytes(pos);
            metrics.inc_fragment();
        }
        self.state = DownloadState::Completed;
        Ok(())
    }

    /// spawn 一个分片任务(主 dispatch 与 steal 路径共享)
    ///
    /// 统一逻辑:acquire permit -> record_spawn -> 分配 write_buf ->
    /// clone 所有共享 Arc -> spawn task(含指数退避重试循环)
    ///
    /// permit 获取失败时返回 Err(调用方 abort 剩余 task + 设置 Failed 状态)。
    #[allow(clippy::too_many_arguments)]
    async fn spawn_fragment_task(
        ctx: &FragmentSpawnCtx<'_>,
        spec: FragmentSpec,
        handles: &mut JoinSet<FragmentTaskResult>,
    ) -> Result<(), DownloadError> {
        let (frag_index, frag_start, frag_end, mut resume_offset, compute_hash, shared) = spec;

        // acquire permit(阻塞直到有可用许可)
        // permit 的 RAII 保证:task 完成/drop/abort 时自动归还
        let permit = match ctx.semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(e) => {
                return Err(DownloadError::Other(format!("信号量获取失败: {e}").into()));
            }
        };
        // 闭环并发控制:记录 spawn,active+1
        ctx.concurrency_ctrl.record_spawn();
        // 每个 task 独立分配 write_buf(从 BufferPool 或直接分配)
        let mut write_buf = match ctx.buffer_pool {
            Some(bp) => WriteBuf::Guard(bp.clone().alloc_guarded().await),
            None => WriteBuf::Owned(
                AlignedBuf::new(WRITE_BATCH_BYTES).expect("AlignedBuf 分配失败(内存不足)"),
            ),
        };
        write_buf.as_mut().clear();

        let frag_protocol = ctx.protocol.clone();
        let frag_storage = ctx.storage.clone();
        let frag_pool = ctx.pool.clone();
        let frag_url = ctx.url.to_string();
        let frag_host = ctx.host.to_string();
        let frag_limiter = ctx.limiter.clone();
        let mut frag_control_rx = ctx.control_rx.clone();
        let frag_progress_tx = ctx.progress_tx.clone();
        let frag_verifier = ctx.verifier.clone();
        let frag_metrics = ctx.metrics.clone();
        let frag_circuit_breakers = ctx.circuit_breakers.clone();
        // 闭环并发控制:传给 task,退出时 record_complete
        let frag_concurrency_ctrl = ctx.concurrency_ctrl.clone();
        let frag_semaphore = ctx.semaphore.clone();
        let task_completed_tx = ctx.completed_tx.clone();
        let frag_has_mirrors = ctx.has_mirrors;
        let max_retries = ctx.max_retries;
        let pause_timeout = ctx.pause_timeout;
        let skip_write = ctx.skip_write;
        let frag_sync_mode = ctx.sync_mode;
        let frag_loose_partial = Arc::clone(&ctx.loose_partial_bytes);
        let frag_loose_completed = Arc::clone(&ctx.loose_completed_frags);
        let frag_object_identity = ctx.object_identity.clone();
        let frag_range_window = ctx.range_window_bytes;
        let frag_soft_until = Arc::clone(ctx.soft_pressure_until);

        handles.spawn(async move {
            // Option permit:退避睡眠期间释放槽位,使 soft-pressure 降并发立刻生效。
            // 若一直持有 permit,target 从 8→4 但 8 个失败片都在 sleep,有效并发不降。
            let mut permit = Some(permit);
            let mut holding_slot = true;

            // 退避/熔断等待后重新占槽。失败时 holding_slot=false,调用方不得再 record_complete。
            async fn reacquire_slot(
                permit: &mut Option<tokio::sync::OwnedSemaphorePermit>,
                holding_slot: &mut bool,
                ctrl: &ConcurrencyController,
                sem: &std::sync::Arc<tokio::sync::Semaphore>,
                control_rx: &mut Option<tokio::sync::watch::Receiver<TaskCommand>>,
                pause_timeout: Duration,
                _frag_index: u32,
            ) -> Result<(), DownloadError> {
                debug_assert!(!*holding_slot && permit.is_none());
                loop {
                    if let Some(rx) = control_rx.as_mut() {
                        DownloadTask::wait_control_rx(rx, pause_timeout).await?;
                    }
                    if ctrl.should_spawn()
                        && let Ok(p) = sem.clone().try_acquire_owned()
                    {
                        ctrl.record_spawn();
                        *permit = Some(p);
                        *holding_slot = true;
                        return Ok(());
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }

            // 单次尝试 + 指数退避重试
            let mut attempt: u32 = 0;
            let frag_result: FragmentTaskResult = loop {
                // 熔断器检查
                if !frag_has_mirrors && !frag_circuit_breakers.allow(&frag_url) {
                    if attempt >= max_retries {
                        break Err((
                            frag_index,
                            DownloadError::Network(format!("源 {frag_url} 已被熔断,跳过重试")),
                        ));
                    }
                    let next_attempt = attempt + 1;
                    warn!(
                        index = frag_index,
                        attempt = next_attempt,
                        source = %frag_url,
                        "源处于熔断状态,跳过本次尝试"
                    );
                    if let Some(tx) = &frag_progress_tx {
                        let _ = tx.try_send(FragmentProgress::Retry {
                            fragment_index: frag_index,
                            attempt: next_attempt,
                        });
                    }
                    // 熔断等待同样释放槽位,避免占满 active 阻塞健康片
                    drop(permit.take());
                    if holding_slot {
                        frag_concurrency_ctrl.record_complete();
                        holding_slot = false;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    if let Err(wait_err) = reacquire_slot(
                        &mut permit,
                        &mut holding_slot,
                        &frag_concurrency_ctrl,
                        &frag_semaphore,
                        &mut frag_control_rx,
                        pause_timeout,
                        frag_index,
                    )
                    .await
                    {
                        break Err((frag_index, wait_err));
                    }
                    attempt += 1;
                    continue;
                }

                // 审计 HTTP-01:每次 attempt 清空 write_buf。
                // 半缓冲失败(未达 WRITE_BATCH 阈值)会留下残留字节;若不 clear,
                // 下次成功 attempt 的首批数据会与污染前缀拼接写盘。
                write_buf.as_mut().clear();
                let result = Self::download_single_fragment(
                    &frag_protocol,
                    &frag_storage,
                    &frag_pool,
                    &frag_host,
                    &frag_url,
                    frag_index,
                    frag_start,
                    frag_end,
                    resume_offset,
                    pause_timeout,
                    frag_limiter.clone(),
                    &frag_control_rx,
                    &frag_progress_tx,
                    &frag_verifier,
                    compute_hash,
                    write_buf.as_mut(),
                    skip_write,
                    frag_sync_mode,
                    &frag_loose_completed,
                    &frag_loose_partial,
                    &shared,
                    frag_object_identity.clone(),
                    frag_metrics.as_deref(),
                    frag_range_window,
                )
                .await;

                match result {
                    Ok((downloaded, duration, computed_hash)) => {
                        if !frag_has_mirrors {
                            frag_circuit_breakers.record_success(&frag_url);
                        }
                        // 存活分片完成:半衰本任务软压力冷却
                        Self::clear_soft_pressure_cooldown_on_success(&frag_soft_until);
                        break Ok((frag_index, downloaded, duration, computed_hash));
                    }
                    Err(e) => {
                        // 用户暂停:不计入 attempt,等 Resume 后从同一 attempt 重下本片
                        if matches!(e, DownloadError::Paused) {
                            if let Some(rx) = frag_control_rx.as_mut()
                                && let Err(wait_err) =
                                    Self::wait_control_rx(rx, pause_timeout).await
                            {
                                break Err((frag_index, wait_err));
                            }
                            continue;
                        }
                        // 先推进 resume,再决定 soft-pressure 策略:
                        // - 本 attempt 新写字节: progress > resume → 更新 resume
                        // - 或此前已有 resume>0(连接失败未再写):仍算有进度
                        // 有进度: mild -1 + 短 jitter 退避 + 额外预算
                        // 零进度: 减半 + 长退避
                        let mut has_partial_progress = false;
                        if !compute_hash {
                            let progressed = shared
                                .realtime_downloaded
                                .load(std::sync::atomic::Ordering::Acquire);
                            if progressed > resume_offset {
                                debug!(
                                    index = frag_index,
                                    old_resume = resume_offset,
                                    new_resume = progressed,
                                    "分片可重试失败,从已写字节续传"
                                );
                                resume_offset = progressed;
                            }
                            has_partial_progress = resume_offset > 0;
                        }
                        Self::apply_soft_pressure_backoff_ex(
                            &frag_concurrency_ctrl,
                            &e,
                            has_partial_progress,
                            &frag_soft_until,
                        );
                        // 零进度 soft-pressure:丢弃共享 HttpClient 空闲池,避免半死
                        // TLS tunnel 被同身份其它任务复用(MultiTaskIsolationAudit P1)。
                        // mild(有进度)不 clear:链路仍在吐数据,重建池成本高。
                        if !has_partial_progress && Self::is_connection_soft_pressure(&e) {
                            crate::http_client_registry::global_http_client_registry().clear();
                        }
                        let soft_progress_budget =
                            if has_partial_progress && Self::is_connection_soft_pressure(&e) {
                                max_retries.saturating_add(2)
                            } else {
                                max_retries
                            };
                        if !e.is_retryable()
                            || Self::is_pause_timeout_error(&e)
                            || attempt >= soft_progress_budget
                        {
                            if let Some(ref m) = frag_metrics {
                                m.inc_error();
                            }
                            // 软压力(403/TLS EOF/5xx 网关)表示源仍可用但需降并发;
                            // 记 failure 会让 N 片同时放弃时瞬间熔断整源,反而无法恢复。
                            if !frag_has_mirrors && !Self::is_connection_soft_pressure(&e) {
                                frag_circuit_breakers.record_failure(&frag_url);
                            }
                            break Err((frag_index, e));
                        }
                        // 退避:429/503 优先 Retry-After;
                        // 已推进 resume 的 soft-pressure:短退避(链路仍在吐数据,长等浪费);
                        // 零进度 soft-pressure:长退避;否则 Full Jitter 指数退避。
                        let backoff = match &e {
                            DownloadError::Throttled {
                                retry_after_secs: Some(secs),
                            } => Duration::from_secs((*secs).min(1024)),
                            _ => {
                                let base_secs = 1u64 << attempt.min(10);
                                let base = if base_secs <= 1 {
                                    Duration::from_secs(1)
                                } else {
                                    let seed = (frag_index as u64)
                                        .wrapping_mul(0x9E3779B97F4A7C15)
                                        .wrapping_add(attempt as u64);
                                    let log2 = base_secs.trailing_zeros();
                                    let hash = seed.wrapping_mul(0x517cc1b727220a95);
                                    let jitter = hash >> (64 - log2);
                                    Duration::from_secs(base_secs.saturating_sub(jitter).max(1))
                                };
                                if Self::is_connection_soft_pressure(&e) {
                                    if has_partial_progress {
                                        // 已有进度:短退避上限 2s + Full Jitter,避免多分片同步重试打爆代理
                                        let cap_ms = 250u64
                                            .saturating_mul(1u64 << attempt.min(3))
                                            .clamp(250, 2000);
                                        let seed = (frag_index as u64)
                                            .wrapping_mul(0x9E3779B97F4A7C15)
                                            .wrapping_add(attempt as u64)
                                            .wrapping_mul(0x517cc1b727220a95);
                                        let jittered = 1 + (seed % cap_ms);
                                        Duration::from_millis(jittered)
                                    } else {
                                        Self::soft_pressure_backoff_secs(attempt, base)
                                    }
                                } else {
                                    base
                                }
                            }
                        };
                        let next_attempt = attempt + 1;
                        warn!(
                            index = frag_index,
                            attempt = next_attempt,
                            max_retries = soft_progress_budget,
                            has_partial_progress,
                            backoff_ms = backoff.as_millis() as u64,
                            error = %e,
                            "分片下载失败,退避后重试"
                        );
                        // 任务级 retry_count 聚合:可重试失败时发出 Retry 事件
                        if let Some(tx) = &frag_progress_tx {
                            let _ = tx.try_send(FragmentProgress::Retry {
                                fragment_index: frag_index,
                                attempt: next_attempt,
                            });
                        }
                        // 不在中间重试记 record_failure:多分片并发同一 URL 时,
                        // N 片各失败 1 次就会瞬间达到阈值(默认 5)误熔断整个源。
                        // 熔断只在最终放弃(上方 break Err)时记一次;成功路径仍 record_success。
                        frag_protocol.clear_selected().await;
                        // 退避期间释放 permit + active,使 set_target 降并发立刻生效;
                        // 睡眠后再按 should_spawn 重新占槽,避免 8 片同时 sleep 占满。
                        drop(permit.take());
                        if holding_slot {
                            frag_concurrency_ctrl.record_complete();
                            holding_slot = false;
                        }
                        let mut wait = backoff;
                        if Self::is_connection_soft_pressure(&e) {
                            // 片间错开重连,减轻代理/对端同步 TLS 风暴
                            wait = wait.saturating_add(Self::soft_reconnect_spacing_delay(150));
                        }
                        tokio::time::sleep(wait).await;
                        if let Err(wait_err) = reacquire_slot(
                            &mut permit,
                            &mut holding_slot,
                            &frag_concurrency_ctrl,
                            &frag_semaphore,
                            &mut frag_control_rx,
                            pause_timeout,
                            frag_index,
                        )
                        .await
                        {
                            break Err((frag_index, wait_err));
                        }
                        attempt += 1;
                    }
                }
            };
            drop(permit);

            // 上报结果:成功经 completed_tx(主循环处理),JoinSet 返回虚拟信号;
            // 失败不经 completed_tx,由 JoinSet 直接返回(主循环处理错误)。
            // 这与旧 per-worker 模型一致:避免成功结果被 completed_rx 和
            // join_next 双重处理导致 record_completed_fragment 重复调用。
            // 闭环并发控制:仅在仍持有槽位时 record_complete。
            if holding_slot {
                frag_concurrency_ctrl.record_complete();
            }
            match frag_result {
                Ok(tuple) => {
                    let _ = task_completed_tx.send(Ok(tuple));
                    Ok((0, 0, Duration::ZERO, None)) // 虚拟信号:join_next 忽略
                }
                Err(e) => Err(e),
            }

            // write_buf 在 task 结束时析构:
            // Guard 变体经 BufferGuard::drop 归还到池并恢复许可;
            // Owned 变体的 AlignedBuf 正常释放内存。
        });

        Ok(())
    }

    /// 并发分片下载
    ///
    /// 将信号量获取移入 spawn 任务内部,确保分片任务立即启动网络请求,
    /// 仅在实际占用并发槽位时才等待信号量,最大化网络并发。
    /// 使用调度器的带宽预测动态调整并发度。
    ///
    /// 每个分片 spawn 内部自带重试循环:单次尝试失败后按指数退避重试,
    /// 直到 `max_retries` 耗尽才整体失败。已完成的分片(断点续传)直接跳过。
    async fn execute_fragmented_download(&mut self) -> DownloadResult<()> {
        if self.config.max_concurrent_fragments == 0 {
            return Err(DownloadError::Config(
                "max_concurrent_fragments 不能为 0".to_string(),
            ));
        }

        // 使用调度器获取动态并发建议
        let file_size = self
            .metadata
            .as_ref()
            .and_then(|m| m.file_size)
            .unwrap_or(0);
        let recommendation = self
            .scheduler
            .recommend(file_size, self.config.max_concurrent_fragments);

        // 使用调度器建议的并发度,但不超过配置的最大值。
        // BT/magnet 冷启动(低置信度)解耦:直接用配置并发,HTTP 路径不变
        // (cold-start 起步 + ramp 爬坡 + 429 保护全部保留)。
        let (effective_concurrency, concurrency_reason) = match self
            .bt_cold_start_concurrency_override(&recommendation)
        {
            Some(configured) => (configured as usize, "bt_cold_start"),
            None => {
                let mut c = recommendation
                    .concurrency
                    .min(self.config.max_concurrent_fragments)
                    .max(1);
                let mut reason = "scheduler";
                if let Some(cap) = self.proxy_cold_start_cap_for_config(recommendation.confidence) {
                    c = c.min(cap).max(1);
                    reason = "scheduler+proxy_cold_start";
                }
                // 稳态天花板:即使置信度升高也不在代理下抬到 4+ 打爆
                let before = c;
                c = self.apply_proxy_concurrency_ceiling(c);
                if c < before {
                    reason = "scheduler+proxy_ceiling";
                }
                (c as usize, reason)
            }
        };

        debug!(
            configured_concurrency = self.config.max_concurrent_fragments,
            recommended_concurrency = recommendation.concurrency,
            effective_concurrency = effective_concurrency,
            confidence = recommendation.confidence,
            reason = concurrency_reason,
            "使用调度器并发建议"
        );

        // FIX-05: Semaphore 作为硬上限(防 OOM)应用配置最大值 max_concurrent_fragments，
        // 而非初始建议值 effective_concurrency。ConcurrencyController.should_spawn() 作为
        // 软目标门禁(active < target)，实现动态升降:上调时 should_spawn 放行、Semaphore 有余量；
        // 下调时 should_spawn 阻止新 spawn、在途任务自然完成。旧实现用 effective_concurrency
        // 构造 Semaphore，导致初始建议为 1 时即便后续 set_target(4) 也无法超过 1 个在途。
        let semaphore = Arc::new(Semaphore::new(
            self.config.max_concurrent_fragments as usize,
        ));
        // 闭环并发控制(P2-5):ConcurrencyController 维护 active/target,
        // 可升可降(set_target)。Semaphore 作为硬上限(permits RAII),
        // Controller 作为软目标(动态调优)。spawn 前检查 should_spawn()。
        // 解决 tokio::Semaphore add_permits 只能增不能降的限制(FastBioDL 闭环控制)。
        let concurrency_ctrl = Arc::new(ConcurrencyController::new(
            effective_concurrency as u32,
            self.config.max_concurrent_fragments,
        ));
        let max_concurrent_fragments = self.config.max_concurrent_fragments;
        // 周期性 re-recommend 间隔:用 sampling_interval_secs(默认 5s),
        // 最小 2s 避免频繁 re-recommend 抖动。
        let reschedule_interval =
            Duration::from_secs(self.scheduler_config.sampling_interval_secs.max(2));
        let url = self.url.clone();
        let storage = self
            .storage
            .clone()
            .ok_or_else(|| DownloadError::Config("存储未初始化".into()))?;
        let protocol = self.protocol.clone();
        let pool = self.pool.clone();
        let buffer_pool = self.buffer_pool.clone();
        self.refresh_resolved_host_from_protocol();
        let host = self.request_host()?;
        let pause_timeout = Duration::from_secs(self.config.pause_timeout_secs);
        let mut control_rx = self.control_rx.clone();
        let progress_tx = self.progress_tx.clone();
        let max_retries = self.config.max_retries;
        // 优先使用外部共享限速器(跨任务全局限速),否则从配置创建 per-task 限速器
        let rate_limiter: Option<Arc<RateLimiter>> = self.rate_limiter.clone().or_else(|| {
            self.config
                .rate_limit_bytes_per_sec
                .filter(|&bps| bps > 0)
                .map(|bps| Arc::new(RateLimiter::new(bps)))
        });
        let circuit_breakers = self.circuit_breakers.clone();
        let metrics = self.metrics.clone();
        tracing::debug!(
            has_progress_tx = progress_tx.is_some(),
            frag_count = self.fragments.len(),
            "分片下载准备就绪"
        );

        let mut handles: JoinSet<FragmentTaskResult> = JoinSet::new();

        // 仅对未完成(Pending)的分片下载,已完成分片(断点续传)跳过
        let fragment_specs: Vec<FragmentSpec> = self
            .fragments
            .iter()
            .filter(|frag| frag.state == crate::fragment::FragmentState::Pending)
            .map(|frag| {
                (
                    frag.info.index,
                    frag.info.start,
                    frag.info.end,
                    frag.resume_offset,
                    frag.info.hash.is_some(),
                    FragmentShared {
                        effective_end: Arc::clone(&frag.effective_end),
                        realtime_downloaded: Arc::clone(&frag.realtime_downloaded),
                    },
                )
            })
            .collect();

        // ── spawn-per-fragment 模型 ────────────────────────────────────
        // dispatcher 逻辑内联到主循环:从 frag_rx 拉取 spec → semaphore.acquire_owned →
        // handles.spawn(download_single_fragment)。Semaphore 自然限制并发,
        // add_permits 后下次 acquire 成功即可 spawn 新 task(动态并发基础)。
        //
        // 相比旧 per-worker channel 模型的优势:
        // 1. 消除 dispatcher round-robin try-send 逻辑(无 per-worker channel)
        // 2. Semaphore permits 即真实并发上限(add_permits 可运行时提升)
        // 3. 每个 fragment task 独立 spawn,无固定 worker 数量限制
        // 容量留余量给 rebalance 重入队(慢片拆分后的尾片)
        let (frag_tx_raw, mut frag_rx) =
            mpsc::channel::<FragmentSpec>((effective_concurrency * 2).max(8));
        let mut frag_tx = Some(frag_tx_raw);
        let (completed_tx, mut completed_rx) = mpsc::unbounded_channel::<FragmentTaskResult>();

        // 入队前检查暂停/取消信号,避免在暂停状态下无意义地启动
        if let Some(ref rx) = control_rx {
            let mut check_rx = rx.clone();
            Self::wait_control_rx(&mut check_rx, pause_timeout).await?;
        }

        // 在独立 task 中入队所有分片:frag_tx.send().await 在 channel 满时阻塞,
        // 必须与主循环(从 frag_rx 拉取并 spawn task)并发执行,否则 channel 容量 <
        // 分片数时死锁。入队 task 持有 frag_tx,完成后 drop 使 frag_rx 返回 None。
        //
        // start_download / inc_fragment 需在入队前同步执行(修改 self.fragments),
        // 仅 send 入队异步化。将已标记 start_download 的 spec 收集后 spawn 入队。
        let mut pending_specs: Vec<FragmentSpec> = Vec::with_capacity(fragment_specs.len());
        for spec in &fragment_specs {
            let frag_index = spec.0;
            if frag_index as usize >= self.fragments.len() {
                return Err(DownloadError::Config("分片索引越界".into()));
            }
            self.fragments[frag_index as usize].start_download()?;
            if let Some(ref m) = metrics {
                m.inc_fragment();
            }
            pending_specs.push(spec.clone());
        }
        // 初始入队用 clone 的 sender;主循环保留 Option<Sender> 供 rebalance 重入队。
        // 全部初始分片入队后不 drop 主 sender,避免 rebalance 无法再 enqueue。
        let frag_tx_enqueue = frag_tx.as_ref().expect("frag_tx 刚创建").clone();
        let mut enqueue_handle = tokio::spawn(async move {
            for spec in pending_specs {
                if frag_tx_enqueue.send(spec).await.is_err() {
                    break; // 主循环退出,frag_rx 已 drop
                }
            }
            // frag_tx_enqueue drop;主循环仍持有 frag_tx(Option)
        });

        // 主循环:同时充当 dispatcher(从 frag_rx 拉取 spec + spawn task)和结果收集器
        let frag_url = url.clone();
        let frag_storage = storage.clone();
        let frag_protocol = protocol.clone();
        let frag_semaphore = semaphore.clone();
        // P1:镜像路径下 engine 层跳过主 host 的 pool.acquire,
        // 改由 MirrorProtocol(已注入同一 pool)按真实命中镜像 host acquire,
        // 使各镜像能各自占满自己的 per-host 配额。单源路径保持 engine 层 acquire。
        let frag_pool = if self.has_mirrors { None } else { pool.clone() };
        let frag_buffer_pool = buffer_pool.clone();
        let frag_host = host.clone();
        let frag_limiter = rate_limiter.clone();
        let frag_control_rx = control_rx.clone();
        let frag_progress_tx = progress_tx.clone();
        let frag_metrics = metrics.clone();
        let frag_circuit_breakers = circuit_breakers.clone();
        // B5:镜像路径禁用 engine 层熔断(以主 URL 为 key 会误熔断整个任务),
        // 改由 MirrorProtocol 的 per-source stats 接管故障隔离。
        // Loose group-commit:任务级完成分片计数,各 fragment worker 共享
        let loose_completed_frags = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // Loose partial 进度 group-commit:任务级累计写入字节水位
        let loose_partial_bytes = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let frag_has_mirrors = self.has_mirrors;
        let frag_verifier = self.verifier.clone();
        // P2-4: 协议直接管理存储时跳过引擎 write_all_at(消除双存储写放大)
        let skip_write = self
            .metadata
            .as_ref()
            .map(|m| m.protocol_managed_storage)
            .unwrap_or(false);

        // completed_tx 包装为 Option:所有分片 spawn 完成后(frag_rx 返回 None)take+drop,
        // 使 completed_rx 在所有 task 完成后能返回 None 触发主循环退出。
        let mut completed_tx = Some(completed_tx);

        // 动态并发度 re-recommend 定时器
        let mut reschedule_timer = interval(reschedule_interval);

        loop {
            // 用户 Pause:强制 abort 在途分片并停车,避免 select 饿死/阻塞 await 导致“无法暂停”
            if Self::control_is_paused(&control_rx) {
                tracing::debug!("检测到 Pause,中止在途分片并等待 Resume");
                // 停掉入队任务,丢弃尚未 spawn 的 spec(Pause 期间不应再开新片)
                enqueue_handle.abort();
                if let Some(tx) = frag_tx.take() {
                    drop(tx);
                }
                while frag_rx.try_recv().is_ok() {}
                // 强制终止在途 IO(含卡在 pool.acquire / stream 中的 task)
                Self::abort_remaining_fragment_tasks(&mut handles).await;
                // abort 路径可能跳过 record_complete,必须清零 active
                concurrency_ctrl.reset_active();
                // drain 成功结果(若 abort 前刚好完成)
                while let Ok(result) = completed_rx.try_recv() {
                    if let Ok((index, downloaded, duration, computed_hash)) = result
                        && (index != 0 || downloaded != 0)
                    {
                        let _ = self.record_completed_fragment(
                            index,
                            downloaded,
                            duration,
                            computed_hash,
                        );
                    }
                }
                // Downloading → Pending + 固化 resume_offset(字节级续传)
                for frag in &mut self.fragments {
                    frag.park_for_pause();
                }
                // 等 Resume / Cancel / 超时
                if let Some(rx) = control_rx.as_mut() {
                    Self::wait_control_rx(rx, pause_timeout).await?;
                }
                // Resume:把仍为 Pending 的分片重新入队
                let pending: Vec<FragmentSpec> = self
                    .fragments
                    .iter()
                    .filter(|f| f.state == crate::fragment::FragmentState::Pending)
                    .map(|frag| {
                        (
                            frag.info.index,
                            frag.info.start,
                            frag.info.end,
                            frag.resume_offset,
                            frag.info.hash.is_some(),
                            FragmentShared {
                                effective_end: Arc::clone(&frag.effective_end),
                                realtime_downloaded: Arc::clone(&frag.realtime_downloaded),
                            },
                        )
                    })
                    .collect();
                if pending.is_empty() {
                    // 全部已完成
                    frag_tx.take();
                    completed_tx.take();
                    break;
                }
                let (new_tx, new_rx) =
                    mpsc::channel::<FragmentSpec>((effective_concurrency * 2).max(8));
                frag_rx = new_rx;
                frag_tx = Some(new_tx);
                let mut requeue = Vec::with_capacity(pending.len());
                for spec in pending {
                    let idx = spec.0 as usize;
                    if idx < self.fragments.len() {
                        // park 后是 Pending,可再 start_download
                        if self.fragments[idx].state == crate::fragment::FragmentState::Pending {
                            self.fragments[idx].start_download()?;
                        }
                    }
                    requeue.push(spec);
                }
                let frag_tx_enqueue = frag_tx.as_ref().expect("frag_tx recreated").clone();
                enqueue_handle = tokio::spawn(async move {
                    for spec in requeue {
                        if frag_tx_enqueue.send(spec).await.is_err() {
                            break;
                        }
                    }
                });
                tracing::debug!("Resume 后已重新入队未完成分片");
                continue;
            }

            tokio::select! {
                // 动态并发度:周期性 re-recommend,带宽变化时提升并发度
                // guard !handles.is_empty():只在有在途 task 时才 poll,
                // 所有 task 完成后此分支 disable,使 else => break 能正确触发
                _ = reschedule_timer.tick(), if !handles.is_empty() => {
                    // 用户暂停期间禁止 re-recommend / rebalance(避免 Pause 后仍开新片)
                    if Self::control_is_paused(&control_rx) {
                        continue;
                    }
                    // BT:周期性上报 peer 发现快照,供 UI「0 peer / 发现中」
                    self.try_emit_peer_stats();
                    let rec = self.scheduler.recommend(file_size, max_concurrent_fragments);
                    let old = concurrency_ctrl.target();
                    let desired = self.apply_proxy_concurrency_ceiling(
                        rec.concurrency.min(max_concurrent_fragments).max(1),
                    );
                    // 抬升步进限制:冷却结束也不允许一次跳回满配
                    let new_target = if self.remote_http_proxy_active() {
                        Self::clamp_concurrency_scale_up_ex(old, desired, true)
                    } else {
                        Self::clamp_concurrency_scale_up(old, desired)
                    };
                    // 低置信度(慢启动/样本不足)只升不降;软压力冷却期内禁止抬升
                    let allow = if new_target > old {
                        !Self::soft_pressure_blocks_scale_up(&self.soft_pressure_until)
                    } else {
                        rec.confidence > 0.5
                    };
                    if allow && new_target != old {
                        concurrency_ctrl.set_target(new_target);
                        debug!(
                            old_concurrency = old,
                            new_concurrency = new_target,
                            active = concurrency_ctrl.active(),
                            confidence = rec.confidence,
                            "闭环并发度调整"
                        );
                    }
                    // 安全 rebalance:try_send 入队,Full 时 revert(不堵主循环)
                    // 软压力冷却期禁止拆片:rebalance 会新增连接,抵消降并发
                    // rebalance_enabled=false 时跳过(A/B 量化 on/off 收益用)
                    if self.rebalance_enabled
                        && let Some(tx) = frag_tx.as_ref()
                        && !Self::soft_pressure_blocks_scale_up(&self.soft_pressure_until)
                    {
                        // queue_empty:中央队列无待领取分片时进入收尾冷却(500ms)
                        let queue_empty = frag_rx.is_empty();
                        let _ = self
                            .try_rebalance_slowest_fragment(tx, &concurrency_ctrl, queue_empty)
                            .await;
                    }
                }
                // dispatcher:从中央队列拉取分片,acquire permit 后 spawn task
                // 闭环并发控制:仅当 active < target 时才拉取新分片(可降并发)
                // Pause 时禁止 spawn:否则 UI 已暂停仍会开新分片,表现为“无法暂停”
                // should_spawn()=false 时,等待 task 完成(record_complete)使 active 下降
                spec = frag_rx.recv(), if concurrency_ctrl.should_spawn()
                    && !Self::control_is_paused(&control_rx) => {
                    match spec {
                        Some(spec) => {
                            let spawn_ctx = FragmentSpawnCtx {
                                protocol: &frag_protocol,
                                storage: &frag_storage,
                                pool: &frag_pool,
                                url: &frag_url,
                                host: &frag_host,
                                limiter: &frag_limiter,
                                control_rx: &frag_control_rx,
                                progress_tx: &frag_progress_tx,
                                verifier: &frag_verifier,
                                metrics: &frag_metrics,
                                circuit_breakers: &frag_circuit_breakers,
                                concurrency_ctrl: &concurrency_ctrl,
                                semaphore: &frag_semaphore,
                                completed_tx: completed_tx.as_ref().unwrap(),
                                buffer_pool: &frag_buffer_pool,
                                has_mirrors: frag_has_mirrors,
                                max_retries,
                                pause_timeout,
                                skip_write,
                                sync_mode: self.config.crash_consistency_mode,
                                loose_completed_frags: Arc::clone(&loose_completed_frags),
                                loose_partial_bytes: Arc::clone(&loose_partial_bytes),
                                object_identity: self
                                    .metadata
                                    .as_ref()
                                    .map(ObjectIdentity::from_metadata),
                                range_window_bytes: self.proxy_range_window_bytes(),
                                soft_pressure_until: &self.soft_pressure_until,
                            };
                            if let Err(e) =
                                Self::spawn_fragment_task(&spawn_ctx, spec, &mut handles).await
                            {
                                // H2: 捕获 RangeNotSupported 降级为整块下载
                                if let Some(result) = self
                                    .try_range_not_supported_fallback(&e, &mut handles, &mut completed_rx)
                                    .await
                                {
                                    return result;
                                }
                                Self::abort_remaining_fragment_tasks(&mut handles).await;
                                Self::drain_completed_channel(&mut *self, &mut completed_rx)?;
                                self.state = DownloadState::Failed;
                                return Err(e);
                            }
                        }
                        None => {
                            // 初始队列耗尽。若仍有在途 task,保留 frag_tx 供 rebalance;
                            // 仅当无在途且无 rebalance 可能时再 drop sender + completed_tx。
                            if handles.is_empty() {
                                frag_tx.take();
                                completed_tx.take();
                            }
                            // 否则继续等待 completed / rebalance 重入队。
                        }
                    }
                }
                // 结果收集:completed_rx 始终 poll(无 guard),确保成功结果不丢失。
                // 退出依赖:completed_tx 原始端在 frag_rx 耗尽后 take+drop,所有 task 的
                // clone 在 task 结束时 drop,completed_rx.recv() 返回 None 触发 else => break。
                Some(result) = completed_rx.recv() => {
                    match result {
                        // task 正常退出(虚拟信号),跳过
                        Ok((0, 0, _, _)) => continue,
                        Ok((index, downloaded, duration, computed_hash)) => {
                            self.record_completed_fragment(
                                index,
                                downloaded,
                                duration,
                                computed_hash,
                            )?;
                            // 样本驱动:每片完成后立即 re-recommend,避免 5s 定时器拖慢爬坡。
                            // 低置信度只升不降;软压力冷却期内禁止抬升。
                            let rec = self
                                .scheduler
                                .recommend(file_size, max_concurrent_fragments);
                            let old = concurrency_ctrl.target();
                            let desired = self.apply_proxy_concurrency_ceiling(
                                rec.concurrency.min(max_concurrent_fragments).max(1),
                            );
                            let new_target = if self.remote_http_proxy_active() {
                                Self::clamp_concurrency_scale_up_ex(old, desired, true)
                            } else {
                                Self::clamp_concurrency_scale_up(old, desired)
                            };
                            let allow = if new_target > old {
                                !Self::soft_pressure_blocks_scale_up(&self.soft_pressure_until)
                            } else {
                                rec.confidence > 0.5
                            };
                            if allow && new_target != old {
                                concurrency_ctrl.set_target(new_target);
                            }
                            // 快片完成后立刻 rebalance 慢片,不必等 reschedule_timer
                            // 软压力冷却期禁止拆片
                            // rebalance_enabled=false 时跳过(A/B 量化 on/off 收益用)
                            if self.rebalance_enabled
                                && let Some(tx) = frag_tx.as_ref()
                                && !Self::soft_pressure_blocks_scale_up(&self.soft_pressure_until)
                            {
                                // queue_empty:中央队列无待领取分片时进入收尾冷却(500ms)
                                let queue_empty = frag_rx.is_empty();
                                let _ = self
                                    .try_rebalance_slowest_fragment(tx, &concurrency_ctrl, queue_empty)
                                    .await;
                            }
                        }
                        Err((failed_index, e)) => {
                            // H2: 捕获 RangeNotSupported(协议层对 GET Range 返回 200
                            // 的运行时降级信号),中止在途 → 重新规划单分片 → 整块下载
                            if let Some(result) = self
                                .try_range_not_supported_fallback(&e, &mut handles, &mut completed_rx)
                                .await
                            {
                                return result;
                            }
                            Self::abort_remaining_fragment_tasks(&mut handles).await;
                            Self::drain_completed_channel(&mut *self, &mut completed_rx)?;
                            if let Some(frag) = self.fragments.get_mut(failed_index as usize) {
                                frag.force_fail();
                            }
                            self.state = DownloadState::Failed;
                            return Err(e);
                        }
                    }
                }
                Some(joined) = handles.join_next() => {
                    match joined {
                        Ok(result) => {
                            // 成功结果已由 completed_tx 处理(返回虚拟 (0,0,..)),
                            // 失败不经 completed_tx 由 JoinSet 直接返回
                            match result {
                                Ok((0, 0, _, _)) => {}
                                Ok((index, downloaded, duration, computed_hash)) => {
                                    // 防御性:若 completed_tx 发送失败(如 channel 已关闭),
                                    // 仍从 join 结果补录(此时不会重复——record_completed_fragment
                                    // 的状态机会拒绝 Done->Done,但补录路径在正常流程不应触发)
                                    if index != 0 || downloaded != 0 {
                                        let _ = self.record_completed_fragment(
                                            index,
                                            downloaded,
                                            duration,
                                            computed_hash,
                                        );
                                    }
                                }
                                Err((failed_index, e)) => {
                                    // H2: 同 completed_rx 路径,捕获 RangeNotSupported 降级
                                    if let Some(result) = self
                                        .try_range_not_supported_fallback(
                                            &e,
                                            &mut handles,
                                            &mut completed_rx,
                                        )
                                        .await
                                    {
                                        return result;
                                    }
                                    Self::abort_remaining_fragment_tasks(&mut handles).await;
                                    Self::drain_completed_channel(&mut *self, &mut completed_rx)?;
                                    if let Some(frag) =
                                        self.fragments.get_mut(failed_index as usize)
                                    {
                                        frag.force_fail();
                                    }
                                    self.state = DownloadState::Failed;
                                    return Err(e);
                                }
                            }
                        }
                        Err(error) => {
                            Self::abort_remaining_fragment_tasks(&mut handles).await;
                            Self::drain_completed_channel(&mut *self, &mut completed_rx)?;
                            self.state = DownloadState::Failed;
                            return Err(DownloadError::Other(
                                format!("分片任务 panic: {error}").into(),
                            ));
                        }
                    }
                }
                else => break,
            }
            // 退出条件:所有分片已入队(frag_tx 已 drop)+ 所有 task 已完成(handles 空)。
            // task 退出时先 send 结果再返回,join_next 返回时结果必在 completed_rx 缓冲中。
            // 但 select! 可能先消费 join_next(虚拟信号)而非 completed_rx,
            // 导致 break 时 completed_rx 仍有未消费结果。必须先 drain 再 break。
            if handles.is_empty() && frag_rx.is_empty() {
                // 无在途且队列空:释放 sender,确保 completed_rx 可 EOF
                frag_tx.take();
                completed_tx.take();
                Self::drain_completed_channel(&mut *self, &mut completed_rx)?;
                break;
            }
        }

        // 入队 task 在所有分片已 send 后自然完成(或被 abort)
        enqueue_handle.abort();

        // 冲刷未满窗口的聚合 goodput,避免短任务/末片零样本
        if let Some(bps) = self.flush_goodput_window() {
            self.scheduler.observe_bandwidth(bps);
        }

        // 显式关闭存储后端,close() 内部已调用 sync_data() 保证数据落盘,
        // 无需额外 sync() 避免双重 fsync 导致的 Flush Storm
        storage.close().await?;

        // 审计 BT-17:protocol_managed 时 FileStream 读完 ≠ piece truth 完成。
        // 在标 Completed 前等待 librqbit wait_until_completed(带 peer_wait 看门狗)。
        #[cfg(feature = "magnet")]
        self.wait_bt_piece_truth_if_protocol_managed().await?;

        // 审计 S-03:已知长度分片路径在标 Completed 前做结构/字节不变式检查。
        Self::validate_known_length_fragment_completion(
            &self.fragments,
            self.metadata.as_ref().and_then(|m| m.file_size),
        )?;

        self.state = DownloadState::Completed;
        debug!("全部分片下载完成");
        Ok(())
    }

    /// 安全慢片 rebalance:拆分下载中剩余字节最大的可拆分片,try_send 入队。
    ///
    /// 相对已删除的 work-stealing:
    /// - **故意用 `try_send` 而非 `send().await`**:主循环在完成事件路径
    ///   同步 await 本函数;channel 满时阻塞 send 会永久卡住 dispatcher
    ///   (实测:冷启动 concurrency=4、容量 8 时 4/17 分片后进度冻结)。
    ///   丢一次 rebalance 可通过 `revert_split` 安全回滚,下次定时/完成再试。
    /// - 入队失败(Full/Closed)则 `revert_split` 回滚,并计 `rebalance_dropped`
    /// - 不依赖 steal_rx / 额外 completed_tx 生命周期
    ///
    /// 策略(对齐空闲 worker 救援,仍保持安全边界):
    /// - 触发:仅当 `concurrency_ctrl.active() < target()` 有空闲 worker 时拆
    /// - 选择:下载速率最低的片(P1-2:此前按 remaining 最大选,无法区分"大但快"与"大且慢");
    ///   速率 = realtime_downloaded / start_time.elapsed();含最后一片 straggler
    /// - 年龄门槛 2s + remaining >= 2*MIN_SPLIT_SIZE
    /// - 拆点对半 `done_abs + remaining/2`,仍尊重 write_safety / min_split_point
    /// - 冷却:收尾(queue_empty)500ms;非收尾 5s;代理路径 20s
    /// - 在途写安全边距 `min(WRITE_BATCH, remaining/4)`
    /// - `info.hash.is_some()` 时 try_split 拒绝拆分
    pub(super) async fn try_rebalance_slowest_fragment(
        &mut self,
        frag_tx: &mpsc::Sender<FragmentSpec>,
        concurrency_ctrl: &ConcurrencyController,
        queue_empty: bool,
    ) -> DownloadResult<bool> {
        use crate::fragment::{FragmentState, MIN_SPLIT_SIZE};
        use std::sync::atomic::Ordering;

        /// 新 spawn 片最短观察时间,避免刚启动即被拆。
        /// 2s 兼顾拖尾救援与 WAN 抖动:过短会在 TLS/限流抖动下连环拆片。
        const REBALANCE_MIN_AGE: Duration = Duration::from_secs(2);
        /// 非收尾两次成功 rebalance 最小间隔:soft-pressure 恢复后若每完成事件都拆
        /// 会把 1 片拆成十几片(kernel.org 曾 21 次)。
        const REBALANCE_MIN_INTERVAL: Duration = Duration::from_secs(5);
        /// 代理路径更长间隔:Range 窗口已增请求密度,恢复瞬间拆尾=再增 TLS。
        const REBALANCE_MIN_INTERVAL_PROXY: Duration = Duration::from_secs(20);
        /// 收尾(队列空、仅剩 straggler)缩短冷却,加快最后一片救援。
        const REBALANCE_MIN_INTERVAL_ENDGAME: Duration = Duration::from_millis(500);

        // 无空闲 worker 时拆片只会积压队列,徒增连接/调度成本。
        if concurrency_ctrl.active() >= concurrency_ctrl.target() {
            return Ok(false);
        }

        // 收尾优先:最后一片 straggler 需要短冷却,代理 20s 仅约束非收尾路径
        let min_interval = if queue_empty {
            REBALANCE_MIN_INTERVAL_ENDGAME
        } else if self.remote_http_proxy_active() {
            REBALANCE_MIN_INTERVAL_PROXY
        } else {
            REBALANCE_MIN_INTERVAL
        };
        if let Some(at) = self.last_rebalance_at
            && at.elapsed() < min_interval
        {
            return Ok(false);
        }

        // 选下载速率最低的可拆在途片(而非 remaining 最大):
        // (idx, remaining, realtime, rate_bytes_per_sec)
        //
        // P1-2:此前按 remaining(字节数)选片,无法区分"大但快"与"大且慢"。
        // 改按瞬时速率 rt / start_time.elapsed() 选最慢片拆分,更精准救援 straggler。
        // age 门槛(≥2s)保证 elapsed 非零,避免除以近零;速率用 u64 bytes/sec 整数比较。
        let mut best: Option<(usize, u64, u64, u64)> = None;
        for (i, frag) in self.fragments.iter().enumerate() {
            if frag.state != FragmentState::Downloading {
                continue;
            }
            let rt = frag.realtime_downloaded.load(Ordering::Acquire);
            let eff_end = frag.effective_end.load(Ordering::Acquire);
            // 防溢出:用 saturating_add 与实际拆分逻辑保持一致。
            let remaining = eff_end
                .saturating_add(1)
                .saturating_sub(frag.info.start.saturating_add(rt));
            if remaining < MIN_SPLIT_SIZE.saturating_mul(2) {
                continue;
            }
            let age_ok = frag
                .start_time
                .map(|t| t.elapsed() >= REBALANCE_MIN_AGE)
                .unwrap_or(false);
            if !age_ok {
                continue;
            }
            // 瞬时速率:rt / elapsed_secs。elapsed ≥ 2s(age 门槛保证),
            // rt 含 resume_offset(已持久化字节)——但 resume_offset 在 start_time 之后不变,
            // 故 rt/elapsed 反映的是含续传字节的整体进度速率,各片横向比较仍有效。
            let elapsed_secs = frag
                .start_time
                .map(|t| t.elapsed().as_secs_f64())
                .unwrap_or(f64::MAX);
            let rate = if elapsed_secs > 0.0 {
                (rt as f64 / elapsed_secs) as u64
            } else {
                u64::MAX // elapsed=0 不可能(age≥2s),兜底视为最快
            };
            match best {
                None => best = Some((i, remaining, rt, rate)),
                // 选速率最低的(rate 最小 = 最慢 = 最该被拆分救援)
                Some((_, _, _, br)) if rate < br => best = Some((i, remaining, rt, rate)),
                _ => {}
            }
        }
        let Some((idx, _best_remaining, realtime, _best_rate)) = best else {
            return Ok(false);
        };

        let frag = &self.fragments[idx];
        let start = frag.info.start;
        let eff_end = frag.effective_end.load(Ordering::Acquire);
        let done_abs = start.saturating_add(realtime);
        let remaining = eff_end.saturating_add(1).saturating_sub(done_abs);
        if remaining < MIN_SPLIT_SIZE.saturating_mul(2) {
            return Ok(false);
        }
        // 在途写可能超前于 realtime。边距取 min(WRITE_BATCH, remaining/4)。
        let write_safety = (WRITE_BATCH_BYTES as u64).min(remaining.saturating_div(4));
        let min_split_point = done_abs
            .saturating_add(write_safety)
            .max(done_abs.saturating_add(1));
        // 对半拆分:理想点 done_abs + remaining/2,不得落在 write_safety 内。
        let ideal_half = done_abs.saturating_add(remaining.saturating_div(2));
        let mut split_point = ideal_half.max(min_split_point);
        // 两侧均须 >= MIN_SPLIT_SIZE
        let left_len = split_point.saturating_sub(done_abs);
        let right_len = eff_end.saturating_add(1).saturating_sub(split_point);
        if left_len < MIN_SPLIT_SIZE {
            split_point = done_abs.saturating_add(MIN_SPLIT_SIZE);
        } else if right_len < MIN_SPLIT_SIZE {
            split_point = eff_end.saturating_add(1).saturating_sub(MIN_SPLIT_SIZE);
        }
        if split_point < min_split_point {
            // 对半/MIN 调整后仍落在安全线内:贴安全线
            split_point = min_split_point;
        }
        // 贴安全线后再次保证右片 >= MIN_SPLIT
        let right_after = eff_end.saturating_add(1).saturating_sub(split_point);
        if right_after < MIN_SPLIT_SIZE {
            return Ok(false);
        }
        if split_point <= done_abs || split_point > eff_end {
            return Ok(false);
        }

        let new_index = self.fragments.len() as u32;
        let stolen = {
            let frag = &mut self.fragments[idx];
            match frag.try_split(split_point, new_index)? {
                Some(s) => s,
                None => return Ok(false),
            }
        };

        let spec: FragmentSpec = (
            stolen.info.index,
            stolen.info.start,
            stolen.info.end,
            stolen.resume_offset,
            stolen.info.hash.is_some(),
            FragmentShared {
                effective_end: Arc::clone(&stolen.effective_end),
                realtime_downloaded: Arc::clone(&stolen.realtime_downloaded),
            },
        );

        // try_send:Full 时立即返回,避免堵死 execute_fragmented_download 主循环
        match frag_tx.try_send(spec) {
            Ok(()) => {
                debug!(
                    slow_index = idx,
                    new_index, split_point, remaining, "rebalance:对半拆分剩余最大片并重入队"
                );
                if let Some(m) = &self.metrics {
                    m.inc_rebalance();
                }
                self.fragments.push(stolen);
                self.last_rebalance_at = Some(Instant::now());
                Ok(true)
            }
            Err(_) => {
                // Full 或 Closed:回滚 split,下次 rebalance 再试
                self.fragments[idx].revert_split_after_failed_dispatch(&stolen);
                if let Some(m) = &self.metrics {
                    m.inc_rebalance_dropped();
                }
                Ok(false)
            }
        }
    }
    /// 审计 H2(200 fallback 运行时降级):服务器忽略 Range 返回 200 时,
    /// `download_range`/`download_range_stream` 返回 `RangeNotSupported`。
    /// `execute_fragmented_download` 在分片 worker 失败路径捕获此错误,
    /// 中止所有在途 task → drain 已完成结果(避免丢失进度)→ 重新规划为
    /// 覆盖整个文件的单分片 → 委托 `execute_full_download` 整块下载。
    ///
    /// 此降级路径比走 make_200_fallback_stream 截取每片请求区间更高效:
    /// 整块下载只传输 1×file_size,而非 N 片各自 fallback 的 ≈ S*N/2。
    ///
    /// 返回 `Some(())` 表示已捕获并降级处理(调用方应返回该结果),
    /// 返回 `None` 表示非 RangeNotSupported 错误(调用方按原路径返回错误)。
    async fn try_range_not_supported_fallback(
        &mut self,
        error: &DownloadError,
        handles: &mut JoinSet<FragmentTaskResult>,
        completed_rx: &mut mpsc::UnboundedReceiver<FragmentTaskResult>,
    ) -> Option<DownloadResult<()>> {
        if !matches!(error, DownloadError::RangeNotSupported) {
            return None;
        }
        warn!(
            url = %tachyon_core::redact_url_for_log(&self.url),
            "服务器不支持 Range 请求,降级为整块下载(execute_full_download)"
        );
        // 审计 batch2:持久化 supports_range=false,避免 resume 再次走分片路径
        if let Some(meta) = self.metadata.as_mut() {
            meta.supports_range = false;
        }
        // 中止所有在途分片任务 + drain 已完成结果(进度对齐)
        Self::abort_remaining_fragment_tasks(handles).await;
        if let Err(e) = Self::drain_completed_channel(self, completed_rx) {
            return Some(Err(e));
        }
        // 重新规划为单分片覆盖整个文件:
        // 原 multi-fragment 规划基于 supports_range=true 的假设,已失效。
        // 改用单分片 [0, file_size-1] 让 execute_full_download_once 的
        // first_mut().complete_download_fast(pos, ...) 状态机正确转换,
        // 且 verify()/snapshot 的分片总数与实际写入一致。
        let file_size = self
            .metadata
            .as_ref()
            .and_then(|m| m.file_size)
            .unwrap_or(0);
        let single = crate::fragment::plan_fragments(
            file_size,
            false, // supports_range=false 强制单分片路径
            None,
            &self.scheduler_config,
        )
        .map_err(|e| {
            warn!(error = %e, "重新规划单分片失败,继续用原 fragments 整块下载");
            e
        });
        if let Ok(frags) = single
            && !frags.is_empty()
        {
            self.fragments = frags
                .iter()
                .map(|info| FragmentRecord::new(info.clone(), self.config.max_retries))
                .collect();
            // 整块下载路径会从 Pending 走 start_download → complete_download_fast
            debug!(count = self.fragments.len(), "已重新规划为单分片覆盖整文件");
        }
        // 通知 app 层重规划:旧分片进度全部作废,必须清零 total_downloaded。
        // 否则 chunk_reader 会把降级前 partial 字节与整块重下字节双计
        // (UI 显示 > file_size 且 100% 仍在下)。completed_indices 空 =
        // 全量重下;total 以当前 fragments 为准(通常 1)。
        if let Some(tx) = &self.progress_tx {
            let total = self.fragments.len() as u32;
            if let Err(e) = tx.try_send(FragmentProgress::PlanComplete {
                total,
                completed_indices: Vec::new(),
                initial_concurrency: 1,
            }) {
                warn!(error = %e, "RangeNotSupported 重规划 PlanComplete 发送失败");
            }
        }
        // 重置存储分配,丢弃 execute_fragmented_download 期间部分写入的残留,
        // 避免 execute_full_download_once 写入与旧数据拼接产生损坏。
        if let Some(storage) = self.storage.as_ref() {
            let _ = storage.allocate(file_size).await;
        }
        Some(self.execute_full_download().await)
    }

    /// 聚合 goodput 采样间隔:窗口至少持续该时长才向调度器 emit
    const GOODPUT_EMIT_MIN: Duration = Duration::from_millis(200);

    /// 累计完成字节到任务级时间窗;窗口时长 >= GOODPUT_EMIT_MIN 时返回 goodput bps 并重置。
    fn note_goodput_bytes(&mut self, delta_bytes: u64) -> Option<u64> {
        if delta_bytes == 0 {
            return None;
        }
        let now = Instant::now();
        match self.goodput_window_start {
            None => {
                self.goodput_window_start = Some(now);
                self.goodput_window_bytes = delta_bytes;
                None
            }
            Some(start) => {
                self.goodput_window_bytes = self.goodput_window_bytes.saturating_add(delta_bytes);
                let elapsed = now.saturating_duration_since(start);
                if elapsed >= Self::GOODPUT_EMIT_MIN {
                    self.emit_goodput_window(now, start)
                } else {
                    None
                }
            }
        }
    }

    /// 冲刷未 emit 的窗口(任务结束/最后一片),避免短任务零样本。
    pub(super) fn flush_goodput_window(&mut self) -> Option<u64> {
        let start = self.goodput_window_start?;
        if self.goodput_window_bytes == 0 {
            return None;
        }
        let now = Instant::now();
        // 极短窗口用 GOODPUT_EMIT_MIN 作分母下界,避免瞬时 bps 爆炸
        let elapsed = now
            .saturating_duration_since(start)
            .max(Self::GOODPUT_EMIT_MIN);
        let secs = elapsed.as_secs_f64().max(1e-6);
        let bps = (self.goodput_window_bytes as f64 / secs) as u64;
        self.goodput_window_start = None;
        self.goodput_window_bytes = 0;
        (bps > 0).then_some(bps)
    }

    fn emit_goodput_window(&mut self, now: Instant, start: Instant) -> Option<u64> {
        let secs = now.saturating_duration_since(start).as_secs_f64().max(1e-6);
        let bps = (self.goodput_window_bytes as f64 / secs) as u64;
        self.goodput_window_start = Some(now);
        self.goodput_window_bytes = 0;
        (bps > 0).then_some(bps)
    }

    /// 审计 S-03:已知长度分片下载的终态结构/字节不变式入口。
    ///
    /// `file_size = None/0` 时跳过(未知长度不在本不变式范围)。
    pub(crate) fn validate_known_length_fragment_completion(
        fragments: &[crate::fragment::FragmentRecord],
        file_size: Option<u64>,
    ) -> DownloadResult<()> {
        let Some(n) = file_size.filter(|&s| s > 0) else {
            return Ok(());
        };
        // 额外要求每片 downloaded == size(字节终态)
        for frag in fragments {
            if frag.state == crate::fragment::FragmentState::Done
                && frag.info.downloaded != frag.info.size
            {
                return Err(DownloadError::Other(
                    format!(
                        "已知长度分片完成校验失败: 分片 {} downloaded {} != size {}",
                        frag.info.index, frag.info.downloaded, frag.info.size
                    )
                    .into(),
                ));
            }
        }
        assert_known_length_fragment_completion(fragments, n)
    }

    pub(super) fn record_completed_fragment(
        &mut self,
        index: u32,
        downloaded: u64,
        duration: Duration,
        computed_hash: Option<String>,
    ) -> DownloadResult<()> {
        let frag = &mut self.fragments[index as usize];
        let previous_downloaded = frag.info.downloaded;
        frag.complete_download_fast(downloaded, duration)?;
        frag.computed_hash = computed_hash;

        if let Some(ref m) = self.metrics {
            m.add_bytes(downloaded.saturating_sub(previous_downloaded));
        }

        // 任务级聚合 goodput:多并发分片吞吐叠加到共享时间窗,再反馈调度器。
        // 避免单片完成速率噪声主导 EWMA;限速器仍不随实测带宽下调。
        let delta = downloaded.saturating_sub(previous_downloaded);
        if delta > 0
            && let Some(bps) = self.note_goodput_bytes(delta)
        {
            self.scheduler.observe_bandwidth(bps);
            debug!(
                index = index,
                bytes_per_sec = bps,
                delta_bytes = delta,
                "聚合 goodput 已反馈给调度器"
            );
        }
        Ok(())
    }

    fn drain_completed_channel(
        &mut self,
        completed_rx: &mut mpsc::UnboundedReceiver<FragmentTaskResult>,
    ) -> DownloadResult<()> {
        while let Ok(result) = completed_rx.try_recv() {
            match result {
                Ok((0, 0, _, _)) => continue,
                Ok((index, downloaded, duration, computed_hash)) => {
                    self.record_completed_fragment(index, downloaded, duration, computed_hash)?;
                }
                // 错误已在触发 abort 的路径上处理,忽略队列中的滞后错误
                Err(_) => {}
            }
        }
        Ok(())
    }

    async fn abort_remaining_fragment_tasks(handles: &mut JoinSet<FragmentTaskResult>) {
        handles.abort_all();
        while let Some(joined) = handles.join_next().await {
            if let Err(error) = joined
                && !error.is_cancelled()
            {
                warn!(error = %error, "分片任务 abort 后异常结束");
            }
        }
    }

    /// 把一个 batch 完整写入存储(含短写重试 + 控制信号中断)
    ///
    /// 入口处 `batch.freeze()` 转为 `Bytes`(零拷贝,Arc 引用计数 +1),循环内用
    /// `storage.write_at(pos, remaining.clone())` 写入。相比旧 `write_at_mut` 路径:
    /// - 消除后端 `Bytes::copy_from_slice` 的 256KiB 全量 memcpy(write_at 后端直接
    ///   move owned `Bytes` 进 `spawn_blocking`,Arc refcount 保证 select! 取消安全)
    /// - 消除 `advance(written.min(batch.len()))` 的 min hack(Bytes::slice 天然处理剩余)
    /// - `Bytes::clone()`/`slice()` 均为零拷贝指针调整,无内存复制
    ///
    /// 接受 `BytesMut` 的版本:仅测试使用(测试构造 `BytesMut` 较 `Bytes` 方便),
    /// 内部 `freeze()`(零拷贝)后委托 [`write_all_at`]。
    #[cfg(test)]
    pub(super) async fn write_all_at_mut(
        storage: &StorageSet,
        pos: u64,
        batch: bytes::BytesMut,
        control_rx: &mut Option<watch::Receiver<TaskCommand>>,
        pause_timeout: Duration,
        metrics: Option<&Metrics>,
    ) -> DownloadResult<u64> {
        Self::write_all_at(
            storage,
            pos,
            batch.freeze(),
            control_rx,
            pause_timeout,
            metrics,
        )
        .await
    }

    /// 把已 owned 的 `Bytes` 完整写入存储(含短写重试 + 控制信号中断)
    ///
    /// 与 [`write_all_at_mut`] 的区别:直接接受 `Bytes`,省去调用方的
    /// `BytesMut::from(chunk)` 分配 + memcpy。大 chunk 直写路径(网络 chunk
    /// 本就是 owned `Bytes`)直接传入,消除 256KiB 的 `BytesMut::from` memcpy。
    ///
    /// `Bytes::clone()`/`slice()` 均为零拷贝指针调整(Arc refcount),无内存复制。
    /// 入口经 `ensure_aligned_bytes`:未对齐则拷入 AlignedBuf 并计 `aligned_write_copied`,
    /// 已对齐零拷贝并计 `aligned_write_passthrough`。
    pub(super) async fn write_all_at(
        storage: &StorageSet,
        mut pos: u64,
        mut remaining: bytes::Bytes,
        control_rx: &mut Option<watch::Receiver<TaskCommand>>,
        pause_timeout: Duration,
        metrics: Option<&Metrics>,
    ) -> DownloadResult<u64> {
        let mut total_written = 0u64;
        while !remaining.is_empty() {
            let (aligned, copied) =
                tachyon_io::ensure_aligned_bytes(remaining).map_err(DownloadError::Io)?;
            remaining = aligned;
            if let Some(m) = metrics {
                if copied {
                    m.inc_aligned_write_copied();
                } else {
                    m.inc_aligned_write_passthrough();
                }
            }
            let write = storage.write_at(pos, remaining.clone());
            let written = if let Some(rx) = control_rx.as_mut() {
                tokio::select! {
                    biased;
                    control = Self::watch_for_interrupt(rx, pause_timeout) => {
                        control?;
                        return Err(DownloadError::Other("控制信号异常结束".into()));
                    }
                    result = write => result?,
                }
            } else {
                write.await?
            };
            if written == 0 {
                return Err(DownloadError::Fragment(format!(
                    "存储短写未前进: offset={pos}, remaining={}",
                    remaining.len()
                )));
            }
            let written_u64 = u64::try_from(written)
                .map_err(|_| DownloadError::Fragment("存储写入长度溢出".into()))?;
            pos = pos.checked_add(written_u64).ok_or_else(|| {
                DownloadError::Fragment(format!(
                    "存储写入偏移溢出: offset={pos}, len={written_u64}"
                ))
            })?;
            total_written = total_written.checked_add(written_u64).ok_or_else(|| {
                DownloadError::Fragment(format!(
                    "存储写入总长度溢出: written={total_written}, len={written_u64}"
                ))
            })?;
            let advance = written.min(remaining.len());
            remaining = remaining.slice(advance..);
        }
        Ok(total_written)
    }

    /// 审计 H-01:按 effective_end 裁剪待写 batch,禁止 write_buf 越过 steal 边界。
    ///
    /// `end_inclusive` 为当前分片允许写入的最后字节偏移。返回 None 表示无可写字节
    /// (已越过边界);同时清空 `write_buf` 中的越界数据。
    pub(super) fn take_clamped_write_buf(
        pos: u64,
        end_inclusive: u64,
        write_buf: &mut AlignedBuf,
    ) -> Option<bytes::Bytes> {
        if write_buf.is_empty() {
            return None;
        }
        if pos > end_inclusive {
            write_buf.clear();
            return None;
        }
        let max = match end_inclusive
            .checked_sub(pos)
            .and_then(|d| d.checked_add(1))
        {
            Some(m) => m as usize,
            None => {
                write_buf.clear();
                return None;
            }
        };
        let batch = write_buf.split().freeze();
        if batch.len() <= max {
            Some(batch)
        } else {
            // 越界尾部丢弃:steal worker 负责 [end_inclusive+1, …]
            Some(batch.slice(..max))
        }
    }

    /// 刷写一个 batch 到存储,统一处理「流式哈希 update + 越界检查 + 写入 + 偏移推进 + 限速」。
    ///
    /// 消除 `download_single_fragment` 中大 chunk 直写 / 批量刷写 / 尾刷三段重复逻辑。
    /// 调用方负责进度上报(各路径的进度计数位置不同,留在调用点保持原有语义)。
    ///
    /// 返回 `(新偏移, 本次写入字节数)`。hash update 在写入前按字节序执行,
    /// 保证流式哈希顺序与文件字节顺序一致(双缓冲乱序落盘亦安全)。
    #[allow(clippy::too_many_arguments)]
    async fn flush_batch(
        storage: &StorageSet,
        pos: u64,
        batch: bytes::Bytes,
        hasher: &mut Option<Box<dyn tachyon_core::traits::StreamingHasher>>,
        frag_index: u32,
        total_written: u64,
        expected_len: u64,
        rate_limiter: &Option<Arc<RateLimiter>>,
        control_rx: &mut Option<watch::Receiver<TaskCommand>>,
        pause_timeout: Duration,
        skip_write: bool,
        metrics: Option<&Metrics>,
    ) -> DownloadResult<(u64, u64)> {
        // 流式哈希:在写入前按字节序更新(batch 内容此后不再变化)
        if let Some(h) = hasher {
            h.update(&batch);
        }
        let batch_len = u64::try_from(batch.len())
            .map_err(|_| DownloadError::Fragment("分片写入长度溢出".into()))?;
        let attempted_written = total_written.checked_add(batch_len).ok_or_else(|| {
            DownloadError::Fragment(format!(
                "分片写入长度溢出: index={frag_index}, written={total_written}, len={batch_len}"
            ))
        })?;
        if attempted_written > expected_len {
            return Err(DownloadError::Fragment(format!(
                "分片下载数据越界: index={frag_index}, 预期 {expected_len} 字节, 本次将写入 {attempted_written} 字节"
            )));
        }
        let w = if skip_write {
            // P2-4: 协议层(BT custom Storage)直接写入目标文件,
            // 引擎跳过 write_all_at(消除双存储写放大),仅推进偏移+进度
            u64::try_from(batch.len())
                .map_err(|_| DownloadError::Fragment("分片写入长度溢出".into()))?
        } else {
            Self::write_all_at(storage, pos, batch, control_rx, pause_timeout, metrics).await?
        };
        let new_pos = pos.checked_add(w).ok_or_else(|| {
            DownloadError::Fragment(format!(
                "分片写入偏移溢出: index={frag_index}, offset={pos}, len={w}"
            ))
        })?;
        // 实时令牌桶限速
        if let Some(limiter) = rate_limiter {
            limiter.acquire(w).await;
        }
        Ok((new_pos, w))
    }

    /// 发送增量进度事件(通道满或关闭时丢弃并记录,不阻塞下载)。
    fn report_progress(
        frag_index: u32,
        total_written: u64,
        progress_tx: &Option<tokio::sync::mpsc::Sender<FragmentProgress>>,
    ) {
        if let Some(tx) = progress_tx {
            match tx.try_send(FragmentProgress::Chunk {
                fragment_index: frag_index,
                completed: false,
                fragment_downloaded: total_written,
            }) {
                Ok(()) => {
                    tracing::trace!(idx = frag_index, bytes = total_written, "进度事件已发送");
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    // 通道满是设计内背压(try_send 可丢增量),高频 warn 会淹没日志
                    tracing::trace!(idx = frag_index, "增量进度事件丢弃(通道满)");
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    tracing::debug!(idx = frag_index, "进度通道已关闭,丢弃增量事件");
                }
            }
        }
    }

    /// mid-flight partial 进度的 durable 上报。
    ///
    /// - `EveryFragment`:每次有写入字节的 partial 前都 `storage.sync()`
    /// - `Loose`:数据 sync 已在写路径按字节水位处理;此处只上报进度,避免与 chunk 切分耦合
    ///
    /// 仅在 partial 上报点调用。
    async fn report_progress_durable(
        storage: &Arc<StorageSet>,
        skip_write: bool,
        sync_mode: tachyon_core::config::CrashConsistencyMode,
        frag_index: u32,
        total_written: u64,
        progress_tx: &Option<tokio::sync::mpsc::Sender<FragmentProgress>>,
    ) -> DownloadResult<()> {
        if !skip_write
            && total_written > 0
            && matches!(
                sync_mode,
                tachyon_core::config::CrashConsistencyMode::EveryFragment
            )
        {
            storage.sync().await?;
        }
        Self::report_progress(frag_index, total_written, progress_tx);
        Ok(())
    }

    /// Loose 模式:任务级累计写入字节跨过水位时 group-commit。
    ///
    /// 在实际 `flush_batch` 推进 `total_written` 后调用,使 sync 次数由写入量决定,
    /// 与网络 chunk 如何切分、partial 上报 countdown 无关。
    async fn maybe_loose_sync_on_written_bytes(
        storage: &Arc<StorageSet>,
        skip_write: bool,
        sync_mode: tachyon_core::config::CrashConsistencyMode,
        loose_partial_bytes: &Arc<std::sync::atomic::AtomicU64>,
        written_delta: u64,
    ) -> DownloadResult<()> {
        if skip_write
            || written_delta == 0
            || !matches!(sync_mode, tachyon_core::config::CrashConsistencyMode::Loose)
        {
            return Ok(());
        }
        let prev =
            loose_partial_bytes.fetch_add(written_delta, std::sync::atomic::Ordering::AcqRel);
        let new = prev.saturating_add(written_delta);
        let prev_marks = prev / LOOSE_PARTIAL_GROUP_COMMIT_BYTES;
        let new_marks = new / LOOSE_PARTIAL_GROUP_COMMIT_BYTES;
        if new_marks > prev_marks {
            storage.sync().await?;
        }
        Ok(())
    }

    /// 分片完成边界的 crash-consistency sync。
    ///
    /// - `EveryFragment`:每次完成都 `storage.sync()`
    /// - `Loose`:跨分片共享计数器每 `LOOSE_GROUP_COMMIT_N` 次完成同步一次
    /// - `skip_write`:协议托管存储,引擎不写盘,跳过
    async fn sync_on_fragment_complete(
        storage: &Arc<StorageSet>,
        skip_write: bool,
        sync_mode: tachyon_core::config::CrashConsistencyMode,
        loose_completed_frags: &Arc<std::sync::atomic::AtomicUsize>,
    ) -> DownloadResult<()> {
        if skip_write {
            return Ok(());
        }
        match sync_mode {
            tachyon_core::config::CrashConsistencyMode::EveryFragment => storage.sync().await,
            tachyon_core::config::CrashConsistencyMode::Loose => {
                // fetch_add 返回旧值;完成序号 = 旧值+1。每 N 次触发一次 group-commit。
                let prev = loose_completed_frags.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                let completed = prev + 1;
                if completed.is_multiple_of(LOOSE_GROUP_COMMIT_N) {
                    storage.sync().await
                } else {
                    Ok(())
                }
            }
        }
    }

    /// 下载单个分片(一次尝试)
    ///
    /// 由 `execute_fragmented_download` 的 spawn 重试循环调用。
    /// 成功返回 `(已写入字节数, 耗时)`;失败返回错误(由调用方决定是否重试)。
    /// 分片整体完成时通过 `progress_tx` 发送 `completed: true`,触发上层 checkpoint。
    #[allow(clippy::too_many_arguments)]
    async fn download_single_fragment(
        protocol: &Arc<dyn Protocol>,
        storage: &Arc<StorageSet>,
        pool: &Option<Arc<ConnectionPool>>,
        host: &str,
        url: &str,
        frag_index: u32,
        frag_start: u64,
        frag_end: u64,
        resume_offset: u64,
        pause_timeout: Duration,
        rate_limiter: Option<Arc<RateLimiter>>,
        control_rx: &Option<watch::Receiver<TaskCommand>>,
        progress_tx: &Option<tokio::sync::mpsc::Sender<FragmentProgress>>,
        verifier: &VerifierKind,
        compute_hash: bool,
        write_buf: &mut AlignedBuf,
        skip_write: bool,
        sync_mode: tachyon_core::config::CrashConsistencyMode,
        loose_completed_frags: &Arc<std::sync::atomic::AtomicUsize>,
        loose_partial_bytes: &Arc<std::sync::atomic::AtomicU64>,
        shared: &FragmentShared,
        object_identity: Option<ObjectIdentity>,
        metrics: Option<&Metrics>,
        range_window_bytes: Option<u64>,
    ) -> DownloadResult<(u64, Duration, Option<String>)> {
        let mut control_rx = control_rx.clone();

        // 真实 I/O 前检查暂停/取消
        if let Some(rx) = control_rx.as_mut() {
            Self::wait_control_rx(rx, pause_timeout).await?;
        }

        // 获取连接许可,持有到本次尝试结束(全局 + 单主机限流真实生效)
        let _pool_permit = match pool {
            Some(pool) => Some(pool.acquire(host).await?),
            None => None,
        };

        let start_instant = std::time::Instant::now();
        debug!(
            index = frag_index,
            start = frag_start,
            end = frag_end,
            resume_offset,
            "开始下载分片"
        );

        // 通知 app 层该分片开始下载(用于 ChunkMatrix 真实状态显示)
        // try_send 非阻塞:channel 满时丢弃,该分片短暂不显示 downloading,不影响正确性
        if let Some(tx) = progress_tx {
            let _ = tx.try_send(FragmentProgress::Started {
                fragment_index: frag_index,
            });
        }

        let actual_start = frag_start + resume_offset;
        // BUG-1 修复:读取 effective_end(try_split 可能已缩小)
        // 用它替代 frag_end 作为实际下载终止点,避免与 steal worker 并发写同一区域
        let current_effective_end = shared
            .effective_end
            .load(std::sync::atomic::Ordering::Acquire)
            .min(frag_end);

        let full_len = current_effective_end
            .checked_sub(frag_start)
            .and_then(|len| len.checked_add(1))
            .ok_or_else(|| {
                DownloadError::Fragment(format!(
                    "分片范围非法: {frag_start}..={current_effective_end}"
                ))
            })?;
        // expected_len 是 absolute 上限(相对 frag_start 的已写总量 total_written 的天花板)。
        // total_written 从 resume_offset 起算(含已续传字节);不得用 remaining 当上限,
        // 否则 resume>0 时 flush_batch 会误报“越界”(half+half > remaining)。
        let expected_len = full_len;
        let remaining0 = full_len.saturating_sub(resume_offset);
        if remaining0 == 0 {
            // 已续满:仍做完成边界 sync(与正常完成路径一致),再返回
            Self::sync_on_fragment_complete(storage, skip_write, sync_mode, loose_completed_frags)
                .await?;
            return Ok((full_len, Duration::ZERO, None));
        }
        let mut pos = actual_start;
        let mut total_written: u64 = resume_offset;
        // BUG-2 修复:初始化 realtime_downloaded 为 resume_offset(已持久化的字节)
        shared
            .realtime_downloaded
            .store(resume_offset, std::sync::atomic::Ordering::Release);
        // 控制通道/进度上报降频计数器，用递减替代 is_multiple_of 模运算
        let mut progress_report_countdown = PROGRESS_REPORT_CHUNK_INTERVAL;
        // write_buf 由调用方传入(跨分片复用),此处不再新建
        // 流式哈希:仅当分片有 expected hash 时计算,verify() 阶段无需重读文件。
        // 通过 Verifier trait 创建 StreamingHasher,支持 blake3/sha256/GPU 等后端切换。
        // 续传完整性:resume_offset>0 时禁止后缀流式哈希当整片 computed_hash。
        // verify() 在 computed_hash=None 时回退读盘计算完整 [start,size]。
        let mut hasher: Option<Box<dyn tachyon_core::traits::StreamingHasher>> =
            if compute_hash && resume_offset == 0 {
                Some(verifier.new_hasher())
            } else {
                None
            };

        // 片内窗口化 Range:代理下每次最多 range_window_bytes,直连 None=整片一次。
        // 外层按窗口推进 pos;内层消费单窗口 stream 直至 EOF/错误。
        'window_loop: loop {
            let current_end = shared
                .effective_end
                .load(std::sync::atomic::Ordering::Acquire)
                .min(frag_end);
            if pos > current_end {
                break 'window_loop;
            }
            let window_end = Self::range_window_end(pos, current_end, range_window_bytes);
            let window_requested_len = window_end.saturating_sub(pos).saturating_add(1);
            let mut window_received: u64 = 0;
            let stream = if let Some(rx) = control_rx.as_mut() {
                tokio::select! {
                    biased;
                    control = Self::watch_for_interrupt(rx, pause_timeout) => {
                        control?;
                        return Err(DownloadError::Other("控制信号异常结束".into()));
                    }
                    result = protocol.download_range_stream(
                        url,
                        pos,
                        window_end,
                        object_identity.clone(),
                    ) => result?,
                }
            } else {
                protocol
                    .download_range_stream(url, pos, window_end, object_identity.clone())
                    .await?
            };
            tokio::pin!(stream);
            loop {
                // 获取下一个 chunk:死 swarm 下(如磁力链接无 peer) stream.next() 永久 Pending,
                // 必须与 watch_for_interrupt 竞速,否则取消信号无法穿透(协作式取消检查点
                // 在循环体内,无 chunk 到达时不可达)。与 write_all_at 的 select! 同构。
                // cancel-safe:StreamExt::next 仅持有 &mut stream,被 select! 取消时无部分状态。
                let chunk_result = if let Some(rx) = control_rx.as_mut() {
                    tokio::select! {
                        biased;
                        interrupt = Self::watch_for_interrupt(rx, pause_timeout) => {
                            interrupt?;
                            return Err(DownloadError::Other("控制信号异常结束".into()));
                        }
                        chunk = tokio_stream::StreamExt::next(&mut stream) => match chunk {
                            Some(r) => r,
                            None => break, // EOF:正常退出循环
                        },
                    }
                } else {
                    match tokio_stream::StreamExt::next(&mut stream).await {
                        Some(r) => r,
                        None => break,
                    }
                };
                // 每 chunk 立即检查 Pause/Cancel(不挂起等 Resume)。
                // wait_control_rx 在 Pause 时会阻塞等 Resume,不适合热路径;
                // select! biased+interrupt 优先是主路径,此处兜底防 select 饿死。
                Self::check_control_interrupt(&mut control_rx)?;
                // 流错误(TLS EOF 等)前先刷 write_buf:否则已收未满批的字节只在内存,
                // 外层 resume 读 realtime_downloaded 仍是旧值,整片重下浪费 WAN 带宽。
                let chunk = match chunk_result {
                    Ok(c) => {
                        // 每 Range 请求体超长 fail-closed(规格 requested_len)。
                        // 在 effective_end 截断写入之前按原始 body 字节计数。
                        let next = window_received.saturating_add(c.len() as u64);
                        if next > window_requested_len {
                            return Err(DownloadError::Fragment(format!(
                                "分片窗口响应超长: index={frag_index}, requested={window_requested_len}, got={next}"
                            )));
                        }
                        window_received = next;
                        c
                    }
                    Err(e) => {
                        let tail_end = shared
                            .effective_end
                            .load(std::sync::atomic::Ordering::Acquire);
                        if let Some(batch) = Self::take_clamped_write_buf(pos, tail_end, write_buf)
                        {
                            // 尽力 flush;失败仍返回原始流错误(主因)
                            if let Ok((new_pos, w)) = Self::flush_batch(
                                storage,
                                pos,
                                batch,
                                &mut hasher,
                                frag_index,
                                total_written,
                                expected_len,
                                &rate_limiter,
                                &mut control_rx,
                                pause_timeout,
                                skip_write,
                                metrics,
                            )
                            .await
                            {
                                let _ = new_pos;
                                total_written = total_written.saturating_add(w);
                                shared
                                    .realtime_downloaded
                                    .store(total_written, std::sync::atomic::Ordering::Release);
                                let _ = total_written; // 已写入 realtime;本 attempt 随后 Err 返回
                            }
                        }
                        return Err(e);
                    }
                };
                // BUG-1 修复:检查 effective_end 是否被 try_split 缩小
                // 若 pos 已超过 effective_end,worker 的区域已被 steal,立即停止
                let current_end = shared
                    .effective_end
                    .load(std::sync::atomic::Ordering::Acquire);
                if pos > current_end {
                    break; // 已进入 steal 区域,停止下载
                }
                // 若 chunk 会跨越 effective_end,截断到 effective_end(避免写越界)
                let chunk = if pos + chunk.len() as u64 > current_end + 1 {
                    let truncate = (current_end + 1 - pos) as usize;
                    chunk.slice(..truncate)
                } else {
                    chunk
                };
                // 大 chunk:已 512 对齐则直写;未对齐则切块装入 write_buf 复用对齐内存
                // (freeze 后指针 512 对齐 → write_all_at passthrough,避免每块 ensure_aligned 拷贝)
                if chunk.len() >= WRITE_BATCH_BYTES {
                    // 先刷写 write_buf 中累积的残余数据(可能因小 chunk 累积未满阈值)
                    // 审计 H-01:按 effective_end 裁剪,避免 steal 后缓冲越界写
                    if let Some(batch) = Self::take_clamped_write_buf(pos, current_end, write_buf) {
                        let (new_pos, w) = Self::flush_batch(
                            storage,
                            pos,
                            batch,
                            &mut hasher,
                            frag_index,
                            total_written,
                            expected_len,
                            &rate_limiter,
                            &mut control_rx,
                            pause_timeout,
                            skip_write,
                            metrics,
                        )
                        .await?;
                        pos = new_pos;
                        total_written += w;
                        shared
                            .realtime_downloaded
                            .fetch_add(w, std::sync::atomic::Ordering::Release);
                        Self::maybe_loose_sync_on_written_bytes(
                            storage,
                            skip_write,
                            sync_mode,
                            loose_partial_bytes,
                            w,
                        )
                        .await?;
                    }
                    if pos > current_end {
                        break;
                    }
                    // write_buf 可能已推进 pos:重新按 current_end 裁剪大 chunk
                    let max_chunk = current_end.saturating_sub(pos).saturating_add(1) as usize;
                    if max_chunk == 0 {
                        break;
                    }
                    let chunk = if chunk.len() > max_chunk {
                        chunk.slice(..max_chunk)
                    } else {
                        chunk
                    };
                    let ptr_aligned = (chunk.as_ptr() as usize).is_multiple_of(512);
                    if ptr_aligned {
                        let (new_pos, w) = Self::flush_batch(
                            storage,
                            pos,
                            chunk,
                            &mut hasher,
                            frag_index,
                            total_written,
                            expected_len,
                            &rate_limiter,
                            &mut control_rx,
                            pause_timeout,
                            skip_write,
                            metrics,
                        )
                        .await?;
                        pos = new_pos;
                        total_written += w;
                        shared
                            .realtime_downloaded
                            .fetch_add(w, std::sync::atomic::Ordering::Release);
                        Self::maybe_loose_sync_on_written_bytes(
                            storage,
                            skip_write,
                            sync_mode,
                            loose_partial_bytes,
                            w,
                        )
                        .await?;
                    } else {
                        let mut rest = chunk;
                        while !rest.is_empty() {
                            if pos > current_end {
                                write_buf.clear();
                                break;
                            }
                            let space = WRITE_BATCH_BYTES.saturating_sub(write_buf.len());
                            let take = rest.len().min(space.max(1));
                            let piece = rest.slice(..take);
                            rest = rest.slice(take..);
                            write_buf.extend_from_slice(&piece);
                            if write_buf.len() >= WRITE_BATCH_BYTES {
                                if let Some(batch) =
                                    Self::take_clamped_write_buf(pos, current_end, write_buf)
                                {
                                    let (new_pos, w) = Self::flush_batch(
                                        storage,
                                        pos,
                                        batch,
                                        &mut hasher,
                                        frag_index,
                                        total_written,
                                        expected_len,
                                        &rate_limiter,
                                        &mut control_rx,
                                        pause_timeout,
                                        skip_write,
                                        metrics,
                                    )
                                    .await?;
                                    pos = new_pos;
                                    total_written += w;
                                    shared
                                        .realtime_downloaded
                                        .fetch_add(w, std::sync::atomic::Ordering::Release);
                                    Self::maybe_loose_sync_on_written_bytes(
                                        storage,
                                        skip_write,
                                        sync_mode,
                                        loose_partial_bytes,
                                        w,
                                    )
                                    .await?;
                                } else {
                                    break;
                                }
                            }
                        }
                    }
                    progress_report_countdown = progress_report_countdown.saturating_sub(1);
                    if progress_report_countdown == 0 {
                        Self::report_progress_durable(
                            storage,
                            skip_write,
                            sync_mode,
                            frag_index,
                            total_written,
                            progress_tx,
                        )
                        .await?;
                        progress_report_countdown = PROGRESS_REPORT_CHUNK_INTERVAL;
                    }
                    continue;
                }
                // 容量不足时先刷写已有数据(AlignedBuf 固定容量不自动扩容,与 BytesMut 不同)
                if !write_buf.is_empty() && write_buf.len() + chunk.len() > WRITE_BATCH_BYTES {
                    if let Some(batch) = Self::take_clamped_write_buf(pos, current_end, write_buf) {
                        let (new_pos, w) = Self::flush_batch(
                            storage,
                            pos,
                            batch,
                            &mut hasher,
                            frag_index,
                            total_written,
                            expected_len,
                            &rate_limiter,
                            &mut control_rx,
                            pause_timeout,
                            skip_write,
                            metrics,
                        )
                        .await?;
                        pos = new_pos;
                        total_written += w;
                        shared
                            .realtime_downloaded
                            .fetch_add(w, std::sync::atomic::Ordering::Release);
                        Self::maybe_loose_sync_on_written_bytes(
                            storage,
                            skip_write,
                            sync_mode,
                            loose_partial_bytes,
                            w,
                        )
                        .await?;
                    }
                    if pos > current_end {
                        break;
                    }
                }
                // 若当前 pos 已越过 steal 边界,丢弃本 chunk 并停止
                if pos > current_end {
                    write_buf.clear();
                    break;
                }
                // 再截断 chunk 到剩余允许写入长度(含已缓冲)
                let remaining_allowed = current_end
                    .saturating_sub(pos)
                    .saturating_add(1)
                    .saturating_sub(write_buf.len() as u64)
                    as usize;
                if remaining_allowed == 0 {
                    // write_buf 已占满允许区间,先 flush 再结束
                    if let Some(batch) = Self::take_clamped_write_buf(pos, current_end, write_buf) {
                        let (new_pos, w) = Self::flush_batch(
                            storage,
                            pos,
                            batch,
                            &mut hasher,
                            frag_index,
                            total_written,
                            expected_len,
                            &rate_limiter,
                            &mut control_rx,
                            pause_timeout,
                            skip_write,
                            metrics,
                        )
                        .await?;
                        pos = new_pos;
                        total_written += w;
                        shared
                            .realtime_downloaded
                            .fetch_add(w, std::sync::atomic::Ordering::Release);
                        Self::maybe_loose_sync_on_written_bytes(
                            storage,
                            skip_write,
                            sync_mode,
                            loose_partial_bytes,
                            w,
                        )
                        .await?;
                    }
                    break;
                }
                let chunk = if chunk.len() > remaining_allowed {
                    chunk.slice(..remaining_allowed)
                } else {
                    chunk
                };
                write_buf.extend_from_slice(&chunk);
                progress_report_countdown = progress_report_countdown.saturating_sub(1);
                // 达到阈值时批量刷写
                if write_buf.len() >= WRITE_BATCH_BYTES {
                    // split().freeze() 零拷贝:split_to 调整指针,freeze 转 Bytes(Arc inc)
                    if let Some(batch) = Self::take_clamped_write_buf(pos, current_end, write_buf) {
                        let (new_pos, w) = Self::flush_batch(
                            storage,
                            pos,
                            batch,
                            &mut hasher,
                            frag_index,
                            total_written,
                            expected_len,
                            &rate_limiter,
                            &mut control_rx,
                            pause_timeout,
                            skip_write,
                            metrics,
                        )
                        .await?;
                        pos = new_pos;
                        total_written += w;
                        shared
                            .realtime_downloaded
                            .fetch_add(w, std::sync::atomic::Ordering::Release);
                        Self::maybe_loose_sync_on_written_bytes(
                            storage,
                            skip_write,
                            sync_mode,
                            loose_partial_bytes,
                            w,
                        )
                        .await?;
                    }
                }
                // 进度上报检查:移到刷写块外,确保小 chunk 累积不满 WRITE_BATCH_BYTES 时
                // countdown 也能正常重置,避免 u64 下溢 panic
                if progress_report_countdown == 0 {
                    Self::report_progress_durable(
                        storage,
                        skip_write,
                        sync_mode,
                        frag_index,
                        total_written,
                        progress_tx,
                    )
                    .await?;
                    progress_report_countdown = PROGRESS_REPORT_CHUNK_INTERVAL;
                }
            } // end inner stream chunk loop
            // 窗口流 EOF:先刷 write_buf 残余,再决定是否开下一窗
            let tail_end = shared
                .effective_end
                .load(std::sync::atomic::Ordering::Acquire)
                .min(frag_end);
            if let Some(batch) = Self::take_clamped_write_buf(pos, tail_end, write_buf) {
                let (new_pos, w) = Self::flush_batch(
                    storage,
                    pos,
                    batch,
                    &mut hasher,
                    frag_index,
                    total_written,
                    expected_len,
                    &rate_limiter,
                    &mut control_rx,
                    pause_timeout,
                    skip_write,
                    metrics,
                )
                .await?;
                pos = new_pos;
                total_written += w;
                shared
                    .realtime_downloaded
                    .fetch_add(w, std::sync::atomic::Ordering::Release);
                Self::maybe_loose_sync_on_written_bytes(
                    storage,
                    skip_write,
                    sync_mode,
                    loose_partial_bytes,
                    w,
                )
                .await?;
            }
            // 窗口未读满且仍在有效边界内 → 对端提前 EOF,交外层重试(已 flush partial)。
            // 用 Network+unexpected eof 归类 soft-pressure:额外 retry budget、短 jitter、
            // reconnect spacing;纯 Fragment 字符串不会触发 is_connection_soft_pressure。
            if pos <= window_end && pos <= tail_end {
                return Err(DownloadError::Network(format!(
                    "分片窗口提前结束(unexpected eof): index={frag_index}, pos={pos}, window_end={window_end}"
                )));
            }
            // pos 已越过 window_end → 本窗完成,继续下一窗(或 frag 结束)
            if pos > tail_end {
                break 'window_loop;
            }
        } // end window_loop

        // 与原始 is_multiple_of 行为对齐:当 chunk 总数为 PROGRESS_REPORT_CHUNK_INTERVAL
        // 整数倍时,尾刷再发送一次进度事件(可能重复)。
        if progress_report_countdown == PROGRESS_REPORT_CHUNK_INTERVAL {
            Self::report_progress_durable(
                storage,
                skip_write,
                sync_mode,
                frag_index,
                total_written,
                progress_tx,
            )
            .await?;
        }

        let mut actual_written = total_written.saturating_sub(resume_offset);
        // BUG-1 修复:work-stealing 拆分后 effective_end 缩小,worker 提前停止,
        // 剩余预期长度需用 final effective_end 重新计算(非拆分时 = full_len - resume)
        let final_effective_end = shared
            .effective_end
            .load(std::sync::atomic::Ordering::Acquire);
        let effective_expected = if final_effective_end < current_effective_end {
            // 被拆分:重新计算剩余预期长度
            final_effective_end
                .checked_sub(frag_start)
                .and_then(|l| l.checked_add(1))
                .unwrap_or(full_len)
                .saturating_sub(resume_offset)
        } else {
            full_len.saturating_sub(resume_offset)
        };
        if actual_written < effective_expected {
            return Err(DownloadError::Fragment(format!(
                "分片下载数据不完整: index={frag_index}, 预期 {effective_expected} 字节, 实际写入 {actual_written} 字节"
            )));
        }
        // rebalance 竞态:在途 batch 可能越过新 effective_end 后才观察到拆分。
        // 越界区间由 steal worker 重下覆盖;原片按缩小后的边界计完成即可。
        if actual_written > effective_expected && final_effective_end < current_effective_end {
            debug!(
                index = frag_index,
                actual_written,
                effective_expected,
                final_effective_end,
                "rebalance 后原片越界写入,按 effective_end 钳制完成"
            );
            actual_written = effective_expected;
            // total_written 是 resume_offset 起的绝对已写;钳制后与缩小边界一致
            total_written = resume_offset.saturating_add(actual_written);
            shared
                .realtime_downloaded
                .store(total_written, std::sync::atomic::Ordering::Release);
        } else if actual_written != effective_expected {
            return Err(DownloadError::Fragment(format!(
                "分片下载数据不完整: index={frag_index}, 预期 {effective_expected} 字节, 实际写入 {actual_written} 字节"
            )));
        }

        let elapsed = start_instant.elapsed();

        // 审计 P0-3:在发送 completed 触发上层 snapshot 之前,先把本分片已写字节 durable sync。
        // skip_write(BT protocol_managed) 时引擎未写 storage,由协议层 storage/piece 语义负责落盘。
        // 不做每 batch fsync(避免 Flush Storm);仅在分片完成边界 group-commit。
        // CrashConsistencyMode::Loose(默认):每 LOOSE_GROUP_COMMIT_N 个完成分片 sync 一次。
        // CrashConsistencyMode::EveryFragment:每分片 fsync,断电后 resume 跳过已 sync 分片。
        Self::sync_on_fragment_complete(storage, skip_write, sync_mode, loose_completed_frags)
            .await?;

        // 分片整体完成回调:触发上层 checkpoint(断点续传落盘)
        if let Some(tx) = progress_tx
            && let Err(e) = tx
                .send(FragmentProgress::Chunk {
                    fragment_index: frag_index,
                    completed: true,
                    fragment_downloaded: total_written,
                })
                .await
        {
            warn!(index = frag_index, error = %e, "分片完成进度事件发送失败");
        }

        debug!(
            index = frag_index,
            written = total_written as usize,
            elapsed_ms = elapsed.as_millis(),
            "分片下载完成"
        );
        // 流式哈希结果:StreamingHasher::finalize 消耗 self 返回十六进制字符串
        let computed_hash = hasher.map(|h| h.finalize());
        Ok((total_written, elapsed, computed_hash))
    }
}

#[cfg(test)]
#[path = "download_executor_tests.rs"]
mod tests;
