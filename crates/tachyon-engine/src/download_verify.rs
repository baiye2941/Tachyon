//! 校验逻辑
//!
//! 从 downloader.rs 拆分:verify() 方法。
//! 校验策略(Skip/BestEffort/Require) + 流式哈希比对 + 整文件哈希重算。
//! 断点续传恢复的分片无 computed_hash 时回退到读盘计算(I/O 放大消除)。

use super::*;

impl DownloadTask {
    // ----- 步骤 5: 校验 -----

    /// 校验已下载数据的完整性
    ///
    /// 根据配置的 `verify_strategy` 决定校验行为:
    /// - `Skip`: 完全跳过校验
    /// - `BestEffort`: 有 expected hash 时校验,无 hash 时跳过并记录 info 日志
    /// - `Require`: 必须有 expected hash 且校验通过,否则返回错误
    pub async fn verify(&mut self) -> DownloadResult<()> {
        // Skip 策略:直接跳过
        if self.config.verify_strategy == tachyon_core::config::VerifyStrategy::Skip {
            debug!(task_id = %self.id, "校验策略为 Skip,跳过校验");
            return Ok(());
        }

        // 兼容旧版 verify_checksum=false:视为 Skip
        if !self.config.verify_checksum {
            debug!(task_id = %self.id, "verify_checksum 已禁用,跳过校验");
            return Ok(());
        }

        self.state = DownloadState::Verifying;
        debug!(task_id = %self.id, "开始校验文件完整性");

        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| DownloadError::Config("存储未初始化".into()))?
            .clone();

        // 收集需要校验的分片(有 expected hash 的),并行计算/比对。
        // 流式哈希分片(有 computed_hash)无需读盘,直接比对;断点续传分片读盘计算。
        // 用 JoinSet + Semaphore(available_parallelism) 并发,任一失败短路 abort。
        let concurrency = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .max(1);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
        let mut has_expected_hash = false;
        let mut join_set: tokio::task::JoinSet<DownloadResult<(u32, String, String)>> =
            tokio::task::JoinSet::new();

        // P6:verify 读盘哈希循环需要取消检查点(大文件读盘持续数分钟,
        // 裸 while 循环下取消信号无法穿透)。将 control_rx clone 传入每个
        // spawn task,读盘循环每累计 VERIFY_CANCEL_CHECK_BYTES 字节已读数据
        // 与 watch_for_interrupt 竞速一次。按字节(而非迭代次数)度量,使检查点
        // 频率与 read_at 单次返回量解耦,对短读与大块读均保证一致的响应延迟。
        let verify_pause_timeout = Duration::from_secs(self.config.pause_timeout_secs);
        let verify_control_rx = self.control_rx.clone();

        for frag in &self.fragments {
            let Some(expected_hash) = frag.info.hash.clone() else {
                continue;
            };
            has_expected_hash = true;
            let index = frag.info.index;
            let computed = frag.computed_hash.clone();
            let start = frag.info.start;
            let size = frag.info.size;
            let storage = storage.clone();
            let permit_sem = semaphore.clone();
            let verifier = self.verifier.clone();
            let mut control_rx = verify_control_rx.clone();
            join_set.spawn(async move {
                let _permit = permit_sem.acquire().await;
                // 流式哈希优先:下载阶段已边写边算,直接比对,消除 I/O 放大。
                let computed = if let Some(h) = computed {
                    debug!(index, "使用流式哈希校验(无需重读文件)");
                    h
                } else {
                    debug!(index, "无流式哈希,回退读盘计算(断点续传分片)");
                    let chunk_size = VERIFY_HASH_CHUNK_SIZE;
                    let mut offset = start;
                    let end = start + size;
                    let mut buf = vec![0u8; chunk_size];
                    let mut hasher = verifier.new_hasher();
                    // P6:读盘循环每累计 N 字节已读数据插入一次取消检查点,与下载路径的
                    // chunk 循环 select! 同构(协作式取消依赖检查点可达)。
                    // 大文件读盘持续数分钟,无检查点时取消信号无法穿透。
                    // 按字节度量:read_at 返回量越大,累加越快、检查越频繁,与"已读数据量"
                    // 成正比,而非与"调用次数"成正比(后者对 1 字节短读会过度检查,对
                    // 8MiB 大块读则检查过疏)。
                    let mut bytes_read_since_check: u64 = 0;
                    while offset < end {
                        let read_len = ((end - offset).min(chunk_size as u64)) as usize;
                        let read = storage.read_at(offset, &mut buf[..read_len]).await?;
                        hasher.update(&buf[..read]);
                        offset += read as u64;
                        // 按已读字节降频检查:累计达阈值后检查一次中断信号并归零
                        bytes_read_since_check = bytes_read_since_check.saturating_add(read as u64);
                        if bytes_read_since_check >= VERIFY_CANCEL_CHECK_BYTES {
                            if let Some(rx) = control_rx.as_mut() {
                                Self::wait_control_rx(rx, verify_pause_timeout).await?;
                            }
                            bytes_read_since_check = 0;
                        }
                    }
                    hasher.finalize()
                };
                Ok((index, expected_hash, computed))
            });
        }

        // 收集结果:任一分片校验失败即 abort 其余并短路返回
        while let Some(res) = join_set.join_next().await {
            let (index, expected_hash, computed) =
                res.map_err(|e| DownloadError::Io(e.into()))??;
            if computed != expected_hash {
                warn!(index, expected = %expected_hash, actual = %computed, "分片校验失败");
                join_set.abort_all();
                self.state = DownloadState::Failed;
                return Err(DownloadError::ChecksumMismatch {
                    expected: expected_hash,
                    actual: computed,
                });
            }
            debug!(index, "分片校验通过");
        }

        // 任务级整文件校验(LFS oid 等):分片 hash 之外的可信来源。
        if let Some(expected) = self.expected_checksum.clone() {
            has_expected_hash = true;
            let file_size = self
                .metadata
                .as_ref()
                .and_then(|m| m.file_size)
                .or_else(|| {
                    let total: u64 = self.fragments.iter().map(|f| f.info.size).sum();
                    Some(total)
                })
                .unwrap_or(0);
            let chunk_size = VERIFY_HASH_CHUNK_SIZE;
            let mut offset = 0u64;
            let mut buf = vec![0u8; chunk_size];
            let mut hasher = self.verifier.new_hasher();
            let mut bytes_read_since_check: u64 = 0;
            let mut control_rx = self.control_rx.clone();
            let pause_timeout = Duration::from_secs(self.config.pause_timeout_secs);
            while offset < file_size {
                let read_len = ((file_size - offset).min(chunk_size as u64)) as usize;
                let read = storage.read_at(offset, &mut buf[..read_len]).await?;
                if read == 0 {
                    break;
                }
                hasher.update(&buf[..read]);
                offset += read as u64;
                bytes_read_since_check = bytes_read_since_check.saturating_add(read as u64);
                if bytes_read_since_check >= VERIFY_CANCEL_CHECK_BYTES {
                    if let Some(rx) = control_rx.as_mut() {
                        Self::wait_control_rx(rx, pause_timeout).await?;
                    }
                    bytes_read_since_check = 0;
                }
            }
            let computed = hasher.finalize();
            if computed != expected {
                warn!(expected = %expected, actual = %computed, "任务级整文件校验失败");
                self.state = DownloadState::Failed;
                return Err(DownloadError::ChecksumMismatch {
                    expected,
                    actual: computed,
                });
            }
            debug!(task_id = %self.id, "任务级整文件校验通过");
        }

        // Require 策略:必须有 expected hash
        if self.config.verify_strategy == tachyon_core::config::VerifyStrategy::Require
            && !has_expected_hash
        {
            self.state = DownloadState::Failed;
            return Err(DownloadError::NoExpectedChecksum);
        }

        // BestEffort 策略:无 expected hash 时跳过并记录日志
        if !has_expected_hash {
            debug!(task_id = %self.id, "无 expected hash,跳过校验(BestEffort 策略)");
        } else {
            debug!(task_id = %self.id, "文件完整性校验通过");
        }
        Ok(())
    }
}
