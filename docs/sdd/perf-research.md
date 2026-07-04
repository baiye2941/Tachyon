# 性能调研与优化路线图

## 调研背景

基于「baseline bench + 论文搜索 + 多 Agent 并行验证 + CRAP 分析 + 性能探针」
五维交叉验证,对 Tachyon 核心下载路径做系统性能审计,定位真实可落地的优化点。

调研日期:2026-07-05

## baseline bench 数据(CI 快速模式 TACHYON_BENCH_MODE=ci)

| bench 组 | 关键数据点 | 含义 |
|---|---|---|
| e2e_execute_download/4MiB_4frag_chunked_mock | 3.52ms | 核心热路径(probe→plan→execute,4MiB/4分片/256KiB chunk,mock+memory) |
| blake3/hash/16MB | 3.96ms | blake3 单核 ~4GB/s(接近 AVX2 理论值) |
| blake3_from_path/mmap_rayon/1MB | 219µs | rayon 并行比 in_memory(735µs)快 3.4x |
| blake3_from_path/mmap_rayon/16MB | 3.83ms | 16MB 时 rayon 开销抵消收益(与 in_memory 3.69ms 接近) |
| buffer_alloc_empty/64K | 1.45µs | 空池 fallback 分配(含 malloc+zero-fill) |
| compute_fragment_size | 3.3-6.5ns | 分片大小计算(可忽略) |
| scheduler/push_pop/1000 | 179ns | 调度器 push/pop(可忽略) |

## 论文搜索结论(arXiv + Stack Exchange + GitHub)

### 方向1: HTTP 并行下载与 Range 请求

**MDTP(arXiv 2505.09597)**:bin-packing 自适应分片,按各服务器性能分配不同大小
chunk,使每轮请求近似同时完成。对比 aria2 静态分片传输时间减少 10-22%。

**FastBioDL(arXiv 2508.05511)**:效用函数 + 梯度下降实时调整并发 socket 流数,
比 SOTA 快 4×(高速网络下 2.1×)。

**工程实践**:小文件走 keep-alive 串行优于并行;线程数超过核数会因上下文切换退化。

### 方向2: 分片调度与带宽自适应

**FastBioDL**:闭环并发控制(测吞吐→算效用梯度→步进并发)优于开环预测。

**Work-stealing(arXiv 1706.03539/1305.6474/1805.00857)**:有延迟时需考虑偷取成本,
低延迟场景偷取收益高,高延迟/非指数分布任务大小下 work-sharing 可能更优。

### 方向3: blake3 / 哈希并行校验

**BLAKE3(★6315)**:Merkle 树结构支持任意线程数 + SIMD 并行;`update()` 结合律
支持分块并行后归并;`update_mmap_rayon` 大文件 10-20× 加速。

**树模式优化(arXiv 1512.05864/1604.04206/1607.00307)**:并行哈希运行时间取决于
树拓扑;BLAKE3 的 2:1 树 + 1024B chunk 是这些理论的工程化。

## 多 Agent 交叉验证(关键发现)

### Explore Agent 的 Top 1 是伪优化

Explore Agent 报告「TokioFile write_at_mut 的 256KiB Bytes::copy_from_slice 是
热路径最大瓶颈」。**交叉验证证伪**:追踪实际调用链发现——

- `download_single_fragment` → `flush_batch` → `write_all_at_mut`(L1670)
- `write_all_at_mut` 参数是 owned `BytesMut`(非 `&mut`),内部 `batch.freeze()` 转
  owned `Bytes`,调用 `storage.write_at(pos, remaining.clone())`(非 `write_at_mut`)
- `StorageSet::write_at`(L474)Single 路径调 `TokioFile::write_at`(L80,无 copy)
- `TokioFile::write_at_mut`(L98,有 copy_from_slice)**仅在测试代码中被调用**

结论:热路径走 `write_at`(无 copy),`write_at_mut` 的 copy 是 C-01 UAF 修复的
防御性复制,且不在生产热路径。**此为伪优化,不可实施**。

### Explore Agent 的 Top 2/3 收益不足

- Top 2(Multi 跨段串行):仅多文件场景触发,bench 用 Single 无法验证;HDD 上
  跨段并发反而增加 seek 开销
- Top 3(http URL 重复 parse):单次 ~1µs,万级分片累积 ~10ms,相对下载耗时
  (秒-分钟级)是 0.01-0.1%,远低于 10% 收益门禁

## 性能探针实验

用一次性探针(crate 不入库)测量:
- 固定 4MiB/4分片,不同 chunk_size(64KB-1MB):execute 时间 2.0-2.6ms,波动在
  criterion 噪声范围内,chunk_size 对总时间影响不大
- 结论:**调度开销(spawn + 状态机 + channel)是固定成本 ~2ms**,per-chunk/per-byte
  开销不是主导

## CRAP 分析

高 CRAP 函数均为**低覆盖导致的测试盲区**,非性能瓶颈:
- iouring driver_task(CRAP 1806, CC42, Windows 不跑)
- chunk_reader_pool run_chunk_reader(CRAP 1056, CC32, app 层)
- task_service create_task(CRAP 812, CC28, app 层)

## 最终结论

**Tachyon 核心下载路径已经过深度优化**(P1-T*/OPT-*/B*/C-01 等大量历史优化),
在 mock+memory bench 环境下**没有可以安全证明 >10% 收益的 CPU 优化点**。

真实性能提升空间在算法层,需真实网络环境验证:

### 优化候选路线图(按优先级)

| # | 优化点 | 预期收益 | 复杂度 | 风险 | 验证方式 |
|---|---|---|---|---|---|
| 1 | **动态 RTT 探测**:替换硬编码 `ESTIMATED_RTT_SECS=0.050`,用 probe 阶段 TCP 握手+TTFB 估计真实 RTT | 高延迟链路分片/并发更准 | 低 | 低 | 真实网络 A/B 对比 |
| 2 | **闭环并发控制**(FastBioDL):测吞吐→算效用梯度→步进并发,替代开环 Holt+BDP | 动态网络吞吐 +20-50% | 中 | 中(震荡) | 真实网络 trace 回放 |
| 3 | **per-mirror 差异化分片**(MDTP bin-packing):多源场景按质量分配不同分片大小 | 多源异构带宽 -10-22% | 中 | 中 | 多源 mock + 真实镜像 |
| 4 | **分片哈希树归并**:BLAKE3 update 结合律,分片 computed_hash 归并成根哈希,免读盘 verify | 大文件 verify 从分钟级降到秒级 | 中-高 | 高(需服务端树哈希) | 需生态支持 |

### 不建议实施(已证伪或收益不足)

- GPU blake3:已主动移除(CPU rayon 20-40GB/s 超 PCIe 带宽)
- TokioFile write_at_mut copy_from_slice 消除:热路径不走此路径(伪优化)
- http.rs URL 预解析缓存:万级分片累积 ~10ms,相对下载耗时 0.01-0.1%
- RL-based 多源调度:复杂度极高,当前 least-in-flight 已足够
