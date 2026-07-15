# F5 Design Addendum — Work-Stealing 安全收敛（Hard-Disable）

- 日期：2026-07-15
- 基线：`5dd8bc7c37e0440c6ccc85aae8724ab9c6751a62`
- 用户决策：Phase 0 采用 **安全收敛**（保留配置/备份兼容；`true` 仅 warning + 静态分片；完整重构转 Phase 0.5/1）
- 状态：bounded safety mitigation / feature repair deferred

## 1. 问题

当前 `enable_work_stealing=true` 路径存在可确认的 P0 正确性缺陷：

1. `try_split` 先缩短原分片并追加 tail，再 `steal_tx.try_send`；通道满时 tail 可无 worker。
2. `frag_rx` EOF 后 drop `completed_tx`，后续 steal spawn 可 `unwrap` panic。
3. 退出条件不检查 `steal_rx`、终端分片状态或字节覆盖。
4. 无 worker safe-cut handoff；原 worker 可能仍持有 buffer / in-flight 写。
5. `TaskSnapshot` 只持久化静态 index / partial bytes，动态 topology 崩溃后不可恢复，可形成永久 byte hole。
6. 现有集成测试使用 4 KiB 分片，低于 `MIN_SPLIT_SIZE=64 KiB` 门槛，不能证明发生过拆分。

完整可恢复架构需要 immutable WorkUnit + Lease、durable coverage manifest、schema migration，属于 Phase 0.5/1，不在本切片。

## 2. Phase 0 目标与非目标

### 目标

1. 任何配置值（含 `enableWorkStealing: true`）都不得进入动态拆分 / tail dispatch。
2. `DownloadTask` 始终执行初始静态分片 plan。
3. 保留公开 `DownloadConfig` / `DownloadPatch` / serde / backup 字段形状，旧配置与 v1 backup 可 round-trip。
4. `true` 时记录一次结构化 warning，明确 requested≠active。
5. 用真实 `DownloadTask::run()` 回归锁定“true 也不拆分、拓扑不变、字节正确”。

### 非目标

- 不实现正确的 work-stealing / 动态再分配
- 不新增 TaskSnapshot schema
- 不归一化或拒绝持久化 `true`（避免 config/backup 破坏与整配置回退）
- 不新增前端控件 / 状态 DTO / IPC 错误类型
- 不宣称任何加速收益
- 不在本切片删除公开 `enable_work_stealing` 字段或公开 `FragmentRecord::try_split` API

## 3. Canonical Owner

| 面 | Owner | 边界 |
|---|---|---|
| 运行时能力开关 | `tachyon-engine::DownloadTask` | `execute_fragmented_download` 不得从 bool 进入 steal timer / steal channel / `try_split` |
| 配置 schema | `tachyon-core::DownloadConfig` / `DownloadPatch` | 继续读写 `enable_work_stealing`，不在 validate 中拒绝 true |
| 备份 | `tachyon-app` Backup v1 | 原样携带 `AppConfig`，不改 schema version，不改写字段 |
| 休眠 API | `FragmentRecord::try_split` | crate 公开 re-export 暂保留；文档标明 DownloadTask 当前不调用 |

## 4. 推荐实现形状

```text
execute_fragmented_download():
  if config.enable_work_stealing {
    warn!(requested=true, active=false, "work-stealing 已请求但 Phase0 运行时硬禁用；使用静态分片")
  }
  // 删除:
  // - steal_timer / steal_interval
  // - steal_tx / steal_rx
  // - select! steal branches
  // - find_slowest_fragment / calculate_split_point 调用链
  // 保留静态 frag_rx dispatcher + completed_rx + reschedule_timer
```

配置字段与 backup 保持原样。`true` 与 `false` 走同一静态路径。

## 5. Anti-Entropy

```text
Anti-Entropy Declaration:
- Deletion Class: code-retirement + contract-carrying code (compat carrier only)
- Old Path/Object: DownloadTask dynamic split orchestration
- New Canonical Owner: static fragmented dispatcher only
- Expected Preserved Behavior: static multi-fragment download correctness; config/backup JSON shape
- Expected Retired Behavior: runtime dynamic split / steal queue / slow-fragment heuristics
- External Boundary Touched: yes (public DownloadConfig field remains)
- Source-of-Truth Data Risk: none (do not mutate persisted true values)
- User Confirmation Required: no for code retirement; persistent true values retained as-is

Retirement Decision:
- Path: delete-first for runtime orchestration; compat-exception for public field/serde/backup carrier
- Why: public field is already persisted in config.json and v1 backup; deleting now is source-breaking and silent semantic change without migration
- Non-edits: no TaskSnapshot schema; no frontend exposure; no reject-true validation
```

## 6. TDD 验收

### RED / GREEN 必须项

1. **真实运行时硬禁用**
   - `enable_work_stealing=true`
   - ≥3 个、每个 ≥256 KiB 的确定性 range 分片
   - 一个快速完成、一个持续慢速；必要时用 `start_paused` / 虚拟时间跨过原 steal 周期
   - 断言：终态 Completed；最终字节精确；`fragments.len()` 与初始 plan 相等；每个 index 的 start/end 与 plan 完全一致
2. **false 基线对照**：同一 fixture 在 false 下同样完成且拓扑不变
3. **配置兼容**：缺字段默认 false；显式 true 反序列化仍为 true；序列化输出 `enableWorkStealing`；`DownloadPatch` 可设置 true
4. **备份兼容**（若现有 fixture 易达）：导出/导入保留 true，schema version 不变
5. 替换旧 `test_work_stealing_split_produces_correct_data`（4 KiB 假阳性证据）为 hard-disable 负向回归

### 不做

- 不要求 GUI 可见 warning
- 不要求删除 `FragmentRecord::try_split` 单测（API 仍 dormant）
- 不在本切片改 recovery / snapshot schema

## 7. 完成语义

本切片完成后：

- Phase 0 F5 = **P0 安全缓解完成**
- 不是 work-stealing 功能修复
- 完整架构见独立 Design Brief：immutable WorkUnit/Lease + durable coverage manifest → Phase 0.5 契约，Phase 1 受控激活
- 重新启用前必须重新证明 ownership handoff、dispatch admission、completion coverage、crash-recoverable topology、hash 语义，且 bench 收益 >10%

## 8. 验证命令

```bash
rtk proxy cargo nextest run -p tachyon-engine --lib --locked --retries 0 work_stealing
rtk proxy cargo nextest run -p tachyon-core --lib --locked --retries 0 enable_work_stealing
rtk proxy cargo clippy -p tachyon-engine -p tachyon-core --all-targets --all-features --locked -- -D warnings
```
