# Spec Brief：残余并发正确性批次（H-03 / M-01 / M-02 / M-04 / M-05 / BT-17+）

## 范围（本批落地）

1. **H-03** `DownloadSupervisor::send_command` 返回 `control.send(...).is_ok()`；receiver 已关时不得伪成功。
2. **M-02** `MirrorProtocol::clear_selected` **不得** 把 `in_flight` 全清零；只清 probe/identity/stats 衰减与 soft circuit。在途计数仅由 `StatsStream` EOF/Err/Drop 调整。
3. **M-04** `ChunkReaderPool` dispatcher 不再对固定 `next_worker` 阻塞 `send`；对 worker 做 `try_send` 轮询，必要时 `select!` 任一可写，避免 busy worker HOL。
4. **M-05** pause 控制路径把 `DownloadTask.state` 设为 `Paused`（在 `wait_control_rx` / 引擎 pause 入口可见），使 pause-timeout 保持 Paused 契约可达；app 侧 timeout 语义对齐文档。
5. **M-01（最小）** Linux `driver_task` 的 `submit_and_wait` 包在 `tokio::task::block_in_place`，避免占死 cooperative worker；**非** 完整 eventfd/AsyncCancel 协议。
6. **BT-17+** `protocol_managed_storage` 分片全部 skip_write 读完后，若有 `bt_magnet`/`bt_fallback`，在标 Completed 前 `wait` torrent 完成（或 progress-watch），避免“stream 读完但 piece 未 have”错误完成。

## 非目标

- H-01 work-stealing 全量 ownership epoch（默认 work-stealing 关闭；本批不重开）
- H-02 完整 per-task command owner 重构（可在 H-03 后单独立项）
- openat2 / Authenticode / 代理 DNS 拦截矩阵
- 内核 `IORING_OP_ASYNC_CANCEL` + CQE drain

## TDD

strict：每项先 RED 测试再改生产代码。

## 成功证据

- 对应 nextest 过滤全绿
- clippy 相关 crate `-D warnings` 干净
