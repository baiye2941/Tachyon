# 可重复真实吞吐基线

> 目的:任何优化前先拿到可重复证据,分解差距是**网络 / 磁盘 / 调度**。  
> 内部已证明 loopback 下磁盘/调度非主因;本基线补齐 WAN、大文件、多源与对标。

## 指标

| 指标 | 来源 | 含义 |
|------|------|------|
| goodput | `bytes / wall_time` | 有效吞吐(关闭 checksum 避免校验污染) |
| peak_active_requests | `ConnectionPool.active_requests` 20ms 采样峰值 | 连接/请求并发峰值 |
| aligned_write_passthrough / copied | `Metrics` | 写路径 512 对齐零拷贝命中 vs 拷贝 |
| aligned_write_hit_rate | pass/(pass+copy) | 对齐直写命中率 |
| rebalance_count | `Metrics.rebalance_count` | 慢片 rebalance 成功次数 |
| CPU% / 磁盘队列 | 外挂 typeperf / 资源监视器 / iostat | harness 不伪造 |

## 生产路径确认(BufferPool + 对齐)

已接线,无需再“猜是否注入”:

1. `AppState` 构造全局 `BufferPool`(容量=`max_tasks × max_fragments`,size=`WRITE_BATCH_BYTES`)
2. `download_supervisor` / `task_fn` 读锁 clone → `DownloadSession` → `build_download_task`
3. `DownloadTask::set_buffer_pool` → 分片 worker `alloc_guarded()` 拿 512 对齐 `AlignedBuf`
4. 大 chunk 直写仍经 `ensure_aligned_bytes`;未对齐记 `aligned_write_copied`
5. `update_config` 并发变更时热重建 BufferPool(A-14)

预期:池化路径 passthrough 应主导;若 copied 比例高,大 chunk 直写仍在拷贝,IOCP 真异步可能被 fallback 淹没。

## DEBUG 热路径日志降级(本轮已落地)

| 位置 | 原级别 | 现级别 |
|------|--------|--------|
| `AdaptiveDownloadScheduler::observe_bandwidth` | info | debug |
| 分片下载准备就绪 | info | debug |
| 闭环并发度调整 | info | debug |
| rebalance 拆分入队 | info | debug |
| 分片下载完成 | info | debug |
| progress-update emit | info | debug |
| chunk reader 进度更新 | info | debug |

`RUST_LOG=info` 下大文件不应再被每带宽样本/每分片完成的 info 淹没;更接近 Release 吞吐。

## 场景矩阵

| 场景 | 如何跑 | 说明 |
|------|--------|------|
| 本机 loopback 不限速 | 默认 | 磁盘/调度上界 |
| RTT 50/100/200ms | `--rtt-ms` | ThrottledServer 首字节延迟 |
| 百兆/千兆 cap | `--bps 12.5M` / `125M` | 应用层节流近似链路带宽 |
| 丢包 0/1/2% | 外挂 netem/clumsy | 见下 |
| 单源 CDN | `--url` | 真实 WAN |
| 双源 HF+mirror | `--url` + `--mirror` | 多源聚合 |
| 大文件 ≥512MB | `--size 512MiB` 或外部 URL | 对齐/ rebalance 更有意义 |
| aria2 对标 | `--compare-aria2` | 同机 `aria2c -x16 -s16` |

### Linux 丢包/延迟(需 root)

```bash
# 出口网卡替换 eth0
sudo tc qdisc add dev eth0 root netem delay 100ms loss 1%
# 跑基线后删除
sudo tc qdisc del dev eth0 root
```

### Windows

- 延迟/带宽:优先用 harness 的 `--rtt-ms` / `--bps`(loopback 可控)
- 真实 WAN 丢包:用 clumsy / 虚拟网卡 / 云 VM;脚本不做内核 netem

## 命令

```bash
# 单场景 smoke
cargo bench --bench throughput_baseline -- --size 32MiB --runs 1

# 高 RTT + 带宽 cap
cargo bench --bench throughput_baseline -- --size 64MiB --rtt-ms 100 --bps 50M --runs 3

# 外部源 + 镜像 + aria2
cargo bench --bench throughput_baseline -- \
  --url 'https://huggingface.co/.../file' \
  --mirror 'https://hf-mirror.com/.../file' \
  --compare-aria2 --runs 3 --out target/perf-baseline/hf.json

# 场景矩阵(Windows)
powershell -File scripts/perf/run_throughput_baseline.ps1 -Quick
powershell -File scripts/perf/run_throughput_baseline.ps1 -Size 512MiB -CompareAria2

# 场景矩阵(Unix)
bash scripts/perf/run_throughput_baseline.sh --quick
bash scripts/perf/run_throughput_baseline.sh --size 512MiB --compare-aria2
```

## 如何读结果

1. **median goodput** 与 `--bps` 上限比 → 是否打满链路
2. **aligned_hit_rate** → 是否 IOCP/WinFile 零拷贝路径在干活
3. **peak_active_requests** vs concurrency → 调度是否展开
4. **rebalance_count** → 慢片是否在拆(高 RTT/异构源更有意义)
5. **同机 aria2 ratio** → 差距在客户端栈还是网络本身

### 分解启发式(harness 也会打印)

- util(median/bps) > 95% → 网络/节流上限,磁盘/调度非主因
- peak_conn≈1 且 concurrency>1 → 并发未展开或 CDN 单连接限流
- hit_rate 低且 copy 主导 → BufferPool/大 chunk 对齐问题(loopback 可见,WAN 可能淹没)

## 对标说明

- **aria2**: `aria2c -x16 -s16`(与默认 concurrency 对齐);需 PATH 可执行
- **IDM**: GUI 工具,无稳定 CLI;人工同文件同盘对比 wall time 即可,不强制自动化

## 相关代码

- harness: `benches/throughput_baseline.rs`
- 编排: `scripts/perf/run_throughput_baseline.ps1` / `.sh`
- 指标: `crates/tachyon-core/src/utils/metrics.rs`
- 对齐写: `DownloadTask::write_all_at` + `ensure_aligned_bytes`
- BufferPool 注入: `tachyon-app` `InfraState.buffer_pool` → `task_fn` → `set_buffer_pool`

## 推荐真实环境基准方法论 — 可用性评估(2026-07-26)

> 结论:**可用,且应作为后续优化门禁的默认矩阵**;本仓库已覆盖大半,缺口明确。

### 与现有工具映射

| 方法论维度 | 已有能力 | 缺口 / 做法 |
|------------|----------|-------------|
| 网络 RTT 10/50/150/300ms | harness `--rtt-ms` + `ThrottledServer`;脚本已有 50/100/200 | 补 10/150/300 场景名即可;真实 WAN 仍靠外网 |
| 带宽 1M/10M/100M/1G | `--bps` 应用层节流 | 1Mbps=`125K`,10M=`1.25M`,100M=`12.5M`,1G=`125M` |
| 丢包 0/0.5/2/5% | **未内建** | Linux:`tc netem`;Windows:clumsy/云 VM;脚本已声明不伪造 |
| 源:CF / HF / GitHub / BT | `--url`/`--mirror`;BT 走产品路径 | HF 需 token 注入;BT 死/活 swarm 另开 e2e,不进本 bench |
| 磁盘 NVMe/SATA/HDD | 改 `--out`/download_dir 或 OS 盘符 | 需人工换挂载;JSON 记磁盘型号 |
| 对标 aria2 | `--compare-aria2` 同会话 | IDM:Windows GUI,建议外挂手工/半自动,不阻塞 harness |
| 指标 goodput/peak/aligned/rebalance | JSON 已有 | TLS EOF 次数、CPU%、IOPS、峰值内存 → **外挂采样**(typeperf/perfmon/iostat) |
| 可复现 JSON + 环境元信息 | `--out` JSON | 建议在脚本追加 `env.json`(OS/代理/磁盘/git SHA) |

### 推荐执行分层(避免被烂链路误导)

1. **L0 可控(必跑,门禁)**:loopback ± rtt/bps 矩阵 + `--max-frag-mib` 多连接 + aria2  
2. **L1 健康 WAN**:CF 64MiB c=1(已证 ~6–10× aria2);勿用 kernel/OVH 抖动当回归  
3. **L2 真 206 多 Range**:GitHub release / OVH,仅当代理到该源健康时比 c=1 vs c=2  
4. **L3 产品路径**:HF auth、BT swarm、混合源 — 独立 e2e,不与吞吐矩阵混判  

### 门禁判据建议

- L0:loopback goodput 不低于历史基线 80%;aligned ≥ 99%;无完成失败  
- L1:CF 64MiB c=1 goodput ≥ aria2 × 2(现约 8–10×,门限保守)  
- L2/L3:失败记环境元信息,不自动 fail CI  

### 不可用 / 慎用点

- 把 **代理 TLS EOF 主导** 的 kernel/OVH/GitHub 绝对 MB/s 当引擎上限  
- 在 CF `__down` 上强推多 Range 当多连接收益证明(源不配合)  
- 期望 harness 单独给出 CPU/IOPS/IDM 数字  

