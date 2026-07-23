# 证据包：BT Session 生命周期（2026-07-23 续）

## Baseline

- Worktree：`C:/Users/白夜/.config/aegis/worktrees/Tachyon/magnet-session-lifecycle`
- Branch：`feat/magnet-session-lifecycle`
- Baseline commit：`58bc93923fe93031330589c96afc22ac55c23a6c`

## 本轮新增严格 TDD 证据

### Slice 3A：cache-miss download acquisition helper

- Tester RED：`production_download_range_stream_registers_cache_miss_added_with_session_coordinator` compile RED（缺 `acquire_magnet_for_download_with_deadline` / `DownloadAcquisition`）。
- Coder GREEN：新增 `DownloadAcquisition { handle, cleanup }` + `acquire_magnet_for_download_with_deadline`（复用 probe helper），`download_range_stream` cache-miss 接 coordinator。
- 独立 Reviewer 发现两处回归 → 修复 → 回归测试 GREEN。

### 回归 1：并发 cache-miss fail-closed

- 根因：`begin_acquire` 对已存在 info-hash lane 返回 `ScopeRetiring`；多 worker 并发 cache-miss 时第二个确定性失败。
- 修复：`MagnetProtocol` 新增 `acquisition_gates: Arc<DashMap<String, Arc<AsyncMutex<()>>>>`；cache-miss 前获取 bind_key 门闩，首个 worker 完成 acquisition + cache insert 后释放，后续 worker 重检缓存命中。
- 回归测试：`concurrent_download_range_stream_cache_miss_does_not_fail_closed`（4 并发 worker，≥2 成功）。

### 回归 2：layout 失败 orphan registration

- 根因：acquisition 成功后 `drop(cleanup)`，若 `with_metadata` 构造 layout 失败只 `map_err` 返回，coordinator 仍持有 exact Added registration 但无 cleanup 请求。
- 修复：layout 失败路径调用 `coordinator.cleanup_action_for(info_hash).request_background_cleanup(deadline)`，与 probe `cleanup_probe_failure` 对称。

## GREEN 证据

```text
CARGO_TARGET_DIR=.../target-lifecycle cargo nextest run -p tachyon-protocol --features test-harness -E 'test(magnet_lifecycle_tests)'
20 passed, 230 skipped

CARGO_TARGET_DIR=.../target-lifecycle cargo nextest run -p tachyon-protocol --features test-harness
249 passed, 0 skipped

CARGO_TARGET_DIR=.../target-lifecycle cargo clippy -p tachyon-protocol --features test-harness --all-targets -- -D warnings
零警告
```

## 限定

- 仅 deterministic seam + real Added adapter seam + download_range_stream cache-miss 接线。
- **不是完整 Slice 3A 或 BT lifecycle closure。**
- 后续必须 RED：`download_full`/`download_full_stream` 接 coordinator、`stop_and_remove_torrent` 退休、engine/CLI/App 接线、AlreadyManaged、Quarantined、observer lazy capture（3B）、engine latch（3C）、CLI（3D）、App（3E）。
- 未运行任何公网下载、真实 swarm 测量或性能 benchmark。
