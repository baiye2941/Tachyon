# F4 设计补充：完整写入不变量

## 触发

F4 基础短写 RED/GREEN 后，独立对抗复核发现：

1. 已知长度整流下载只在 EOF 后发现 source 超界，已经写污染目标；
2. `AsyncStorage` 可返回超过输入长度的非法 count，当前 `DynStorage` 透传，`StorageSet::Multi` 可能 `Bytes::slice` panic，`write_all_at` 会虚增 offset/metrics；
3. `protocol_managed_storage` full path 确有遗漏，但用户重命名会改变 BT factory target identity，不能裸 skip-write。

## 决策

### F4（本切片）

**Canonical boundary：`DynStorage`。**

- `AsyncStorage::{write_at,write_at_mut}` 契约明确：`0 <= written <= offered_len`；
- `DynStorage` 在类型擦除入口验证上界，返回 `DownloadError::Fragment`；
- 因 `StorageSet::Multi` 的每个 child 都是 `DynStorage`，其内部 slice 不再面对非法 overreport；
- `write_all_at` 也显式拒绝超 remaining 的返回，不再使用 min clamp 掩盖契约错误；
- `execute_full_download` 在每个 chunk 写前统一计算 `attempted = pos + chunk_len`：known size 不得超过 expected，unknown size 不得超过 configured max。

### F4-R3（控制写入准入，用户已选择协作式语义）

F4-R3 RED 已确认：`run_inner()` 在 execute 阶段 `take()` 走 `self.control_rx`，使 full-stream、fragment worker 与 `write_all_at` 都看见 `None`；且 `write_all_at` 在短写补写的每轮开始前没有暂停准入检查。于是首短写同步发出 `Pause` 后，第二次逻辑 `StorageSet::write_at` 仍会在 `Resume` 前启动。

用户选择的契约是**协作式准入**，线性化点为每次新的逻辑 `StorageSet::write_at` 调用前：

- `Pause` 被该准入点观察到后，禁止启动新的逻辑写入，直到 `Resume`；
- `Cancel` 被该准入点观察到后，返回 `DownloadError::Cancelled`，不得启动后续逻辑写入；
- Pause/Cancel 到达前已通过准入、已被轮询或提交给底层的写允许完成。不得声称可撤销已提交的内核或 `spawn_blocking` I/O；
- `StorageSet::Multi` 内部的多个后端写属于已获准入的同一个逻辑写入，本切片不向 storage trait 下传新的控制通道；
- `write_all_at` 只负责 I/O 准入，不能成为 `DownloadTask.state` 的第二状态所有者。UI 可见 Pause 状态、final-EOF 与 Pause 的终态优先级，仍由后续控制状态机切片统一处理。

Canonical owner 与最小变更：

1. `DownloadTask.control_rx` 在 execute 期间保留在 task 上；外层 `run_inner` 仅用独立 clone 观察 Cancel，不能以 `take()` 剥夺 execute/worker 的控制 receiver；
2. `write_all_at` 在每轮短写补写创建 `storage.write_at` 前调用既有 `wait_control`，作为所有 full-stream、fragment flush 与 fallback 写入共享的准入门；
3. 保留现有 `watch_for_interrupt`，其职责是已开始写/死流的中断观察，不替代新写的准入门；若已获准入写的结果与控制信号在同一次 poll 同时就绪，active-I/O `select!` 使用 write-first `biased;`，允许该已获准入写完成。若写仍 Pending，控制分支仍可中断等待。

验收：

- 真实 `DownloadTask::run()` full-stream：首短写发 Pause 后，Resume 前第二次 write 不得开始；Resume 后精确完成；
- 直接 `execute()` 的同型路径也必须成立，证明不是仅修复 `run_inner` receiver 所有权；
- 直接 `execute()`：首短写的 Ready future 在 Drop 后发送 Cancel；Cancel 必须在下一轮准入被拒绝，第二次逻辑 write 不得启动。测试以 write-first active-I/O tie-break 使“删除准入门”的 mutant 稳定 RED；
- 既有 full-stream short-write、B11 stalled-stream cancel、fragment blocked-write cancel 回归保持通过；
- F4-R3 实现后由独立规格审查和对抗质量审查复核；不把 FileLayout/Multi 构造硬化或 protocol-managed BT ownership 混入本切片。

### F7（后续 BT ownership）

`protocol_managed_storage=true` 的 full path 应跳过二次 engine write，但仅当 BT factory target 与 engine final target 一致。用户 preferred filename 当前在 protocol probe 后覆盖 metadata，可能令 factory 写 torrent 名而 engine 认为目标是 preferred 名。F7 需选择并验证：

- factory 预先/延迟绑定最终 target，或
- rename 时降级为 engine-owned copy (`managed=false`)。

禁止仅在 full path 加无条件 skip-write。

## 验收

- known/unknown multi-chunk full stream + short writes：完整精确字节；
- known source over expected：越界 chunk **写前**拒绝；
- bad backend overreport：DynStorage、Multi write/write_mut 都 error 不 panic；
- trait/API 仅收紧既有实际写入 count 契约，无新 public owner；
- F7 target identity 的 regression 独立处理。
