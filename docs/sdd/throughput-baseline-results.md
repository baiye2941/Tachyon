### 45) 代码侧 P0:HEAD 不得单凭 Accept-Ranges 宣称 Range

#### Bug

`evaluate_head_response` / `metadata_from_headers(range_response=false)` 用 HEAD 的 `Accept-Ranges: bytes` 设 `supports_range=true`。  
GitHub/CDN/WAF 常见 **HEAD 200 + Accept-Ranges** 但实际 Range 返回 200/403 → 引擎多分片后整块回退。

GET Range 0-0 的 200 路径早已强制 `supports_range=false`;HEAD 不得更乐观。

#### Fix

1. HEAD 若声明 Accept-Ranges bytes → **NeedFallback** → 既有 `probe_via_get_range` 用 206 证实  
2. `metadata_from_headers(..., false)` **恒 supports_range=false**  
3. 单测:Accept-Ranges 强制 fallback;无 Accept-Ranges 的 HEAD 元数据 supports_range=false

#### 验证

`tachyon-protocol` evaluate_head/metadata 相关 **10/10 PASS**

---

### 44) 代码侧:set_scheduler_config 与 recommend 同源

#### Bug

`plan_fragments` 读 `DownloadTask.scheduler_config`,而 `recommend()` 读 `AdaptiveDownloadScheduler` 内部 `config`。  
`set_scheduler_config` 原先只写前者 → 测试/bench 改 max_fragment_size 时 **plan 与 recommend 分叉**(CF 多连接假失效的根因之一)。

生产路径 `create_adaptive_scheduler(config)` + `set_scheduler_config` 双写同源,风险较低;bench/单测路径易踩坑。

#### Fix

`set_scheduler_config`:在 `Arc::strong_count(scheduler)==1` 时用 `create_adaptive_scheduler` **重建**同源调度器;已共享则 warn 且只更新 plan 侧。

#### 验证

- `test_set_scheduler_config_syncs_plan_and_recommend_bounds` PASS
- `test_plan_fragments_clamps_suggested_size_to_max` PASS
- loopback 32MiB `--max-frag-mib 4 -c4` → peak=4 frags=16 goodput **256.5 MB/s**

---

### 43) 基准方法论可用性结论 + 清理

**可用性:是。** 映射与分层见 `docs/sdd/throughput-baseline.md`「推荐真实环境基准方法论」。

清理(本机):
- 删除 `%TEMP%\tachyon-test-*` / `tachyon-sec002-*` 测试目录
- 清理 `%TEMP%\.tmp*` 中 >10min 的 aria2/tempfile 残留 **780** 项
- `target/perf-baseline` 保留有效 JSON(~99 个,合计 ~0.1MB),无空文件

---

### 42) 真·多 Range CDN 对标(OVH / GitHub release)

#### 源探测(curl)

| 源 | Accept-Ranges | Range 0-1023 |
|----|---------------|--------------|
| proof.ovh.net/files/100Mb.dat | bytes | **206** Content-Range 正确 |
| GitHub Git-2.47.1-64-bit.exe → Azure blob | bytes | **206** Content-Range 正确 |
| Hetzner ash-speed 100MB | (本轮 HEAD 无有效输出) | — |
| CF `__down` | 多 Range 不友好 | aria2 Invalid range |

#### 实测(代理 + `--max-frag-mib 8`)

| 场景 | Tachyon | peak | frags | rebalance | aria2 | 判定 |
|------|---------|------|-------|-----------|-------|------|
| OVH 100MB c=2 | 420s 墙钟未完成(持续 TLS EOF,已多分片重试) | — | 多片(日志 index 0–5) | — | 未跑完 | **路径支持多分片**,链路掐断主导 |
| **GitHub Git ~66MB c=2** | **完成 116.7s, 592 KB/s**, aligned 100% | **2** | **17** | **3** | 同会话极慢(~3.5MiB/@几十 KiB,420s 未完成) | **多连接路径真实生效** |
| loopback 64MiB max-frag8 c=2(对照) | 195.7 MB/s | 2 | 16 | 0 | — | harness/clamp 正确 |

JSON: `cdn_github_git_c2_maxfrag8_vs_aria2.json`(Tachyon 完成;aria2 可能未完整写出)

#### 读法

1. **多分片/多连接在真 206 源上已打通**: GitHub release 出现 `frags=17 peak_conn=2 rebalance=3`,不再是 CF 单片假并发。
2. **本轮代理到欧/美大文件源整体很差**(OVH/GitHub/kernel 均 TLS EOF 主导);goodput 绝对数字**不能**当引擎性能上限。
3. **相对 aria2**:同烂链路下 Tachyon 仍先完成(~66MB/117s),aria2 长时间卡在数 MB 级——恢复/续传路径有效,但非健康带宽对比。
4. **健康带宽对比仍以 CF 单连接为准**(§40: 6–10× aria2);多连接收益需在**代理到该源也健康**时再测 c=2 vs c=1 同源。

#### 工程收获

- `--max-frag-mib` + `plan_fragments` clamp + scheduler 同步注入:loopback/GitHub 均证明可用
- 真 206 源 + 代理抖动 = 窗口化 Range + soft-pressure 的压力测试场;未引入完成性回归(GitHub 任务完成,aligned 100%)

---

### 41) peak_conn=1 根因 + max-frag 修复 + CF 多连接尝试

#### peak_conn=1 根因(已定位)

1. **默认 `max_fragment_size=64MiB`**: CF 64MiB 文件 → **1 分片** → 即使 `--concurrency 2` 也只能 peak=1
2. **`plan_fragments` 未 clamp 调度器建议值**: `suggested_frag_size` 可大于 `max_fragment_size`,配置被绕过
3. **bench 只 `set_scheduler_config` 不够**: `AdaptiveDownloadScheduler` 仍用 default max,需同时 `AdaptiveDownloadScheduler::new(sc)`

#### 代码修复

- `plan_fragments`: `suggested_frag_size` 也 `clamp(min,max)` + 单测 `test_plan_fragments_clamps_suggested_size_to_max` PASS
- harness: `--max-frag-mib N` 同时注入 task + scheduler
- loopback 验证: `--size 64MiB --max-frag-mib 8 -c2` → **frags=16, peak_conn=2**, 195 MB/s

#### CF 外部实测(代理)

| 场景 | Tachyon | aria2 | ratio | frags | peak | 备注 |
|------|---------|-------|-------|-------|------|------|
| 64MiB c=1 (先前) | **6.67 MB/s** | 656 KB/s | **10.2x** | 1 | 1 | 健康单连接 |
| 64MiB c=2 无 max-frag | **7.64 MB/s** | 964 KB/s | **7.9x** | 1 | 1 | 仍单片 |
| 64MiB c=2 max-frag=8 修后 | **2.91 MB/s** | 491 KB/s | **5.9x** | **仍 1** | 1 | 外部源未走多分片 |
| 64MiB c=4 max-frag=8 | **6.13 MB/s** | 未完成/极慢 | — | 1 | 1 | 同上 |
| CF 128MiB | **403 全失败** | — | — | — | — | 源限流/拒绝 |
| loopback 64MiB max-frag=8 c=2 | **195.7 MB/s** | — | — | **16** | **2** | 证明 harness+clamp 正确 |

#### 外部 CF 仍 frags=1 的推断

loopback 已证明 max-frag 路径正确。CF `__down` 经代理时很可能:

- probe 得到 **`supports_range=false`**(200 忽略 Range → 强制整块),或
- 元数据 size/Range 语义与多分片规划不一致

aria2 对 CF 多连接也报 **Invalid range header**(Response 整段 0-end),说明 **CF 该端点对多 Range 不友好**,回落单连接——与 Tachyon 单片路径一致。

#### 结论

1. **健康 WAN 单连接:Tachyon 仍 6–10× aria2**(§40)
2. **peak_conn=1 对 64MiB CF 是分片策略+源 Range 行为,不是连接池坏了**
3. **多分片/多连接** 在 loopback 已验证;对 CF `__down` 强推多 Range **无收益且源不配合**
4. 128MiB CF 被 403:换源或降频,勿当引擎回归失败

JSON: `cdn_cf_64mb_c2_maxfrag8_clamped.json`, `loopback_64_maxfrag8.json`, `loopback_maxfrag4_clamp.json`

---

### 40) 健康 WAN 公平对标:Cloudflare 大文件(替代 kernel.org)

kernel.org 经代理仍双边 TLS 掐断(§39)。改用 **speed.cloudflare.com** 作为健康大文件源做同会话 aria2 对标。

#### 结果(同代理,RUST_LOG=warn)

| 场景 | Tachyon | aria2 | ratio | peak | aligned | JSON |
|------|---------|-------|-------|------|---------|------|
| CF 8MiB c=1 复确认 | **8.48 MB/s** | — | — | 1 | 100% | `cdn_cf_reconfirm.json` |
| CF 64MiB c=1 | **6.67 MB/s** (10.1s) | **656 KB/s** (102s) | **10.16x** | 1 | 100% | `cdn_cf_64mb_c1_vs_aria2.json` |
| CF 64MiB c=2 | **7.64 MB/s** (8.8s) | **964 KB/s** (69.6s) | **7.92x** | 1 | 100% | `cdn_cf_64mb_c2_vs_aria2.json` |

注:c=2 时 harness 请求并发 2,但 peak_conn=1(单分片/调度未抬到 2);aria2 `-x2` 曾报 Invalid range 后回落单连接仍完成。

#### 读法

1. **健康 WAN 上 Tachyon ≫ aria2**(约 **8–10×**),aligned 100%,无 TLS EOF 风暴。
2. 与 §39 kernel 对照:**差距在源路径**,不是「Tachyon 全面输代理」。
3. kernel.org 历史健康 ~6 MB/s 仍待该源路径恢复后再比;当前有效对标源 = **CF**。
4. c=1→c=2:Tachyon 6.67→7.64 MB/s(+15%),收益有限;主优势已在单连接路径。

#### 与本机可控网络

- loopback 仍 ~200+ MB/s → 磁盘/调度非瓶颈
- CF WAN ~7–8 MB/s → 网络/代理带宽上限主导;客户端自伤路径已关

---

### 39) WAN 健康探针 + kernel 公平对标(本轮)

#### 探针(代理健康?)

| 场景 | Tachyon | aria2 | 判定 |
|------|---------|-------|------|
| CF 8MiB c=1 | **8.63 MB/s** (0.97s, aligned 100%) | 进行中被墙钟截断(慢启动,约百 KB/s 级) | **CF 路径健康** → 代理本身可用 |
| JSON | `cdn_cf_health_probe_before_kernel.json`(若完整写出) | — | — |

#### kernel.org ~134MiB c=2 同会话

| 客户端 | goodput | peak | rebalance | aligned | 备注 |
|--------|---------|------|-----------|---------|------|
| Tachyon | **275.8 KB/s** (507.7s 完成) | 2 | 1 | 100% | 多次 ~35–90s 周期 TLS EOF,partial resume 生效 |
| aria2 -x2 -s2 | **未完成**(600s 墙钟截断时 ~3MiB/@~20–50KiB/s) | 2 | — | — | **双边链路崩**,非 Tachyon 独慢 |

日志特征(Tachyon):`peer closed connection without sending TLS close_notify` / unexpected eof,分片 index 0/1/3/4/5 多次 `has_partial_progress=true` 短 backoff 续传。

#### 结论(必须分开读)

1. **代理整体**:CF 证明可用(~8.6 MB/s),不是「代理全挂」。
2. **kernel.org 经该代理的路径**:本轮 **Tachyon 与 aria2 同病**,墙钟由 TLS 掐断主导;历史健康会话 ~6 MB/s **本次未复现**。
3. **不可用本轮 kernel 数字** 作为「窗口化 Range / soft-pressure 优化有效/无效」的证据,也 **不能** 宣称输赢 aria2。
4. 客户端侧:aligned 100%、peak_conn=2 守住 ceiling、任务能完成(aria2 同超时内未完成)——说明修复后的恢复路径仍工作,但 goodput 被网络淹没。

#### 下次有效对标条件

- kernel 单次 goodput **≥ ~2 MB/s** 或 TLS EOF 次数显著下降后再比 ratio
- 同会话连续:Tachyon → aria2,c=2,记录 EOF 次数
- 若仅 CF 健康而 kernel 双边崩:换镜像/时段,勿只改 Tachyon 并发硬怼

JSON: Tachyon 结果写入 `cdn_kernel_c2_healthy_session.json`(若 harness 在 aria2 前已落盘);aria2 本轮无完整结果。

---

### 38) 多任务隔离 + 残余微优化

#### 代码

1. **HttpClientRegistry::clear**: 零进度 soft-pressure 时清空共享 client 池,防半死 TLS tunnel 跨任务复用
2. **ConnectionPool::host_semaphore**: hit 先 `get` 免 `to_string`,miss 仍 entry(H-06)
3. **Metrics 写侧 Relaxed**: 独立计数器无发布契约;并发计数单测 PASS

#### 验证

- `test_registry_clear_drops_cached_clients` + soft-pressure 套件 + host_semaphore hit + metrics concurrent **15/15 PASS**
- loopback 16MiB c=8: **276 MB/s** aligned 100% (`loopback_after_host_sem_opt.json`)

#### 第二轮审计结论

- MultiTaskIsolation: soft_pressure_until **NO_P0**(已 per-task);P1 registry 半死连接 → 本轮 clear
- MicroPerfResidual: **NO_HIGH_VALUE**(无 >5% e2e 可证项);P2 host String/Metrics/window 自适应/短写二次对齐
- 禁止项保持:4MB proxy frag cap

---

### 37) soft-pressure 冷却 per-task 隔离

#### 代码

- `DownloadTask.soft_pressure_until: Arc<AtomicU64>` 每任务独立
- `extend/blocks/clear/apply_soft_pressure_*` 全部吃 `&AtomicU64`
- `FragmentSpawnCtx` 注入同一 Arc 到分片 spawn
- **重连 spacing** 仍进程全局(有意:多任务交错减轻代理 TLS 风暴)
- 单测用 `fresh_soft_until()` 本地 Arc,无全局互串

#### 动机

审计 P0:任务 A soft-pressure 后任务 B 成功 `store(0)` 会清掉 A 的冷却 → 跨任务自伤。

#### 验证

soft-pressure/proxy/window/clamp 相关单测 PASS;无 clippy dead_code。

---

### 36) 续传/哈希/窗口完整性 P1 修复(CorrectnessAudit)

#### 代码

1. **resume_offset>0 禁用流式 computed_hash**: 仅冷启动 `resume==0` 时 `new_hasher()`;续传走 verify 读盘全范围
2. **`try_split` 拒绝 expected hash 分片**: whole-fragment hash 禁止 rebalance 拆分
3. **窗口响应超长 fail-closed**: 每 Range 按 `window_requested_len` 累计 body,超长 `DownloadError::Fragment`

#### 测试

- `test_try_split_rejects_when_expected_hash_present`
- `test_streaming_hash_skipped_on_resume_offset`
- `test_window_overlong_fail_closed_semantics`
- soft-pressure/proxy/try_split 相关 **20/20 PASS**

---

### 35) 调度策略收紧 v2 + 热路径日志再降级

#### 代码(交叉审计后落地)

1. **soft-pressure 成功冷却半衰**: `clear_soft_pressure_cooldown_on_success` 不再 `store(0)`,改为剩余时间 /2(至少 1s),防完成事件路径 2→4 回弹
2. **代理抬升步进 +1**: `clamp_concurrency_scale_up_ex(..., conservative=true)`
3. **proxy steady ceiling 4→2**: 与 soft-pressure floor / 健康会话 / aria2 `-x2` 对齐
4. **代理 rebalance 间隔 5s→20s**: 降恢复瞬间拆尾增连
5. **热路径 info→debug**: 分片完成/rebalance/Pause-Resume/HTTP Range 成功完成
6. 保留: mild 不砍 target、零进度 floor=2、2MiB windowed Range

#### 验证

| 场景 | Tachyon | aria2 | ratio | peak | aligned |
|------|---------|-------|-------|------|---------|
| CF 8MiB c=1 | **9.34 MB/s** | 0.43 MB/s | **21.9x** | 1 | 100% |
| loopback 16MiB c=8 | **260 MB/s** | — | — | 4 | 100% |
| unit soft-pressure/proxy/window/clamp | **14/14 PASS** | — | — | — | — |

JSON: `cdn_cf_8mb_after_policy_v2.json`, `loopback_after_policy_v2.json`

---

### 34) 代理下片内窗口化 Range(2MiB)

#### 代码

1. `proxy_range_window_bytes()`: 代理激活时 `Some(2MiB)`,直连 `None`
2. `range_window_end(start, frag_end, window)`: 计算含端窗口终点
3. `download_single_fragment`: 外层 `window_loop` 按窗开 `download_range_stream`;
   窗 EOF 后刷 write_buf,未满窗 → Fragment 错误交外层 partial resume;
   **plan_fragments 边界不变**(resume/rebalance 仍按分片 index)
4. 经 `FragmentSpawnCtx.range_window_bytes` 注入 spawn 路径

#### 动机

跨境代理 ~35s 掐 TLS 长连接。整片 8MiB@~600KB/s 跑不完 → EOF 丢当前连接上未 flush 进度。
2MiB 窗把最坏重传上界从整片收到 2MiB,且不改分片规划。

#### 验证

| 场景 | Tachyon | aria2 | ratio | peak | aligned | 备注 |
|------|---------|-------|-------|------|---------|------|
| CF 8MiB c=1 | **4.85 MB/s** | 0.98 MB/s | **4.93x** | 1 | 100% | 回归 OK |
| loopback 16MiB c=8 `--no-proxy` | **216 MB/s** | — | — | 4 | 100% | 直连不窗化 |
| kernel ~134MiB c=2 | **277 KB/s** 完成 | 同会话极慢(~12MiB/min 量级) | — | 2 | 100% | 本轮双边链路崩;窗口化仍能完成 |
| unit | `test_range_window_end_semantics` + soft-pressure 9 | PASS | — | — | — | — |

**说明**: 本轮 kernel 对 aria2 同样极慢,不能据此判定窗口化收益/损失;健康会话历史 ~6 MB/s 需另日同会话对照。

JSON: `cdn_cf_8mb_after_windowed_range.json`, `loopback_after_windowed_range.json`, `cdn_kernel_c2_windowed_range.json`(Tachyon 完成;aria2 墙钟中断)

---

### 32) probe 超时收紧后 CF 公平对标

| 场景 | Tachyon | aria2 | ratio | aligned |
|------|---------|-------|-------|---------|
| CF 8MiB 代理 c=1 | **9.15 MB/s** | 3.03 MB/s | **3.02x** | 100% |
| loopback 16MiB c=8 | **177.8 MB/s** | — | — | 100% |

probe attempt 超时改为 connect_timeout clamp 5..=10s;Timeout 退避与 403 同短(200–800ms)。

JSON: `cdn_cf_8mb_vs_aria2_tight_probe.json`, `loopback_after_probe_timeout.json`

---

### 33) soft-pressure: mild 不降并发 + 零进度下限 2

#### 代码

1. **mild(有 partial 进度)**: 只设 5s 冷却挡 scale-up,**不** `set_target` 降并发
2. **零进度**: 仍减半,但 `old>=2` 时 **floor=2**(禁止 2→1 串行化)
3. **回退** 代理 4MB max fragment cap(实测 1.10→0.60 MB/s 有害)

#### 动机

- 代理 TLS EOF 约 35s 周期掐长连接;有进度时再砍并发 = 客户端自伤
- aria2 不因 partial 失败主动把连接压到 1
- kernel 健康会话历史可到 ~6 MB/s peak2–4;抖动脉冲仍由链路主导

#### 验证

| 场景 | Tachyon | aria2 | ratio | peak | aligned |
|------|---------|-------|-------|------|---------|
| CF 8MiB c=1 | **5.83 MB/s** | 1.34 MB/s | **4.36x** | 1 | 100% |
| loopback 16MiB c=8 | **263.7 MB/s** | — | — | 4 | 100% |
| soft-pressure 单测 | 9/9 PASS | — | — | — | — |

kernel.org 本轮代理抖动:连续 ~35s TLS EOF/handshake eof,墙钟超时;不作为回归失败,链路主导。

JSON: `cdn_cf_8mb_after_mild_nocut.json`, `loopback_after_mild_nocut.json`

---

### 31) probe 墙钟上限 + RTT 钳制 + 重连片间错开

#### 代码

1. **probe attempt timeout**: `min(15, connect+5)` 秒,避免代理黑洞下 HEAD 挂满 `request_timeout`
2. **RTT 钳制**: probe 耗时 clamp 到 [1ms, 10s] 再 `observe_rtt`(>10s 不再被丢弃成默认 50ms)
3. **soft reconnect spacing**: soft-pressure 重连全局最小间隔 150ms,片间错开

#### 复测

| 场景 | 结果 |
|------|------|
| CF 8MiB 代理 c=1 | **10.78 MB/s** aligned 100% (`cdn_cf_8mb_probe_timeout.json`) |
| soft-pressure / reconnect / proxy cap 单测 | **10/10 PASS** |

---

### 30) 阶段交付：可重复基线 + 客户端最优栈

#### 关键证据

| 场景 | Tachyon | 对照 | 备注 |
|------|---------|------|------|
| loopback 32MiB c=8 | **218 MB/s** aligned **100%** | — | 磁盘/调度非瓶颈 |
| CF 8MiB 代理 c=1 | 0.70–4.03 MB/s | aria2 1.1–1.6 MB/s | 代理冷启动抖动大 |
| kernel.org 代理健康会话 | **~6.0 MB/s** peak4 | 最初 ~0.6 MB/s peak1 | **约 10×** |
| kernel 代理抖动脉冲 | 0.5–1.5 MB/s peak≤4 | — | 链路掐 TLS 主导 |

#### 客户端已收敛能力

1. 误熔断修复；403 软重试；TLS EOF soft-pressure  
2. 冷却不滑动 + 成功清冷却 + mild −1 / 零进度减半  
3. 分片 partial resume + 错误前 flush + absolute `expected_len`  
4. 有进度短 Full Jitter 退避 + 额外 +2 重试预算  
5. rebalance 5s 间隔 / 2s 年龄 / 20% 滞后  
6. 对齐写 passthrough 100%（分片大块批写）  
7. 代理冷启动 ≤2、稳态 ≤4；RTT 档位更严  
8. harness：`--no-proxy` / `--http1-only` / `--compare-aria2`  

#### 剩余上限

- **本机 HTTP_PROXY 跨境抖动**（TLS EOF/handshake eof/probe RTT>10s）  
- 非磁盘、非调度、非对齐写、非再叠客户端复杂度可消  

JSON 目录：`target/perf-baseline/`；文档：`docs/sdd/throughput-baseline-results.md`。

---

# 吞吐基线与 WAN Range 修复（2026-07-25 续）

## 0e. 本轮关键修复（2026-07-25）

### 1) 403 软重试 + 冷却期内不连砍并发

- `DownloadError::Forbidden{status:403}` / `Http{403}` **可重试**（401 仍不可）
- 软压力**先于** `is_retryable` 判定：本片终态放弃前也降并发保护兄弟分片
- 冷却期内只降一次并发（防 8→4→2→1 连砍）；冷却 30s
- 冷却结束后并发抬升 **步进限制**：每次最多翻倍
- 分片退避睡眠期间 **释放 permit/active**，使降并发立刻生效
- 软压力终态失败 **不** `record_failure`（避免 N 片同时放弃瞬间熔断整源）
- 冷却期内禁止 rebalance 拆片

### 2) Probe：HEAD → Range:0-0 → plain GET 三级回退

- CF `speed.cloudflare.com` 对 Range:0-0 返回 403 时，旧路径 probe 直接终态失败
- 新增 `probe_via_get_no_range`：Range 403/405/5xx 后再用无 Range GET 取头
- 强制 `supports_range=false`，避免未证明 Range 时走分片浪费

### 3) TLS handshake EOF 纳入软压力

- 覆盖 `tls handshake eof` / `handshake eof` / `client error (connect)`
- rebalance 新片最小年龄 **300ms → 1s**，降低 WAN 抖动连环拆

### 4) 热路径日志

- 分片准备 / 调度并发建议 / 闭环并发调整 / probe 完成等 `info!` → `debug!`

### 5) 外网复测（本机）

| 源 | 结果 | goodput | aligned | rebalance | peak | JSON |
|----|------|---------|---------|-----------|------|------|
| CF 1MiB（plain GET 回退） | **成功** | 2.81 MB/s | 100% | 0 | 1 | `cdn_cf_1mb_plain_get_fallback.json` |
| OVH 1MiB | 成功但极慢 | 8.57 KB/s | 100% | 0 | 1 | `cdn_ovh_1mb_range.json`（TLS EOF 重试） |
| OVH 10MiB Range | 成功但极慢 | 38.28 KB/s | 100% | 21 | 8 | `cdn_ovh_10mb_range.json`（TLS EOF + handshake eof） |
| loopback 16MiB | 成功 | 40–110 MB/s 噪声 | 100% | 0 | 4 | `loopback_after_backoff_release.json` 等 |

**分解**:

1. **客户端自伤路径已收敛**: 误熔断、403 不可重试、退避占槽、冷却连砍、假 rebalance 均已修。
2. **本机到 OVH 的主瓶颈是链路/对端掐连接**（TLS EOF/handshake eof），不是磁盘/对齐写。
3. **CF 合成下载** 在本机对 Range 不友好；plain GET 回退后可下，但无分片。
4. loopback aligned_hit 稳定 100%；磁盘/调度非 WAN 主因（与历史结论一致）。

### 6) loopback 被系统代理劫持(已修)

**现象**: 本机 `ThrottledServer` 已 listen,但 probe 报 `tcp connect error / 连接被拒绝`。

**根因**: `Proxy::all(HTTP_PROXY)` 把 `127.0.0.1:随机端口` 也塞进代理;代理无法转发本机随机端口。

**修复**: `apply_loopback_no_proxy` 对 `localhost/127.0.0.1/::1/.localhost` 旁路。

**复测**: loopback 16MiB → **224.84 MB/s**, aligned 96.2%, rebalance=0, peak_conn=4  
(`loopback_after_no_proxy.json`)

### 7) 验证


- `tachyon-core` is_retryable 真值表 + 403 可重试：通过
- `tachyon-protocol` probe HEAD/Range/plain GET：通过
- soft pressure / rebalance age / 403 recover：通过
- clippy `-D warnings`（engine/core）：通过

---

### 28) 代理稳态并发天花板 ≤3

#### 代码

- `http_proxy_active()` / `proxy_steady_concurrency_ceiling()` / `apply_proxy_concurrency_ceiling()`
- 冷启动 cap 仍 ≤2(低置信度);re-recommend 抬升也受 **稳态 cap=3**
- `proxy=direct|none` 不 cap

#### 复测

| 场景 | 请求并发 | peak_conn | goodput | aligned | JSON |
|------|----------|-----------|---------|---------|------|
| kernel 代理 + steady cap | 8 | **3** | 677 KB/s | 100% | `cdn_kernel_c8_proxy_steady_cap3.json` |
| 对比:仅 cold cap | 8 | **5** | 572 KB/s | 100% | `cdn_kernel_c8_proxy_cold_cap.json` |
| 健康会话 c=4 mild | 4 | 4 | **6.00 MB/s** | 100% | `cdn_kernel_c4_mild_cut.json` |

证据:稳态 cap 阻止代理下爬到 5+ 打爆;健康链路时 c=2/4 仍可达 ~6MB/s。

单测: `test_proxy_cold_start_cap_for_config` 覆盖 steady ceiling。

---

### 27) 代理冷启动 cap + 更严 RTT 档位

#### 代码

1. **engine `proxy_cold_start_cap_for_config`**: 低置信度且系统/显式代理激活时,初始并发 ≤2  
   - `proxy=direct|none` 不 cap  
   - 样本充足(confidence≥0.5)后仍走 Holt 爬升  
2. **scheduler `rtt_cold_start_cap`**: 30–80ms→6, 80–150ms→3, ≥250ms→1  

证据:经 `HTTP_PROXY` probe RTT 近 0,旧 RTT cap 失效;代理下 c=2 往往稳于盲目 c=4/8。

#### 复测

| 场景 | goodput | peak | rebalance | aligned |
|------|---------|------|-----------|---------|
| kernel c=8 代理(cold cap) | 572 KB/s | 5 | 1 | 100% | 完成;初始 cap 后可爬升 |
| kernel c=4 mild-cut 健康会话 | **6.00 MB/s** | 4 | 2 | 100% |
| CF 8MiB vs aria2 | **4.03 / 1.59 MB/s** | 1 | 0 | 100% | **2.54x** |

单测: `test_proxy_cold_start_cap_for_config`, `test_rtt_cold_start_cap_tiers`, `test_cold_start_high_rtt_caps_concurrency` PASS。

---

### 26) 公平对标与 mild-cut 后本地健康(2026-07-26)

| 场景 | Tachyon | aria2 | ratio | aligned | peak |
|------|---------|-------|-------|---------|------|
| CF 8MiB c=1 代理 | **4.03 MB/s** | 1.59 MB/s | **2.54x** | 100% | 1 |
| loopback 32MiB c=8 `--no-proxy` | **215.5 MB/s** | — | — | **100%** copy=0 | 4 |
| kernel H2 c=4 mild-cut | **6.00 MB/s** | — | — | 100% | 4 |
| kernel H2 c=2 mild-cut | **5.80 MB/s** | — | — | 100% | 2 |

JSON: `cdn_cf_8mb_vs_aria2.json`, `loopback_after_mild_cut.json`, `cdn_kernel_c4_mild_cut.json`

**阶段结论(可重复基线已建立)**:

1. **可控网络**: 磁盘/调度/对齐写非瓶颈; Tachyon ≥ aria2。
2. **WAN 代理**: 客户端自伤路径已关; goodput 从 ~0.6 MB/s/peak1 提升到 **~6 MB/s/peak4**。
3. **剩余上限**: 本机 `HTTP_PROXY` 跨境链路抖动,非再砍客户端复杂度可消。
4. **工具**: `throughput_baseline` 支持 `--no-proxy` / `--http1-only` / `--compare-aria2`。

---

### 25) mild soft-pressure + H1/H2 对照(2026-07-26)

#### 代码

- 有进度 soft-pressure:**mild −1 + 冷却 5s**(不再完全不砍,也不再 /2 钉死)
- 零进度:仍减半 + 15s
- harness:`--http1-only` → `PoolConfig.enable_http2=false`

#### 同会话 kernel.org (~134MiB) 代理下

| 配置 | goodput | peak | rebalance | aligned | JSON |
|------|---------|------|-----------|---------|------|
| H2 c=2 (此前抖动) | 388 KB/s | 2 | 1 | 100% | `cdn_kernel_c2_extra_budget.json` |
| **H1 c=2** | **1.19 MB/s** | 2 | 1 | 100% | `cdn_kernel_c2_h1.json` |
| **H2 c=2 same-session** | **6.05 MB/s** | 2 | 2 | 100% | `cdn_kernel_c2_h2_samesession.json` |
| H2 c=4 抖动 | 603 KB/s | 4 | 1 | 100% | `cdn_kernel_c4_h2_samesession.json` |
| **H2 c=2 mild-cut** | **5.80 MB/s** | 2 | 2 | 100% | `cdn_kernel_c2_mild_cut.json` |
| **H2 c=4 mild-cut** | **6.00 MB/s** | **4** | 2 | 100% | `cdn_kernel_c4_mild_cut.json` |

CF 1MiB 代理: H2 3.91 MB/s vs H1 3.38 MB/s(单连接噪声内)。

#### 分解

1. **客户端路径已能在代理 WAN 上稳定 ~6 MB/s**,aligned 100%,peak 可保持配置并发。
2. 相对最初 ~0.6 MB/s / peak=1:**约 10× goodput,peak 恢复**。
3. H1/H2 非决定性;链路抖动主导。mild-cut 让 c=4 不再明显劣于 c=2。
4. 剩余上限仍是 **代理/跨境链路**,非磁盘/调度/对齐写。

---

### 24) 额外重试预算 + c=2 kernel 复测

| 场景 | goodput | peak | rebalance | aligned | 备注 |
|------|---------|------|-----------|---------|------|
| kernel c=4 progress-aware | **1.50 MB/s** | 4 | 1 | 100% | 最佳 |
| kernel c=2 + budget+6 | **388 KB/s** | 2 | 1 | 100% | 完成但更慢;日志 `advanced_resume=true`/`max_retries=6`/`backoff_ms` jitter 已生效 |
| CF 1MiB 代理 | **1.99 MB/s** | 1 | 0 | 100% | aria2 8.5x 落后 |

结论: **有进度续传+短 jitter 退避+额外预算** 可完成;并发 4 优于 2。外网仍受代理掐 TLS 主导。

JSON: `cdn_kernel_c2_extra_budget.json`, `cdn_kernel_after_progress_aware.json`

---

### 23) 有进度 soft-pressure 额外重试预算

**问题**: kernel.org 仍见 `attempt=4` 后片失败;代理抖动下 `max_retries=4` 不够撑完 partial 续传。

**修复**: `advanced_resume && soft_pressure` 时 `budget = max_retries + 2`;零进度不变。

单测: `test_soft_progress_retry_budget` PASS。

---

### 22) 有进度短退避 Full Jitter

**问题**: 多分片同时 TLS EOF 后用相同 250ms×2^n 退避,同步重试仍打爆代理。

**修复**: 有进度短退避改为 **Full Jitter ∈ [1ms, cap]**, cap=min(2000, 250×2^attempt)。

单测 cap 公式 PASS;与 progress-aware 不砍并发叠加。

---

### 21) 进度感知 soft-pressure 后 WAN 复测

| 场景 | goodput | peak | rebalance | aligned | 备注 |
|------|---------|------|-----------|---------|------|
| kernel.org ~134MiB c=4 | **1.50 MB/s** | **4** | **1** | 100% | 修前 598KB/s peak1; 中间 838KB/s rebalance21 |
| CF 1MiB c=1 + 代理 | **1.99 MB/s** | 1 | 0 | 100% | aria2 235KB/s → **8.5x** |

证据: 有进度不砍并发 + 短退避 + rebalance 间隔 → peak 稳住、墙钟下降。

JSON: `cdn_kernel_after_progress_aware.json`, `cdn_cf_1mb_progress_aware.json`

---

### 20) 有落盘进度时不砍并发

**问题**: TLS EOF 后即使 partial 已 flush,仍先 `apply_soft_pressure_backoff` 把 target 砍半,再短退避续传 → peak_conn 仍易掉到 1。

**修复**: 先推进 `resume_offset`;**仅 `!advanced_resume` 时**才砍并发。有进度 = 链路仍在吐数据,只短退避续传。

单测: `test_soft_pressure_skips_cut_when_progress_exists` + partial resume 集成测 PASS。

---

### 19) 已推进 resume 时 soft-pressure 短退避

**问题**: TLS EOF 后即使已 flush partial,仍走 2/4/8/16s 长退避,空等主导墙钟。

**修复**: `advanced_resume` 时退避改为 **250ms×2^attempt,上限 2s**;零进度仍用长退避。

单测: `test_soft_pressure_short_backoff_when_resume_advanced` + partial resume 集成测 PASS。

---

### 18) rebalance 最小间隔 + 更严年龄/滞后

**问题**: kernel.org soft-pressure 恢复后 `rebalance=21` 连环拆片,分片数膨胀、连接抖动放大 TLS EOF。

**修复** (`try_rebalance_slowest_fragment`):
1. **成功拆分最小间隔 5s** (`last_rebalance_at`)
2. 新片最小年龄 **1s → 2s**
3. 滞后门控 **15% → 20%**

单测: `test_rebalance_min_interval_blocks_second_split` + 既有 rebalance/soft-pressure **14/14 PASS**。

#### 复测

| 场景 | goodput | aligned | rebalance | peak |
|------|---------|---------|-----------|------|
| rtt50 64MiB c=8 | **126.8 MB/s** | 100% | **0** | 6 |
| cap≈100Mbps c=8 | **20.5 MB/s** | 100% | **1** (曾 2) | 8 |
| kernel.org c=4 | 本轮代理连续 TLS EOF 失败 | — | — | 环境抖动(前次 838KB/s, rebalance 21→本改目标压频) |

---

### 17) kernel.org 复测(续传 + 成功清冷却 + 不滑动)

| 指标 | 修前(滑动冷却) | 修后 |
|------|----------------|------|
| 结果 | 成功但极慢 | **成功** |
| goodput | ~598 KB/s | **838 KB/s** |
| peak_conn | **1** | **4** |
| aligned | 100% | **100%** copy=0 |
| rebalance | 0 | 21 |
| JSON | `cdn_kernel_linux661_c4_proxy.json` | `cdn_kernel_after_resume_retry.json` |

证据链: soft-pressure 不再钉死并发 + TLS EOF 后 partial resume 减少重传浪费。  
外网仍受代理掐连接影响,但客户端自伤路径已关掉。

---

### 16) 分片 TLS EOF 续传可工作(flush + absolute expected_len)

**配套修复**（否则 resume 推进无效）:

1. **流错误前 flush `write_buf`**: TLS EOF 时已收未满批字节先落盘并更新 `realtime_downloaded`。
2. **`expected_len` 语义**: 用 absolute `full_len` 作 `flush_batch` 上限；完成校验用 `full_len - resume`。
   - 旧语义把 remaining 当 absolute 上限 → resume 后 half+half 误报越界。
3. 集成测: `test_fragment_retry_resumes_after_partial_tls_eof` **PASS**。
4. 回归: `test_fragment_*` / soft-pressure / 403 / full-align **27/27 PASS**；clippy engine `-D warnings` 通过。

---

### 15) 分片可重试失败从已写字节续传

**问题**: TLS EOF 后分片重试仍用初始 `resume_offset`,已 flush 的字节被整片重下(WAN 主浪费)。

**修复** (`spawn_fragment_task` 重试路径):
- 无流式哈希(`!compute_hash`):`resume_offset = max(resume, realtime_downloaded)`
- 有 expected hash:保持原 resume(流式哈希不能中途接续)

单测: `test_fragment_retry_resume_semantics_without_hash`。

---

### 14) 2026-07-26 成功清冷却后本地矩阵

| 场景 | Tachyon | aria2 | ratio | aligned | peak | rebalance |
|------|---------|-------|-------|---------|------|-----------|
| loopback 32MiB c=8 `--no-proxy` | **201.9 MB/s** | — | — | **100%** | 4 | 0 |
| rtt50 64MiB c=8 `--no-proxy` | **143.2 MB/s** | — | — | **100%** | 4 | 0 |
| cap≈100Mbps c=8 `--no-proxy` | **22.7 MB/s** | 7.7 MB/s | **2.94x** | **100%** | 8 | 2 |
| CF 1MiB + 系统代理 | 183 KB/s | 95 KB/s | **1.93x** | 100% | 1 | 0 |
| kernel.org ~134MiB c=4 | 本轮代理掐断失败 | — | — | — | — | 环境抖动(曾成功 598KB/s) |

结论不变:

1. **可控网络 Tachyon ≥ aria2**,aligned 100%,磁盘/调度非瓶颈。
2. soft-pressure 成功清冷却 + 不滑动续期后,cap 场景 **peak_conn=8** 正常打满。
3. 外网仍由代理/链路主导;客户端路径已不自伤。

---

### 13) 分片/整块成功提前结束软压力冷却

**问题**: 冷却 15–30s 固定窗口内,即使已有分片成功,`soft_pressure_blocks_scale_up` 仍禁止抬升,peak_conn 长期钉在 1。

**修复**:
1. 冷却期内不滑动续期(§12)
2. 冷却默认 30s → **15s**
3. **分片成功** / **整块成功** 调用 `clear_soft_pressure_cooldown_on_success`
4. 抬升仍受 `clamp_concurrency_scale_up`(每次最多翻倍)

单测: `test_soft_pressure_success_clears_cooldown` 通过。

#### 复测

| 场景 | goodput | aligned | peak | ratio vs aria2 |
|------|---------|---------|------|----------------|
| loopback 32MiB c=8 `--no-proxy` | **201.9 MB/s** | **100%** copy=0 | 4 | — |
| CF 1MiB c=1 + 系统代理 | 182.6 KB/s | 100% | 1 | **1.93x** (aria2 94.5 KB/s) |

JSON: `loopback_after_success_clear_cooldown.json`, `cdn_cf_1mb_after_success_clear.json`

---

### 12) soft-pressure 冷却不滑动续期

**问题**: kernel.org c=4 实测 `peak_conn=1`。根因是每片 TLS EOF 都 `extend_soft_pressure_cooldown(+30s)`,until 被滑动推后,冷却永不结束,并发永久卡在半值。

**修复**: 冷却期内 `apply_soft_pressure_backoff` **直接 return**:
- 不滑动续期
- 不连砍 target

单测: `test_soft_pressure_cooldown_does_not_slide` 通过。

---

### 11) 2026-07-25 系统代理外网矩阵 + probe 403 短退避

#### 代码

- probe 遇 `403`：**200–800ms 短退避**，不走 soft-pressure 2/4/8/16s（WAF 永久拒时少浪费）
- TLS EOF/5xx 仍长退避 + 冷却

#### 外网结果（`HTTP_PROXY=127.0.0.1:7897`，**无** `--no-proxy`）

| 源 | 结果 | goodput | aligned | rebalance | peak | 说明 |
|----|------|---------|---------|-----------|------|------|
| CF 1MiB c=1 | **成功** | **1.39 MB/s** | 100% | 0 | 1 | aria2 165 KB/s → **Tachyon 8.4x** |
| CF 16MiB | 失败 403 | — | — | — | — | 大文件 WAF；probe 短退避 ~2s 终态 |
| OVH 1/10MiB | TLS EOF 重试失败/极慢 | — | — | — | — | 代理掐连接 |
| **kernel.org linux-6.6.1.tar.xz (~134MiB) c=4** | **成功** | **598 KB/s** | **100%** | 0 | **1** | soft-pressure 压到 1 连接; copy=0 |

JSON:
- `cdn_cf_1mb_proxy_aria2.json`
- `cdn_kernel_linux661_c4_proxy.json`

#### 分解

1. **客户端自伤已收敛**: Range 外网大文件可完成且 aligned 100%、不误熔断。
2. **peak_conn=1**: soft-pressure 在 TLS EOF 后正确降并发（c=4 配置 → 运行峰 1）。
3. **外网 goodput 受代理/链路限制**（~0.6–1.4 MB/s），非磁盘/调度。
4. 同机 CF 1MiB：**Tachyon ≫ aria2**（小文件启动/连接噪声放大 ratio，但方向正确）。

---

### 10) 2026-07-25 harness `--no-proxy` + 分片大块对齐批写

#### 代码

1. **`--no-proxy`**: `DownloadConfig.proxy = "direct"` → `HttpClient` `builder.no_proxy()`,忽略 `HTTP_PROXY`。
2. **分片大 chunk 未对齐**:不再直写 `flush_batch(chunk)` 触发 `ensure_aligned` 拷贝;改为切块装入 `write_buf`,满批 freeze 后指针对齐 → passthrough。
3. 配置校验允许 `proxy=direct|none` 哨兵。

#### 复测

| 场景 | 结果 | goodput | aligned | copy | rebalance | JSON |
|------|------|---------|---------|------|-----------|------|
| loopback 64MiB c=8 `--no-proxy` | **成功** | **220.58 MB/s** | **100%** | **0** | 0 | `loopback_after_align_batch_no_proxy.json` |
| 对比:修前 matrix_loopback | 成功 | 258 MB/s | **75.4%** | **63** | 0 | `matrix_loopback_c8.json` |
| CF 16MiB `--no-proxy` | 失败 403 | — | — | — | — | 本机无代理被 WAF 拒 |
| OVH 10MiB `--no-proxy` | 超时/不可达 | — | — | — | — | 直连外网差 |

**结论**:

- aligned copy 主因是 **分片热路径大 chunk 未对齐直写**;批写修复后 loopback **copy=0**。
- 吞吐略降属噪声(220 vs 258),对齐命中率更重要(IOCP/NO_BUFFERING)。
- 本机外网:有代理时部分可达,无代理 CF/OVH 更差 → 环境依赖代理,非客户端回归。

---

### 8) 2026-07-25 本地可控矩阵 + aria2 对标

环境: `HTTP_PROXY=http://127.0.0.1:7897`(系统代理在线)。  
外网 GitHub 当前 HEAD 超时;OVH 经代理偶发 TLS handshake eof。  
**外网矩阵暂不可用**,改用本地 server 可控 RTT/带宽。

| 场景 | Tachyon goodput | aria2 goodput | ratio | aligned | rebalance | peak_conn | JSON |
|------|-----------------|---------------|-------|---------|-----------|-----------|------|
| loopback 64MiB c=8 | **258.36 MB/s** | 23.05 MB/s | **11.2x** | 75.4% | 0 | 4 | `matrix_loopback_c8.json` |
| rtt50 64MiB c=8 | **144.41 MB/s** | 80.59 MB/s | **1.79x** | 73.1% | 0 | 4 | `matrix_rtt50_c8.json` |
| cap≈100Mbps (12.5MB/s) c=8 | **22.75 MB/s** | 7.88 MB/s | **2.89x** | 100% | 2 | 8 | `matrix_cap100Mbps_c8.json` |

解读:

1. **可控网络下 Tachyon ≥ aria2**,磁盘/调度不是短板。
2. 带宽 cap 场景 Tachyon 多连接把上限打穿到 ~1.8× 单连接 cap 标称值;aria2 同 cap 约 7.9 MB/s。
3. loopback aria2 墙钟含启动/连接关闭噪声,ratio 偏大;以 rtt50/cap 更公平。
4. **外网主瓶颈仍是代理/链路**:本机 `HTTP_PROXY` 劫持 HTTPS CONNECT;GitHub timeout;OVH TLS EOF。
5. 新增 **probe 软重试**(`max_retries` + soft-pressure backoff),避免 probe 一次 TLS EOF 整任务终态失败。

### 9) 本轮代码增量(相对 0e)

- engine `probe`:可重试错误退避重试 + 延长 soft-pressure 冷却
- 单测 `test_probe_retries_on_soft_pressure_network_error`
- 代理 loopback 旁路(0e§6)继续生效

---

# 吞吐基线与 WAN Range 修复（2026-07-24 续）

## 0. 本轮关键修复：误熔断

**现象**: 真实 CDN Range 下载多片同时 `error decoding response body`，随后整 URL 被熔断。

**根因（已用错误链确认）**:
```
error decoding response body
  -> request or response body error
  -> error reading a body from connection
  -> peer closed connection without sending TLS close_notify (rustls unexpected EOF)
```
对端/中间盒掐断 TLS 时，**每个分片每次中间重试**都 `circuit_breakers.record_failure(url)`。  
默认阈值 5 + 并发 8 片 → **第一次抖动就熔断整源**。

**修复**:
1. 仅在分片**终态失败**时 `record_failure`；中间可重试失败不记。
2. 流读错误展开 `error_chain`（可诊断 timeout/TLS EOF）。
3. 基线 harness WAN 超时放宽：`request_timeout_secs=120`，`max_retries=4`。

**复测（GitHub release Range, concurrency=4）**:
| 项 | 值 |
|----|----|
| 结果 | **成功** |
| goodput | 1.46 MB/s（跨国链路慢，但不再误熔断） |
| aligned_hit | **100%** |
| rebalance | 10 |
| peak_conn | 4 |
| JSON | `target/perf-baseline/cdn_github_git_after_breaker.json` |

**测试**: `test_b5_*` + `test_rebalance*` 全绿。

**清理**: 删除 `target/tmp-wan`、`%TEMP%\tachyon-test-*`；保留 `tachyon-advisory-db` 与基线 JSON。

---

## 0b. 软压力降并发（本轮）

**动机**: 慢跨国链路下多连接易触发 TLS unexpected EOF；继续高并发只会加剧掐断。

**实现** (`DownloadTask`):
- `is_connection_soft_pressure`: 识别 TLS close_notify / unexpected EOF / connection reset / body decode 等
- `apply_soft_pressure_backoff`: 中间重试时 `target = max(1, target/2)`，不中断在途 task

**日志证据**（GitHub Range, concurrency=8）:
```
检测到连接软压力,降低目标并发 old_concurrency=4 new_concurrency=2
检测到连接软压力,降低目标并发 old_concurrency=2 new_concurrency=1
```

**单测**: `test_connection_soft_pressure_detection` + `test_soft_pressure_backoff_halves_target` 通过。

**本机外网复测**: 软压力路径已触发；后续失败变为源端 **HTTP 504**（环境/对端网关），非客户端误熔断。此前 concurrency=4 曾成功（1.46 MB/s）。

**清理**: `%TEMP%\tachyon-test-*`、`target/tmp-wan` 已删；保留 `tachyon-advisory-db` 与 `target/perf-baseline/*.json`。


## 0c. 502/504 也走软压力 + 更长退避

- `is_connection_soft_pressure` 覆盖: `Http{502|503|504}`, `Timeout`, `Throttled`, TLS EOF 等
- 中间重试: 并发减半 + `soft_pressure_backoff_secs`（至少 2s，随 attempt 指数放大，上限 60s）
- 单测: detection / 降并发 / backoff 下限 全绿

---

## 0d. RTT 冷启动并发上限

`AdaptiveDownloadScheduler::rtt_cold_start_cap`（仅样本不足时生效）:

| probe RTT | 冷启动 concurrency 上限 |
|-----------|-------------------------|
| < 50ms | 不额外限制 |
| 50–100ms | 8 |
| 100–200ms | 4 |
| ≥ 200ms | 2 |

样本充足后仍走 Holt；运行中 TLS EOF/502/504 由 soft-pressure 继续降并发。

Smoke: loopback 32MiB → 99.7 MB/s, aligned 100%, peak_conn=4；rtt150 → peak_conn=10（爬坡后）。

---



# 吞吐基线结果（更新 2026-07-24 夜）

> 环境: Windows 11, i9-12900H  
> 工具: `throughput_baseline` + `tools/aria2/aria2c.exe`  
> 原始 JSON: `target/perf-baseline/*.json`

## 1. 真实 CDN / 外网（任务 1）

| 源 | Range | 结果 | goodput | aligned_hit | rebalance | peak_conn | 说明 |
|----|-------|------|---------|-------------|-----------|-----------|------|
| Cloudflare `speed.cloudflare.com/__down?bytes=32MiB` | **否**(Range→200 整包) | 成功 | **7.97 MB/s** | **0.0%** (copy 主导) | 0 | **1** | 整块路径;无分片/rebalance |
| CF 双源同 URL mirror | 否 | 成功但退化 | 8.27 MB/s median | 0% | 0 | 1 | 身份不兼容剔除混拼;仍单连接 |
| OVH `proof.ovh.net/100Mb.dat` | 是(HEAD 206) | **失败** | — | — | — | — | 分片流 `error decoding response body` → 熔断 |
| kernel.org linux-6.6.1.tar.xz | 是 | **失败** | — | — | — | — | 同上 |
| jsDelivr 小文件 | 是 | 成功 | 2.26 MB/s | 0% | 0 | 1 | 过小无分片意义 |
| HF / hf-mirror | — | **不可达** | — | — | — | — | 本机超时 |

### 关键结论（外网）

1. **本机到部分海外源 Range 分片不稳定**（body decode 失败 → 熔断），不能作为稳定 rebalance 证据。  
2. Cloudflare 合成下载 **不支持 Range** → 强制整块 + 未对齐 `Bytes` → **aligned_hit=0**，暴露生产侧“大 chunk 未对齐拷贝”在 WAN 上真实发生。  
3. 双源需要 **兼容对象身份**；同动态 URL 会触发身份剔除。  
4. **本地可复现替代**: `--local-mirror` 异构双源（见 §3）。

## 2. debug profile vs Release（任务 2）

同场景: 64MiB loopback, concurrency=16, runs=3

| 构建 | 二进制 | median goodput | rebalance | peak_conn | aligned |
|------|--------|----------------|-----------|-----------|---------|
| **dev** (`cargo build --bench ... --profile dev`) | `target/debug/deps/throughput_baseline-3beadc2c84ed1104.exe` | **133.30 MB/s** | 8–12 | 8 | 100% |
| **release** (`cargo bench` / release deps) | `target/release/deps/throughput_baseline-862257f3109717cb.exe` | **39.05 MB/s*** | 2–7 | 9–11 | ~99% |

\* 同会话 release 中位异常偏低且方差大（35–70）；更早同机 release 常见 **130–141 MB/s**。  
**结论**:

- 当前 `dev` profile 在 workspace 下带 `opt-level=1`（根 `Cargo.toml`），**不是纯未优化 debug**。  
- 本轮数字 **不能**证明 “Release 更快”；更像 **瞬时负载/调度噪声**。  
- 严格 A/B 需: 同会话交替跑、runs≥5、或 `RUSTFLAGS`/`profile.dev.package` 关掉 opt 做真 debug。  
- 热路径日志降级后，**RUST_LOG 级别不是主因**（此前 r5 已显示 info≈warn）。

## 3. rtt50 rebalance 积极性（任务 3）

| 场景 | median goodput | rebalance | peak_conn | 解读 |
|------|----------------|-----------|-----------|------|
| rtt50 纯主源 (lag-gate 后) | **87.70 MB/s** | **3–8** | 8 | 仍有 rebalance，但低于早期 11–16 假拆分 |
| rtt50 + cap 100Mbps | **25.23 MB/s** | **3–8** | 9–10 | 多连接打穿单连接 cap；rebalance 适度 |
| **本地双源异构** 主 rtt20 + 慢镜 rtt200/5MB/s | **53.06 MB/s** | **11–15** | 8–12 | **滞后门控下仍积极救援**；对齐 100% |

命令:

```text
cargo bench --bench throughput_baseline -- \
  --size 64MiB --rtt-ms 20 --local-mirror --mirror-rtt-ms 200 --mirror-bps 5M --runs 3
```

**判定**: lag-gate（≥2 在途且 progress 差 ≥15%）**没有把 rtt/异构场景饿死**；loopback 均匀假拆分已收敛。  
**暂不需要再调阈值**（15% 合适）；若未来 WAN 拖尾仍长，可把阈值降到 10% 或按 remaining_bytes 加权。

## 4. 三项任务完成状态

| 任务 | 状态 | 产物 |
|------|------|------|
| 1. 真实 CDN/HF 矩阵 | **部分完成** | CF 成功(无 Range)；OVH/kernel 失败有记录；HF 不可达；本地双源补异构证据 |
| 2. debug vs Release | **部分完成** | dev vs release 二进制已跑；噪声大，未证稳定差距 |
| 3. rtt50 rebalance | **完成** | rtt50 + 异构双源显示 rebalance 仍积极；无需再激进 |

## 5. 工程改动（本轮）

- harness: `--local-mirror` / `--mirror-rtt-ms` / `--mirror-bps`
- rebalance: 进度滞后门控（此前已合）
- 清理: 忽略 `tools/aria2/`；删部分 `%TEMP%\tachyon-test-*`

## 6. 优先后续（按证据）

1. **WAN Range 失败根因**: `error decoding response body`（代理/TLS/并发？）— 比再调 rebalance 更影响真实 CDN 吞吐  
2. **CF 类无 Range 源**: 大 chunk 对齐拷贝（aligned_hit=0）— 可选在 HTTP 层预对齐缓冲  
3. 真 debug（`opt-level=0`）vs Release 同会话 ≥5 runs  
4. 有 HF 可达网络时补 HF+mirror 双源
