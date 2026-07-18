# Spec Brief：恢复快照的本地 extent 准入

## 背景

恢复路径目前只在 `DownloadTask::probe()` 中比较远端 `ObjectIdentity`。当远端对象未变化、
但本地目标文件在暂停后被删除或截断时，`plan()` 仍会把 snapshot 的 completed/partial
claims 转成 `Done`/`resume_offset`；随后 `prepare_storage()` 的 `allocate()` 会把实际短文件
扩回远端长度，从而掩盖丢失数据并可能跳过重新下载。

多文件任务不能根据 `TaskSnapshot.save_path` 或各文件长度总和判断：snapshot 不持久化
`FileLayout`，且一个子文件变短、另一个变长时总和可以恰好相等。

## 范围

1. 将恢复声明的本地准入归属到 `tachyon-engine::DownloadTask`：该对象同时拥有当前 probe 的
   metadata/layout、已经打开的 `StorageSet`，以及把 claims 转为 fragment 状态的唯一转换点。
   对公开分步调用，`prepare_for_plan().await` 是唯一的异步桥接入口：它执行/复用 probe、初始化
   storage 并完成 admission；随后同步 `plan()` 才能应用恢复声明。
2. 在 `init_storage()` 后、`plan()` 应用 claims 前、`prepare_storage()/allocate()` 前验证本地
   logical EOF：
   - Single：实际 EOF 必须精确等于当前远端总大小；
   - Multi：每个实际 storage 的 EOF 必须精确等于当前 `FileLayout` 对应文件长度（包括合法的
     零长度文件）；storage 数量、`file_id` 与连续全局 layout 必须一致；禁止用长度总和替代
     逐文件检查。
3. 恢复声明同时必须具有与 probe 后远端兼容的 `ObjectIdentity`；缺少 identity 的裸
   completed/partial claim 不得恢复。
4. 本地 extent 不匹配、无法读取、identity 缺失/不兼容、或 snapshot claim 结构不合法时：清空
   该任务全部 `completed_fragments` 与 `partial_fragments`，继续从零全量下载；不得让不可信
   claim 进入 `Done` 或 `resume_offset`。结构合法要求 completed 索引唯一且在当前计划范围内，
   partial 索引在范围内、`0 < offset < fragment.size`，且两类索引不重叠。
5. 公开的 `DownloadTask::plan()` 不得成为绕过 admission 的路径：未获准的 claims 必须在任何
   fragment 状态迁移前被丢弃；admission 仅供紧随其后的单次 `plan()` 消费。公开 staged 流程固定为
   `configure resume -> prepare_for_plan().await -> plan -> prepare_storage -> execute`；任何 setter
   在 preparation 后变更 claims 都必须重新 preparation。
   `prepare_for_plan()` 还必须为本次 completed/partial claims 捕获私有、瞬态的精确
   `{index,start,end,size}` 几何见证；最终 `plan()` 仅在当前被声明分片的几何及 partial bytes
   与见证完全一致时迁移 `Done`/`resume_offset`。这保护公开共享 `DownloadScheduler` 在
   preparation 与 plan 间改变 recommendation 的窗口；见证由单次 `plan()` 消费，不持久化。
6. `protocol_managed_storage` 继续采用现有 BT piece truth 策略，保持 snapshot fragment claims
   不参与跳过。
7. 测试至少覆盖：Single short/matching extent、Multi 每文件长度匹配、合法零长度文件、Multi
   总和相等但组件长度不匹配、identity 缺失、非法 claims、直接 `plan()` 绕过、调度器配置或共享
   scheduler recommendation 改变几何时整组拒绝，以及真实 `run()` 路径中的恢复 claims 被拒绝/保留。

## 不变量

- “可恢复”同时要求远端对象身份兼容（既有 P0-6）与本地存储 extent 对当前 metadata/layout
  精确匹配；仅长度匹配或仅 identity 匹配均不足。
- 检查必须发生在 `allocate()` 之前；不得让预分配改变作为准入证据。公开分步调用必须经由
  `prepare_for_plan().await`，不得绕过该约束。
- 失败关闭的是**恢复声明**，不是下载任务：不可信快照必须退化为全量下载，而非永久失败。
- app 层保持快照 transport，不根据 `save_path` 重建/复制多文件布局语义。

## 非目标

- 不证明同长度文件的内容未被篡改、稀疏洞已填充，或处理外部进程在 admission 后修改文件；
  内容级恢复验证需 fragment hash/layout fingerprint 的独立设计。
- 不改变 `TaskSnapshot` schema、app `TaskRunner` contract、ObjectIdentity 规则，且不新增或持久化
  scheduler fragment-plan fingerprint；本次仅允许 `DownloadTask` 内存内、一次性、仅覆盖已声明
  fragments 的精确几何见证。
- 不处理 metadata snapshot 保存与 progress checkpoint 的并发覆盖时序。
- 不扩展 BT protocol-managed storage 的 librqbit piece 校验策略。

## TDD

严格 RED → GREEN → REFACTOR：先在 `StorageSet` 写逐文件 extent（含零长度与 metadata I/O 失败）
RED 测试，再在 `DownloadTask` 写 identity、非法 claim、公开 `prepare_for_plan()`/`plan()` staged 路径
和真实 `run()` 均不得绕过 admission 的 RED 测试；每轮运行定向 nextest。

## 验证

```bash
cargo nextest run -p tachyon-engine -- storage_set_matches_resume_extent
cargo nextest run -p tachyon-engine -- resume_claims
cargo nextest run -p tachyon-engine -- prepare_for_plan
cargo nextest run -p tachyon-engine -- shared_scheduler
cargo clippy -p tachyon-engine --all-targets --all-features -- -D warnings
```
