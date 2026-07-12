# Tachyon 深度优化计划:P2-4 BT 自定义 Storage + P1-1 动态分片/Work-Stealing

日期:2026-07-12
依据:`Document/deep-review-2026-07-11.md` P1-1 / P2-4

## 已完成任务(本会话)

- ✅ P3-7 HTTP/3 编译启用(`.cargo/config.toml` + 验证)
- ✅ P1-2 HLS/DASH 核心实现(m3u8 解析 + Protocol trait + 15 tests)
- ✅ P2-5 闭环并发控制(`ConcurrencyController` 可升可降 + 11 tests + engine 集成)

## Plan Basis

本计划覆盖剩余两个高复杂度任务:P2-4(librqbit 自定义 Storage)、P1-1(动态分片/work-stealing)。

## BaselineUsageDraft

- Required baseline refs:`perf-research.md` 第四轮(Storage trait 评估)、
  `magnet-speedup-limitations.md`、`docs/sdd/magnet-dead-swarm-resilience.md`
- Cited in plan refs:`crates/tachyon-protocol/src/magnet.rs`、
  `crates/tachyon-engine/src/bt_session.rs`、`crates/tachyon-engine/src/fragment.rs`、
  `crates/tachyon-engine/src/downloader.rs`
- Decision:continue

## Requirement Ready Check

- Requirement source refs:deep-review-2026-07-11.md P1-1/P2-4
- Goals:P1-1 消除慢分片尾延迟(-20-40%);P2-4 消除磁力双存储写放大
- Acceptance:bench 证明收益 >10%(AGENTS.md 规则);覆盖率 >= 90%;零 clippy
- Decision:needs-clarification -- 需真实网络/BT 环境 bench 验证收益

## Change Necessity

- P1-1:静态分片在慢分片场景产生尾延迟。code-change 必要:execute 循环需运行时 split。
- P2-4:磁力下载走 FileStream 读 librqbit 已写 piece(双存储)。code-change 必要:
  实现 librqbit TorrentStorage trait 直接写 Tachyon Storage。
- Minimum change boundary:P1-1 -> fragment.rs + downloader.rs;P2-4 -> 新建 bt_storage.rs + bt_session.rs

---

## P2-4:librqbit 自定义 Storage(消除双存储写放大)

### 问题

`magnet.rs download_range_stream` 走 `FileStream` 读 librqbit 已下载的 piece(双存储:
librqbit 先写 piece 到 `download_dir`,Tachyon 再读出来写到目标路径)。

### librqbit TorrentStorage trait(API 已核实)

```rust
// librqbit-8.1.1/src/storage/mod.rs
pub trait TorrentStorage: Send + Sync {
    fn init(&mut self, shared: &ManagedTorrentShared, metadata: &TorrentMetadata) -> anyhow::Result<()>;
    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()>;
    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()>;
    fn remove_file(&self, file_id: usize, filename: &Path) -> anyhow::Result<()>;
    fn remove_directory_if_empty(&self, path: &Path) -> anyhow::Result<()>;
    fn ensure_file_length(&self, file_id: usize, length: u64) -> anyhow::Result<()>;
    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>>;
    fn on_piece_completed(&self, _piece_index: ValidPieceIndex) -> anyhow::Result<()> { Ok(()) }
}

pub trait StorageFactory: Send + Sync + Any {
    type Storage: TorrentStorage;
    fn create(&self, shared: &ManagedTorrentShared, metadata: &TorrentMetadata) -> anyhow::Result<Self::Storage>;
    fn clone_box(&self) -> BoxStorageFactory;
}
```

`SessionOptions.storage_factory: Option<BoxStorageFactory>` -- 注入点在 `bt_session.rs build_session_options`。

### 核心挑战:sync -> async 桥接

`TorrentStorage::pwrite_all/pread_exact` 是 **同步** 的(`-> anyhow::Result<()>`),
Tachyon 的 `AsyncStorage::write_at/read_at` 是 **异步** 的(`-> Pin<Box<Future>>`)。

桥接方案:
1. `tokio::task::block_in_place` + `Handle::current().block_on()` -- 阻塞当前 worker 线程,
   允许其他 task 运行。需多线程 runtime(`tokio::runtime::Handle::current()`)。
2. `pwrite_all` -> `block_in_place(|| handle.block_on(storage.write_at(...)))`
3. `pread_exact` -> `block_in_place(|| handle.block_on(storage.read_at(...)))`

风险:`block_in_place` 在单线程 runtime 会 panic。需文档说明需多线程 runtime。
Tachyon 默认用 `tokio::main`(多线程),风险可控。

### 文件映射

新建 `crates/tachyon-engine/src/bt_storage.rs`:
- `TachyonTorrentStorage`:实现 `TorrentStorage`,内部持 `Arc<DynStorage>` + `FileLayout`
- `TachyonStorageFactory`:实现 `StorageFactory`,创建上述 storage
- pwrite_all:按 FileLayout 把全局 offset 映射到各文件 -> write_at
- pread_exact:同理 -> read_at

修改 `crates/tachyon-engine/src/bt_session.rs`:
- `build_session_options`:可选注入 `TachyonStorageFactory`(需传入目标 storage)
- 需重构 `build_session_options` 签名,接受 `Option<Arc<DynStorage>>` 参数

### 验证

- 单元测试:mock `AsyncStorage`,验证 pwrite/pread 正确映射
- 集成测试:用预置 torrent(from_handle 接缝)+ TachyonStorageFactory,验证 piece 直接写入
- bench:对比 FileStream 路径 vs 自定义 Storage 路径的 I/O 吞吐(需真实 BT 下载)

### 风险

- `block_in_place` 在 current_thread runtime panic(Tachyon 默认多线程,风险低)
- librqbit 内部 piece 校验逻辑依赖 pread_exact 读回数据,需正确实现
- `take()` 方法需返回 dummy storage(用于 pause),需设计

---

## P1-1:动态分片 / Work-Stealing(IDM 式加速)

### 问题

`plan_fragments` 静态分片,`execute_fragmented_download` spawn-per-fragment 固定。
慢分片拖尾延迟,快分片完成后空闲 worker 无法"偷"慢分片剩余部分。

### 论文依据

MDTP(arXiv 2505.09597)bin-packing 自适应分片,对比 aria2 静态分片减少 10-22% 传输时间。
FluxDown "IDM-style dynamic segmentation"。

### 方案:运行时分片再分裂

1. **FragmentRecord split 扩展**:支持运行时将 Downloading 状态的分片一分为二
   - 原分片:start..mid(已下载部分保留,剩余由原 worker 继续)
   - 新分片:mid..end(空闲 worker 从 mid 开始下载)
2. **execute 循环监控**:快分片完成后,检查是否有慢分片(进度 < 平均 50%)
3. **split 触发**:慢分片剩余部分 > min_split_size 时,split 并 spawn 新 task

### FragmentRecord 状态机扩展

当前:Pending -> Downloading -> Verifying/Writing -> Done

新增:
- `try_split(&mut self, split_point: u64) -> Option<FragmentRecord>`:
  Downloading 状态分片在 split_point 处分裂,返回新分片(Downloading,mid..end)
- 原分片 end 更新为 split_point - 1
- 需处理 resume_offset、computed_hash 等字段

### execute_fragmented_download 改动

主循环 `select!` 新增分支:
```
// work-stealing:有空闲 permit 且存在慢分片时,split 并 spawn
_ = steal_timer.tick(), if has_slow_fragment && controller.should_spawn() => {
    let slow_idx = find_slowest_fragment();
    let split_point = calculate_split_point(slow_idx);
    if let Some(new_frag) = self.fragments[slow_idx].try_split(split_point) {
        // 将 new_frag 入队 frag_tx 或直接 spawn
    }
}
```

### 验证

- 单元测试:FragmentRecord::try_split 状态机正确性
- 集成测试:MockProtocol 模拟慢分片(分片 0 延迟 2s,分片 1-3 快速完成),
  验证 work-stealing 后总时间 < 静态分片
- bench:`e2e_download` bench 新增 `bench_dynamic_split_vs_static` 子项

### 风险

- **高**:execute 主循环重构,与 P2-5 ConcurrencyController 交互
- 断点续传需适配:split 后的 fragment 需持久化
- split 频率控制:避免频繁 split 导致请求开销(需 min_split_size 阈值)
- 慢分片检测算法:EWMA 进度 vs 实际进度,需调参

### 降级策略

若 bench 无 >10% 收益(AGENTS.md 规则),revert work-stealing,保留 FragmentRecord split
能力(未来扩展用)。

---

## 交叉验证检查清单

- [ ] cargo build --all 零警告
- [ ] cargo nextest run --all 全通过
- [ ] cargo clippy --all-targets --all-features -- -D warnings
- [ ] cargo fmt --all -- --check
- [ ] 覆盖率门禁(逐 crate >= 90%)
- [ ] bench 收益 >10%(P1-1/P2-4 需真实环境验证)
