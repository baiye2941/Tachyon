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

## 第二轮探针:并发度 scaling 与固定开销分析

### 实验设计

用一次性探针(不入库)测 max_concurrent_fragments = 1/2/4/8 下的 execute 时间,
每配置 20 次采样取中位数。4MiB / 4 分片 / 256KiB chunk / MemoryStorage。

### 结果

```
max_conc=1 median=1913us min=1690us
max_conc=2 median=1945us min=1696us
max_conc=4 median=1880us min=1629us
max_conc=8 median=1969us min=1851us
```

### 结论

**并发度从 1 到 8 几乎无差异**(1.9ms ± 0.1ms)。原因:
- mock 的 I/O 零延迟,4 分片的总工作量(4MiB memcpy)固定
- 内存带宽共享,并行不加速 memcpy
- effective_concurrency = min(max_conc, fragment_count),max_conc=8 实际只 spawn 4 worker

**固定开销 ~1.9ms 不可压缩**:4MiB 内存拷贝(~200µs @ 20GB/s)+ spawn 调度 +
状态机 + channel 通信。write_all_at 纯逻辑开销 4.6µs/op(NoopStorage 隔离测量),
16 chunk × 4.6µs = 74µs,占总时间 3%,非瓶颈。

### 已实施优化(第二轮)

**write_all_at 零拷贝直写**(commit 6f2abba):消除大 chunk 直写路径的
`BytesMut::from(chunk)` memcpy。e2e bench 从 3.06ms 降到 2.42ms,改善 ~20%
(p=0.00 统计显著,可重复)。此优化吃掉了"大 chunk 路径 memcpy"的最大红利,
剩余 2.4ms 是不可压缩的固定开销(spawn + 内存拷贝 + 状态机)。

### 后续优化方向

mock+memory bench 的 CPU 路径已优化到极限。真实性能提升必须转向:
1. **算法层**(动态 RTT、闭环并发控制,需真实网络验证)
2. **真实 I/O 路径**(IOCP 对齐写入、磁盘调度,需磁盘 bench)
3. **小 chunk 聚合路径**(真实 HTTP 16-64KiB chunk 的双 memcpy,需改 bench 用小 chunk)

## 第三轮:算法层三方向深度反思与实施

### 方向 3:bandwidth_based 钳制 — 不做(语义合理)

分析 `recommend()` 的 `bandwidth_based = bw × target_secs / frag_size`:高带宽场景
算出 48(1Gbps/64MB)。但最终有 `.min(max_concurrency)` 钳制——用户配置 max=8 时
48→8。高估已被 max_concurrency 限制。

语义上 bandwidth_based 是"分片并行加速大文件"(视角 2),非"TCP 管道充盈"(视角 1)。
多分片并行在真实下载中有效(更快完成大文件),高估被合理限制。**非 bug,不做。**

### 方向 2:MirrorProtocol 选源公式 — 已实施

**发现的缺陷**:旧加性公式 `inflight×10000 + (1-quality)×1000` 中,in_flight 权重
远大于 quality,导致 inflight=0 的慢源(score=1000)总优先于 inflight=1 的快源
(score=10000)。**负载均衡主导,快源不能多干**,与"快源多干"目标矛盾。

**修复**:改用乘性公式 `score = (in_flight + 1) / max(quality, ε)`(预期完成时间排序):
- 快源(quality 高)→ score 小 → 多被选(快源多干)
- 快源 in_flight 积累后 score 超过慢源 → 切换慢源(防过载)
- 冷启动(quality=0.5)→ 退化为 in_flight 排序(负载均衡,正确)

**验证**:35 个 mirror 测试全通过(含新增 test_multiplicative_scoring),旧 quality
感知测试无回归。

### 方向 1:闭环并发控制 — 不做(架构约束)

FastBioDL 闭环控制(测吞吐→步进并发)需运行时增减并发度。当前 execute 的
worker_count 在启动时固定(spawn N 个 worker task),Semaphore 只能 add_permits
(增)不能减已发出的许可。改成动态并发需重构 execute spawn 模型,风险高。

且 mock bench 带宽恒定,recommend 结果不变,无法验证闭环效果。**不做。**

`record_completed_fragment` 已调 `observe_bandwidth` 更新预测器,下次 plan(断点续传)
自然用新数据。闭环控制的真正价值在真实网络的带宽波动场景,需联网验证。

## 第四轮:磁力/HTTP 加速调研与瓶颈定位(bench+coverage+CRAP 交叉验证)

### 调研范围

针对"提升磁力链接和普通链接的下载与检测速度",执行了:
1. 多 Agent 并行搜索最新论文/博客(FastBioDL、MDTP、BEP-6/9/11)
2. 交叉验证 4 个关键假设(handle_cache 非 LRU、P2SP 预取未实现、双存储写放大、HTTP probe 无缓存)
3. 评估自研 BT 引擎可行性(15-19 人月,4 痛点均非只能靠自研解决,**决定不自研**)
4. 评估 librqbit 自定义 Storage 消除双存储(可行,需 cargo fetch 核实 4 项签名)
5. bench + llvm-cov + cargo crap 交叉验证定位真实瓶颈

### 交叉验证结论:4 个假设的最终裁决

| 假设 | 验证结果 | 裁决 |
|------|---------|------|
| handle_cache 非 LRU | 确认(magnet.rs:108 iter().next()),但 MAX=64 实际并发≤10 永不触发 | **放弃**(死代码路径) |
| P2SP 后台预取未实现 | 确认(downloader.rs:2415 同步 probe),但用户放弃预热优化方向 | **放弃** |
| 双存储写放大 | 确认(magnet.rs:1022 read+write),但有协议约束(piece 校验) | **记录**(librqbit 自定义 Storage 可解,后续方向) |
| HTTP probe 无缓存 | 确认,但 UI 不触发重复 probe(probe 在 run_inner 只调一次) | **放弃**(前提不成立) |

### 自研 BT 引擎评估

- 工程量:13,000-21,000 LOC,15-19 人月(含 DHT 3-4 人月)
- 4 个痛点(同步 storage/handle 生命周期/策略不可调/UDP-over-SOCKS5)均已有 librqbit 之上扩展方案或被架构规避
- 论文 cs/0609026 证明 rarest-first+choke 已近最优,替换收益有限
- Tachyon 是 AI 模型下载器,BT 是补充协议,15-19 人月应投入 HTTP/HF 主路径
- **结论:不自研,继续用 librqbit + 针对性扩展**

### bench 基线(第四轮,cargo clean 后重测)

| bench | 时间 | 说明 |
|-------|------|------|
| e2e_download(4MiB mock+memory) | 2.69-3.18ms | 固定开销不可压缩 |
| e2e save/load snapshot | 1.25-2.04ms | 状态持久化 |
| e2e fragment_state_machine | 5.87-12.26ms | 分片状态机 |
| e2e bandwidth_sampling | 29.5-375ms | 含延迟模拟 |
| scheduler_recommend | 23-25ns | 极快,非瓶颈 |
| scheduler_batch_pop/1024 | 75µs | 批量调度 |
| hex_encode/4096 | 3.6µs | hex 编码 |

### 覆盖率与 CRAP 交叉验证(核心发现)

| 文件 | 覆盖率(regions) | CRAP 最高函数 | 风险 |
|------|----------------|--------------|------|
| **http.rs** | **69.09%** | `download_range` CC=12 CRAP=156 **0%覆盖** | 极高 |
| **http.rs** | | `probe` CC=8 CRAP=72 **0%覆盖** | 极高 |
| **bt_session.rs** | **0%** | `build_session_options` CC=12 CRAP=156 | 极高 |
| magnet.rs | 84.85% | `add_magnet_to_session` 0%覆盖 | 高 |
| downloader.rs | 86.30% | `execute_fragmented_download` CC=53 CRAP=70 81.8% | 中 |
| mirror.rs | 91.79% | — | 低 |

**关键结论**:http.rs 的核心下载路径(probe/download_range/download_range_stream/download_full)
全部 0% 单元测试覆盖(仅通过 MockProtocol 在 engine 层间接测);bt_session.rs 0% 覆盖。
CRAP 高分(156)正是这些未覆盖的核心路径。**没有测试保护,任何性能优化都是盲改。**

### 优化方向决策(数据驱动)

当前最高 ROI **不是性能优化,而是补齐 HTTP/BT 协议层测试覆盖**:
1. AGENTS.md 要求 ≥90% 覆盖率,http.rs(69%)、bt_session.rs(0%) 严重不达标
2. CRAP 高分函数(download_range CRAP=156)正是未覆盖的核心下载路径
3. 补测试后才能安全实施性能优化(librqbit 自定义 Storage、HTTP probe 优化等)
4. 协议层测试需引入 mock HTTP server(wiremock/httpmock),当前 workspace 无此依赖

### 根因发现与修复:bt_session.rs 0% 覆盖率的真正原因

**根因**:tachyon-engine 的 `default` feature 仅含 `tachyon-protocol/magnet`(协议层 BT
支持),**不含 engine 自身的 `magnet` feature**(bt_session 模块门控)。

```
# 修复前(Cargo.toml)
default = ["tachyon-protocol/magnet"]  # 只开协议层,不开 engine 的 bt_session
magnet = ["tachyon-protocol/magnet", "dep:librqbit"]
```

这导致 `cargo test -p tachyon-engine` / `cargo llvm-cov -p tachyon-engine` 等单 crate
命令下 `#[cfg(feature = "magnet")] pub mod bt_session;` 不编译,bt_session.rs 的 11 个
测试被排除出 test binary,lcov 显示 0% 覆盖率。只有 tachyon-app(显式开
`tachyon-engine/magnet`)作为最终 binary 时 bt_session 才完整。

CI 的 `cargo nextest run --all` 和 `cargo llvm-cov -p tachyon-engine` 均未传
`--features magnet`,所以 CI 一直在"跳过 bt_session 测试"的状态下运行覆盖率门禁。

**修复**:让 `default` 含 `magnet`,使单 crate 命令自动包含 bt_session:

```
# 修复后(Cargo.toml)
default = ["magnet"]  # 含 engine 自身 magnet,bt_session 自动编译
magnet = ["tachyon-protocol/magnet", "dep:librqbit"]
```

tachyon-app 用 `default-features = false` + `features = ["magnet"]` 显式控制,不受影响。

**效果**:
- bt_session.rs 覆盖率:0% → 92.35% regions
- tachyon-engine 整体:88.65% → 90.20% regions(跨过 90% 门禁)
- 测试数:236 → 250(+14,含 bt_session 11 个 + downloader magnet 路径 3 个)
- 1370 全量测试通过,clippy 零警告

**教训**:feature 门控的模块,其测试在单 crate 命令下可能被静默跳过。覆盖率门禁
必须确保门控 feature 在门禁命令中被开启,否则覆盖率数据是假性的(跳过的模块显示 0%,
而非"未编译")。`default` feature 应包含所有"生产环境必需"的 feature,使单 crate
命令的行为与最终 binary 一致。

## 第五轮:HTTP 加速技术调研与实施

### 调研结论(基于 reqwest 0.13 + tokio 官方文档)

7 个 HTTP 加速方向评估,排序后取最值得实施的 3 个:
1. **Range 分片 + 自适应 chunk size**(已有基础设施,收益天花板高)
2. **HTTP/2 流控窗口调优 + keepalive**(实现成本极低,纯 ClientBuilder 配置)
3. **多源并行下载**(长期护城河,工程量大,已有 MirrorProtocol 基础)

### 已有配置盘点(确认现状)

| 优化项 | 状态 | 位置 |
|--------|------|------|
| HTTP/2 流窗口(1MB/16MB) | ✅ 已配置 | http.rs:164-166 |
| HTTP/2 max_frame_size(1MB) | ✅ 已配置 | http.rs:168 |
| HTTP/2 PING 保活(30s) | ✅ 已配置 | http.rs:170 |
| TCP_NODELAY | ✅ 已配置 | http.rs:132 |
| pool_idle_timeout | ✅ 已配置 | http.rs:130 |
| tcp_keepalive | ✅ 已配置 | http.rs:131 |
| DNS 自定义 resolver | ✅ 已配置 | http.rs:133 |
| **http2_keep_alive_while_idle** | ❌ **缺失** | http.rs:170 |
| buffer pooling(BufferPool) | ✅ 已配置 | downloader.rs:124 |
| write_all_at 零拷贝直写 | ✅ 已实施 | commit 6f2abba |

### 已实施:http2_keep_alive_while_idle

**问题**:此前 H2 keepalive 只在有活跃流时发 PING,空闲连接不发 PING。
多文件串行下载的文件间隙、P2SP 多源池中的空闲镜像源连接,在 NAT/代理超时后
会被静默掐断,下次使用需重建 TCP+TLS 握手(1-2 RTT)。

**修复**:开启 `http2_keep_alive_while_idle(true)`,空闲连接也发 PING 保活。

**收益**:多文件串行下载的文件间隙连接保持复用(省 1-2 RTT/文件);
P2SP 池中空闲镜像源连接同样受益。具体收益需真实网络验证(NAT 超时行为)。

**验证**:148 个 protocol 测试通过,clippy 零警告,新增
`test_build_client_http2_keepalive_config_succeeds` 验证配置正确。

## 第六轮:e2e_http_real bench 瓶颈定位与修正

### 背景

e2e_http_real bench 建立 4 轮 grill-me 审查后,发现两个关键 bench 测的不是产品路径,
无法定位真实瓶颈。本轮通过 4 个假设逐一验证,修正 bench 设计,首次暴露产品侧真实行为。

### 修正前的 bench 瓶颈归因

| Bench | 中位数 | 表面瓶颈 | 真实归因 |
|-------|--------|---------|---------|
| http_range_real/1MiB | 11-17ms | HTTP CPU 开销 | 真实 - reqwest 连接+解析,keep-alive ~1ms/请求 |
| throttled_download | 180-230ms | 带宽采样? | **bench 工具假象** - 16 chunk×6.25ms sleep 抖动;download_full 绕过 DownloadTask |
| rtt_effect/0ms | 8-13ms | RTT 基线 | 真实 - 1MiB loopback 无节流 |
| rtt_effect/50ms | 68-79ms | RTT 影响 | 真实 - 50ms+~10ms 噪声,与理论吻合 |
| mirror_aggregation | 70-99ms | 多源聚合? | **双重假象** - pool=None 每迭代重建 3 个 reqwest Client + 512KiB<1MB 强制单分片 |
| disk_io/memory | 5-8ms | 内存基线 | 真实 - 512KiB 完整 run() 路径 |
| disk_io/tokio_file | 6-15ms | 磁盘反压 | 真实 - 磁盘增量 ~5ms/512KiB |

### 4 个假设验证(全部确认)

1. **chunk+sleep 节流精度问题**:64KiB chunk @ 10MB/s -> chunk_delay=6.25ms,16 次 sleep
   累积抖动 80-130ms。改 256KiB chunk -> 4 次 sleep,抖动降为 1/4。
2. **mirror bench per-iteration 重建**:pool=None 时 with_mirrors 每源独立 build_http(),
   每迭代重建 3 个 reqwest Client(连接池/DNS/TLS 全丢弃)。
3. **小文件强制单分片**:min_fragment_size=1MB(config.rs:871),512KiB 文件 clamp 到 1MB,
   只产生 1 个分片,走 execute_full_download(无分片并发)。
4. **调度开销可忽略**:execute_fragmented_download 的 channel/spawn 按下载摊销(非按分片);
   download_via_least_in_flight 用 std::sync::Mutex,锁临界区微秒级;有回归测试守门 <1ms。

### 修正内容

1. **throttled_download**:download_full -> DownloadTask::run()(走 probe->plan->execute);
   chunk_size 64KiB -> 256KiB;文件 1MiB -> 2MiB(>1MB 触发分片)
2. **mirror_aggregation**:pool=None -> pool=Some(ConnectionPool);文件 512KiB -> 2MiB;
   带宽 50MB/s -> 20MB/s
3. **新增 large_file_fragmented**:4MiB/16MiB,无节流,走完整 run() 路径,对比 memory vs tokio_file

### 修正后 bench 数据(CI 模式)

| Bench | 改进前 | 改进后 | 关键发现 |
|-------|--------|--------|---------|
| throttled_download | 180-230ms | **122ms** | 分片并发突破单连接节流(2 分片各自 10MB/s,聚合 20MB/s) |
| mirror_aggregation | 70-99ms | **281ms** | 真实多源分片聚合(2MiB/4 分片/3 源,数据量 4 倍) |
| large_file_fragmented/memory | N/A | **22ms** | 4MiB/4 分片 loopback 全速,memory 无磁盘反压 |
| large_file_fragmented/tokio_file | N/A | **26ms** | 磁盘增量 ~4ms/4MiB(大文件磁盘占比下降) |

### 核心发现:分片并发突破单连接节流

throttled_download 改走 DownloadTask::run() 后,2MiB 文件 >1MB(min_fragment_size)
触发 2 分片,2 个分片各自建独立 HTTP 连接。服务端节流是**按连接**生效的,
2 个并发连接各自受 bytes_per_sec 节流,但聚合带宽 = 2 × 10MB/s = 20MB/s。
因此 2MiB / 20MB/s ≈ 100ms 即可完成,远低于单连接理论值 200ms。

这是正确行为--分片并行的收益。断言调整为上界(<=单连接理论 3 倍)而非下界。

### 产品代码路径分析结论

| 阶段 | 典型耗时 | 瓶颈性质 | 可优化? |
|------|----------|---------|--------|
| probe | 20-500ms | 网络RTT(已优化:首成功即返回) | 否 |
| init_storage | 1-10ms | 磁盘open | 微优化 |
| plan | <0.1ms | 纯CPU | 否(可忽略) |
| prepare_storage | 1-50ms | 磁盘fallocate | 微优化 |
| execute | 秒级 | 网络+磁盘I/O(>99%墙钟) | I/O层 |
| verify | 文件大小相关 | 磁盘+哈希 | 已用mmap_rayon |

**结论:产品侧 CPU/调度层不是瓶颈。真实瓶颈在 I/O 层(网络带宽+磁盘吞吐)。
分片并发是主要加速手段--N 个分片各自独立连接,聚合带宽 = N × 单连接带宽。**

