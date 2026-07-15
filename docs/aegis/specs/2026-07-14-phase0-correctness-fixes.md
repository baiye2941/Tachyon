# Spec Brief：Phase 0 正确性修复（TDD）

- 日期：2026-07-14
- 基线：`5dd8bc7c37e0440c6ccc85aae8724ab9c6751a62`
- 状态：**待用户批准设计后进入 writing-plans / 实现**
- 用户范围决策：
  - 第一批：AlignedBuf 别名、Unix Statvfs FFI、retry `write_buf`
  - 策略：第一批后尽量一口气完成可本地证明的 Phase 0 其余项

## 1. 目标与非目标

### 目标

消除审计确认的、可本地用测试锁定的静默损坏 / UB / 错误完成路径，使后续性能优化建立在正确字节与安全内存契约上。

### 非目标

- 不做吞吐优化、不做竞品速度对比
- 不在本 spec 内完成 HLS 产品接线、公网/swarm 实验、发布签名
- work-stealing **默认保持关闭**；若修启用路径，只修正确性不宣称加速收益

## 2. 第一批（已选，严格 TDD）

### F1 — `AlignedBuf::split/freeze` 别名

**问题**：`split()` 共享同一 `Arc<AlignedAlloc>` 且不推进 `offset`，父缓冲可覆写 freeze 视图；池复用 + 取消后台写可放大为数据竞争。

**不变量（验收）**：

1. `split().freeze()` 得到的 `Bytes` 在父缓冲继续 `extend_from_slice` 后内容不变。
2. 生产路径仍能在 `WRITE_BATCH_BYTES` 边界批量刷写；对齐（512）保持。
3. 现有 split/freeze 对齐单测继续通过。

**推荐方案：A（经交叉复核修正）— 写前 Copy-on-Write（COW）**

```text
split():
  产出 [offset, offset+pos) 的前缀视图（共享 Arc，cap=pos）
  parent.offset / parent.cap 保持不变
  parent.pos = 0

任何安全可变入口（extend_from_slice / as_mut_ptr / as_mut_slice）:
  若 Arc::get_mut(&mut alloc) 成功：原分配独占，直接写
  否则：按原 align 分配 cap 大小的新 backing，仅复制 [offset, offset+pos) 已初始化前缀；
        self.offset=0，保留 pos/cap，然后写入新 backing
```

- 关键原因：`split()` 可处理任意 `pos`（包括 7 字节）；推进 offset 会破坏 512 对齐，缩小 cap 会耗尽批量复用容量。
- 优点：正常 `split().freeze() -> flush().await -> 下一次写` 中 frozen Bytes 已释放，仍零复制/零额外分配；仅存活共享视图时付出隔离成本。
- 额外紧耦合修复：`as_mut_slice() -> &mut [u8]` 必须在形成引用前初始化完整可见 cap（可仅零填 `[pos..cap)`），否则未初始化 `u8` 切片本身 UB。
- 备选 B：split/freeze 始终拷贝到新分配（最简单、恒安全、每批 256KiB memcpy）。
- 拒绝：仅让 BufferPool 在共享时不复用（不修 API 契约）；仅推进 offset（破坏任意长度 split 与复用）。

**TDD**：

1. RED：`test_split_freeze_not_aliased_with_parent_writes`、非扇区长度 split、以及非空 child 的 `as_mut_slice` 写入回归（当前均应失败）。
2. RED：断言 split 后 parent 保持完整 cap 和原对齐窗口（避免错误的 offset 推进方案）。
3. GREEN：方案 A COW；所有安全可变入口走 `Arc::get_mut` 唯一性门。
4. 回归：既有 alignment / continue writing 测试；补 `as_mut_slice` 安全初始化行为测试。

**文件**：`crates/tachyon-io/src/aligned_buf.rs`（可能 `buffer.rs` 若池契约变化）

### F2 — Unix `statvfs` 短结构 FFI

**问题**：手写 5 字段 `Statvfs` 作为完整 C 输出缓冲 → 栈越界写 / UB。

**不变量**：

1. 传给 `statvfs` 的缓冲大小 ≥ 平台 `libc::statvfs` / 完整 `struct statvfs`
2. `available_disk_space(temp)` 仍返回合理正值（既有测试）
3. 编译期或单测断言布局（`size_of` 与 libc 一致）

**推荐方案：A（推荐）— 依赖 `libc::statvfs`**

- `tachyon-engine` 在 `cfg(unix)` 增加 `libc` 依赖
- 删除手写短结构与手工 `extern "C"`
- SAFETY 注释说明指针有效与结构完整

- 备选 B：把磁盘空间探测下沉到已有 `libc` 的 `tachyon-io`（跨层职责变化更大）

**TDD**：

1. RED（unix）：`size_of` / 布局断言，或 canary 风格“完整结构调用不越界”的单元约束
2. 既有 `test_available_disk_space_*` 保持绿
3. Windows 路径不变（`GetDiskFreeSpaceExW`）

**文件**：`crates/tachyon-engine/src/storage_adapter.rs`、`crates/tachyon-engine/Cargo.toml`

### F3 — 分片 retry 残留 `write_buf`

**问题**：`write_buf` 在 retry loop 外分配，仅首次 `clear()`；中途 stream 错误后残留字节污染下一 attempt。

**不变量**：

1. 每次 attempt 开始时 `write_buf` 逻辑为空
2. “先产出 < WRITE_BATCH_BYTES 再 Network 错误，再成功” 的分片最终字节正确、无错位
3. 不改变成功路径批刷语义

**推荐方案：A（推荐）— attempt 入口 `clear()`**

在 `spawn_fragment_task` 的 retry loop 内、调用 `download_single_fragment` 之前：

```rust
write_buf.as_mut().clear();
```

错误路径也可 clear，但入口 clear 已足够。

- 备选 B：每次 attempt 重新 alloc（更重，无必要）

**TDD**：

1. RED：专用 MockProtocol：attempt1 推 64KiB 后 `Err(Network)`；attempt2 返回完整正确 range
2. 断言最终 storage 内容 == 期望，且无“旧缓冲前缀 + 新数据”错位
3. GREEN：入口 clear

**文件**：`crates/tachyon-engine/src/downloader.rs`（测试同文件 `#[cfg(test)]`）

## 3. Phase 0 后续切片（第一批后连续推进）

按依赖与风险排序：

| 序 | 项 | 策略要点 | 默认风险 |
|---|---|---|---|
| F4 | full-stream short write | `execute_full_download` 改 `write_all_at`；ShortWriteStorage 单测 | 中 |
| F5 | work-stealing 安全收敛 | 运行时 hard-disable：`true` 仅 warning + 静态分片；删除 steal 编排；保留配置/备份字段；完整 WorkUnit/Lease 重构延后 Phase0.5/1；**默认仍 false** | 高（危险路径可达） |
| F6 | 快照 ≤ sync 水位 | Engine 在 `completed:true` 前 `storage.sync`；零 schema；partial/BT 边界另记 | 高（持久契约） |
| F7 | BT block_on / 取消停 torrent | 去嵌套 block_on；cancel→pause/remove；定向单测 | 高 |
| F8 | HTTP 对象身份（If-Range/镜像） | 可单列为 Phase0.5；契约跨 protocol/app | 高 |

用户要求“尽量一口气 Phase0”：实现时仍 **按切片串行 TDD + 交叉验证**，不并行改同一 `downloader.rs` 冲突域。

## 4. 多 Agent 交叉验证协议

每个 GREEN 切片后：

1. **Tester Agent**：只读验证测试是否真失败过、是否断言行为而非实现细节
2. **Reviewer Agent**：对抗式检查是否假修复（例如只改注释、Windows-only、默认路径绕过）
3. **主审**：跑 `cargo nextest -p <crates>` + clippy 相关包；更新 `docs/aegis/work/.../90-evidence.md`

禁止：实现 Agent 与复核 Agent 共用未隔离的“已通过”口述。

## 5. TDD 路由

```text
TDD Route:
- Mode: auto
- Decision: strict
- Reason: 用户明确要求 TDD；行为/内存安全/数据正确性修复
- Verification: nextest 定向 + 包级；交叉验证；unsafe 需 Safety 注释
```

```text
Change Necessity:
- User-visible need: 消除静默损坏/UB/错误完成
- No-change option: 不可接受（审计已确认可达缺陷）
- Minimum change boundary: io AlignedBuf；engine statvfs；engine retry clear
- Decision: code-change
```

## 6. 复杂度与存在性

```text
Existence Check:
- Proposed new surface: 无新 crate；仅收紧既有 API 契约
- Decision: reuse-existing
```

```text
Complexity Budget:
- Artifact class: core library fix
- Target: aligned_buf.rs, storage_adapter.rs, downloader.rs
- Pressure: downloader.rs 已超大 → F3 保持最小 diff（单点 clear + 单测）
- Recommendation: edit-in-place for F1–F3; F5+ 考虑抽取 helper 避免继续膨胀
```

## 7. 验证命令（实现阶段）

```bash
# 单测过滤示例
cargo nextest run -p tachyon-io -- split
cargo nextest run -p tachyon-engine -- available_disk_space
cargo nextest run -p tachyon-engine -- retry

cargo clippy -p tachyon-io -p tachyon-engine --all-targets -- -D warnings
```

WSL 可追加 statvfs canary 复现脚本（审计目录已有）确认不再栈外写。

## 8. 批准门

在用户明确批准本 Spec Brief 之前：

- **不写生产代码**
- **不创建修复分支提交**（除非用户另行要求）

批准后终端状态：进入 `writing-plans`，为 F1→F2→F3 生成原子任务计划并开始 RED。
