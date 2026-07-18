# Spec Brief:7 项审计残留修复(F-15/F-09/P0-4/F-04/F-14/SEC-013/engine 覆盖率)

## 背景
审计 `Document/PI/Tachyon-Deep-Audit-2026-07-14` 识别的残留项,经探索确认 + 用户决策,7 项并行修复。

## 范围与用户决策

### F-15 父目录 sync
- 新建 `crates/tachyon-io/src/dir_sync.rs`,导出 `pub fn sync_parent_dir(path: &Path) -> io::Result<()>`
- 复用 `tachyon-store/src/store.rs:sync_directory` 逻辑(代码搬移,不跨 crate 依赖)
- 4 个后端 close 末尾调用:`tokio_file.rs`/`winio.rs`/`iocp.rs`/`iouring.rs`
- IoCpStorage 需确认有 path 字段(若无则补)
- 多文件 torrent 由 `StorageSet::Multi` 各 storage path 分别 sync

### F-09 StorageSet 边界检查
- 新增 `FileLayout::try_from_spans(spans) -> Result<Self, LayoutError>`(`types.rs`)
- 验证:file_id 从 0 连续、global_offset 从 0 连续无空洞无重叠、len 不溢出 offset+len
- `from_spans` 改为内部调 try 版并 `expect`(测试用),生产路径用 try 版
- 4 处 `storages[file_id]` 加 `debug_assert!(file_id < storages.len())`
- 新增 `LayoutError` 枚举(`FileIdGap`/`FileIdDuplicate`/`OffsetGap`/`OffsetOverlap`/`LenOverflow`)

### P0-4 work-stealing 硬关
- 删除 `downloader.rs` 中:`steal_timer`(2050-2058)、`steal_tx/steal_rx` channel(2059-2061)、steal timer select 分支(2088-2148)、steal_rx select 分支(2197-2238)
- 删除 `find_slowest_fragment`、`calculate_split_point` 辅助函数
- 保留 `enable_work_stealing` config 字段标 `#[deprecated]`
- 保留 `try_split`/`revert_split_after_failed_dispatch`(fragment.rs,纯逻辑)
- 删除 `test_work_stealing` 测试;保留 `test_try_split_*`(fragment 纯逻辑)

### F-04 io_uring 槽位回收
- `BufferIndexPool` 新增 `fn reset(&self)`:bitmap 全部置 0
- driver task 正常退出(Shutdown 命令)前调 `pool.reset()`
- IoUringStorage::Drop 中 abort 后 reset(双重保险)
- 异常退出(panic)仅 `tracing::error!` 记录,storage 进入 Unavailable 状态后续操作返回错误
- 新增 `IoUringState::Unavailable` 状态

### F-14 allocate 溢出 + rollback
- 新建 `crates/tachyon-io/src/alloc.rs`,封装 `fn allocate_windows(file: &File, size: u64) -> Result<()>`
- helper 内:`i64::try_from(size)` 溢出检查 + `set_len` 后 `SetFileInformationByHandle` 失败 rollback 到旧 `metadata().len()`
- tokio_file Windows / winio / iocp 的 allocate 改为调 helper(消除重复)
- tokio_file Linux 加 `libc::off_t::try_from(size)` 检查

### SEC-013 产物签名(A+C)
- A:Tauri updater ed25519
  - `tauri.conf.json` 加 `plugins.updater.endpoints` + `pubkey`
  - `release.yml` tauri-action step 加 `TAURI_SIGNING_PRIVATE_KEY` + `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` env
  - 本地 `tauri signer generate` 生成密钥对,私钥入 GH secret
- C:sigstore cosign keyless
  - `release.yml` publish-release job 加 `permissions: id-token: write`
  - 加 `sigstore/cosign-installer@v3` setup step
  - 加 `cosign sign-blob --yes --bundle <file>.bundle <file>` 循环签名所有产物
  - 用户验证:`cosign verify-blob --certificate-identity <workflow> --certificate-oidc-issuer https://token.actions.githubusercontent.com`

### engine 覆盖率回 90%+(S2+S3+S4+S6)
- S2:补 `bt_storage.rs` 测试(init/take/on_piece_completed/remove_*/pwrite_all 零进度/pread_exact EOF)
- S3:补 `http_client_registry.rs` 测试(conn=Some 的 from_parts/get_or_create)
- S4:补 `storage_adapter.rs` 测试(check_disk_space 磁盘不足错误分支)
- S6:删 `downloader.rs` 测试 mock 中无引用的死代码

## 不变量
- F-15:close 后父目录项已持久(Unix fsync;Windows NTFS 日志保证)
- F-09:FileLayout 构造后 file_id 连续 + offset 连续 + 无溢出;storages 索引不越界
- P0-4:无 work-stealing 代码路径;config 字段保留兼容
- F-04:driver 正常退出后 pool 可重新 alloc;异常退出后 storage 返回错误
- F-14:allocate size > i64::MAX 返回 InvalidInput;失败后文件大小不变
- SEC-013:产物有密码学签名(身份绑定);SHA256 保留作完整性补充
- engine:region 覆盖率 >= 90%

## 非目标
- F-15:不实现临时文件+原子 rename 发布协议(仅 close 时 sync 父目录)
- F-09:不验证 span 内容正确性(仅结构)
- P0-4:不修复 work-stealing(删除而非修复)
- F-04:不实现 driver panic-restart(仅 reset + Unavailable)
- F-14:不处理 macOS F_PREALLOCATE(仅 set_len)
- SEC-013:不做 EV 证书(付费);不做 GPG(体验差)
- engine:不补 execute 深分支(S5,易 flaky,后续迭代)

## TDD
严格 RED→GREEN→REFACTOR,垂直切片,每项一个测试一个实现。

## 验证
```bash
cargo nextest run --all
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo llvm-cov -p tachyon-engine --ignore-filename-regex "(test_harness|iocp|winio|iouring)" --fail-under-regions 90 --summary-only
```
