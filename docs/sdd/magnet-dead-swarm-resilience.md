# SDD: 磁力链接死 swarm 韧性设计

> 日期: 2026-07-02
> 状态: 已实现
> 范围: tachyon-core / tachyon-protocol / tachyon-engine / tachyon-app(前端)

## 1. 问题背景

磁力链接下载在死 swarm(无可用 peer)场景下表现恶劣,存在三层叠加缺陷:

### 1.1 缺陷一:dispatcher spawn 顺序死锁(根因 A)

`execute_fragmented_download` 中,分片入队循环(`frag_tx.send().await`)在
dispatcher/worker spawn **之前**执行。`frag_tx` channel 容量为
`worker_count * 2`,当分片数 > 容量时(如 84 分片、4 worker → 容量 8),
`send().await` 在第 9 个分片永久挂起 —— 消费者(dispatcher)尚未 spawn。

**现象**:日志停在"分片下载准备就绪"后完全静默,无 worker 启动日志,
无 stall 超时,进度永久 0%。`frag_count=84` 证明元数据已获取,但数据
下载完全不启动。

### 1.2 缺陷二:取消信号无法穿透 stream.next().await(根因 B)

`download_single_fragment` 的 chunk 消费循环用裸
`while let Some(...) = stream.next().await`,取消检查点(`wait_control_rx`)
在循环体**内部**,只在 chunk 到达后执行。死 swarm 下 `FileStream::read()`
永久 `Pending`(librqbit 注册 waker 等 piece 完成,但无 peer 产出 piece),
检查点不可达 → 用户取消被忽略。

**现象**:用户点击取消后任务不响应,需等 stall_timeout(60s)触发后
经重试循环(每 60s × max_retries)才最终失败,约 240s。

### 1.3 缺陷三:死 swarm 无智能反馈(根因 C)

即使修复 A/B,stall_timeout 仍是唯一逃生舱:60s 静默等待 → Timeout →
重试 → 再 60s。用户在等待期间看到 0% 进度无任何说明,体验差。且
stall_timeout 无法区分"有 peer 但 piece 延迟"(应继续等)与"无 peer
死 swarm"(应快速失败或智能等待)。

## 2. 根因分析(逐行验证)

### 2.1 死锁路径(根因 A)

```
execute_fragmented_download:
  1005: let (frag_tx, mut frag_rx) = mpsc::channel(worker_count * 2);
  1025: for spec in &fragment_specs {           // ← 入队循环
  1034:     frag_tx.send(*spec).await           // ← 容量满后永久挂起
  1040: drop(frag_tx);
  1047: let dispatcher_handle = tokio::spawn(...) // ← 消费者,但永远到不了
  1100: for ... handles.spawn(...)               // ← worker,也到不了
```

诊断证据:加临时 `info!` 日志到 dispatcher/worker 入口,死 swarm 下
**三个日志全未出现**,证明执行流卡在入队循环,未到 spawn。

### 2.2 取消不穿透路径(根因 B)

```
download_single_fragment:
  1637: while let Some(chunk_result) = stream.next().await {  // ← 裸 await
  1639:     if let Some(rx) = control_rx.as_mut() {
  1640:         if control_check_countdown == 0 {
  1641:             Self::wait_control_rx(...)  // ← 死 swarm 下不可达
```

对比:`probe()`/`prepare_storage()`/`verify()`/`write_all_at_mut` 全部
用 `tokio::select!` + `watch_for_interrupt` 正确处理取消,唯独流读取循环
是裸 await。

### 2.3 librqbit FileStream 死 swarm 行为(根因 C)

`FileStream::poll_read`(streaming.rs:166)在 piece 未就绪时:
- `chunk_tracker.get_have_pieces()[current.id] == false`
- `register_waker(stream_id, cx.waker())`
- 返回 `Poll::Pending`

waker 只在 `wake_streams_on_piece_completed` 调用时唤醒 —— 即 piece
真正下载完成时。死 swarm 无 peer 产出 piece → waker 永不唤醒 →
`poll_read` 永久 Pending。

librqbit 暴露 `stats_snapshot().peer_stats.{live,connecting}` 可判断
swarm 是否有活跃 peer,这是智能等待的基础。

## 3. 解决方案

### 3.1 dispatcher spawn 顺序重排(修复 A)

将 dispatcher 和 worker spawn 移到入队循环**之前**:

```
1019: wait_control_rx 检查
1047: tokio::spawn(dispatcher)    // ← 先 spawn 消费者
1100: for ... handles.spawn(worker) // ← 先 spawn worker
1273: drop(completed_tx)
1276: for spec in &fragment_specs { // ← 入队在 spawn 之后
1285:     frag_tx.send(*spec).await  // ← dispatcher 已在 recv,不会死锁
1295: drop(frag_tx)
```

验证:诊断测试显示 4 个 worker 立即启动("开始下载分片" index=0,1,2,3),
stall_timeout 15s 后触发,最终 run() 返回 `Err(Timeout)`。

### 3.2 流读取循环 select! 化(修复 B)

将裸 `while let stream.next().await` 改为:

```rust
loop {
    let chunk_result = if let Some(rx) = control_rx.as_mut() {
        tokio::select! {
            chunk = stream.next() => match chunk {
                Some(r) => r,
                None => break,
            },
            interrupt = Self::watch_for_interrupt(rx, pause_timeout) => {
                interrupt?;
                return Err(DownloadError::Other("控制信号异常结束".into()));
            }
        }
    } else { /* 裸 await,无 control_rx 路径 */ };
    // ... 原循环体保留
}
```

cancel-safe:`StreamExt::next` 仅持有 `&mut stream`,被 select! 取消时
无部分状态。与 `write_all_at_mut` 同构模式。

额外:`run_inner` 步骤 4 的 `execute()` 也用 `select!` + `wait_for_cancel`
包裹,作纵深防御(与步骤 1/3/5 同构)。

### 3.3 peer 健康监控 + 智能等待(修复 C)

#### 3.3.1 PeerHealthSource trait

```rust
pub trait PeerHealthSource: Send + Sync {
    /// 是否有活跃 peer(已连接或正在连接)
    fn healthy(&self) -> bool;
}
```

生产实现 `ManagedTorrentPeerHealth` 包装
`handle.live()?.stats_snapshot().peer_stats.{live + connecting} > 0`。
测试实现 `MockPeerHealth` 用原子 bool 模拟。

#### 3.3.2 make_chunk_stream 超时分层

```
┌─────────────────────────────────────────────────────────┐
│              make_chunk_stream(reader, ...)              │
│                                                          │
│  ┌─ unfold loop ──────────────────────────────────────┐ │
│  │                                                     │ │
│  │  tokio::time::timeout(stall, reader.read())         │ │
│  │      │                                             │ │
│  │      ├─ Ok(Ok(n)) → yield Ok(Bytes), 重置 no_peer  │ │
│  │      ├─ Ok(Ok(0)) → None (EOF)                     │ │
│  │      ├─ Ok(Err)  → yield Err(Io)                   │ │
│  │      └─ Err(超时) → 检查 peer_health:             │ │
│  │          ├─ None/healthy → yield Err(Timeout,       │ │
│  │          │   "stall 超时,有 peer 但无数据")        │ │
│  │          └─ 不健康 → 累计 no_peer += 5s:           │ │
│  │              ├─ < peer_wait → sleep(5s), loop 重试  │ │
│  │              └─ ≥ peer_wait → yield Err(Timeout,    │ │
│  │                  "无可用 peer,等待 N秒后超时")     │ │
│  └─────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────┘
```

关键设计:
- **None = 未启用 peer 监控**:回退纯 stall_timeout 行为(向后兼容)
- **有 peer + read 超时**:产出 stall Timeout,让引擎重试
- **无 peer + 累计 < peer_wait**:sleep 轮询间隔后 loop 重试(不产出空项)
- **无 peer + 累计 ≥ peer_wait**:产出"无可用 peer"Timeout

`peer_wait` 给死 swarm 恢复窗口:tracker 重试 60s,DHT 重建 1-2min,
默认 5 分钟平衡恢复概率与用户体验。

### 3.4 DHT 持久化配置开关

`MagnetConfig.disable_dht_persistence`(默认 false)贯穿到 `BtSession::new`
的 `SessionOptions.disable_dht_persistence`。测试环境/沙箱下持久化文件
可能因文件锁失败导致 Session 创建报错,此开关允许禁用(纯内存 DHT)。

前端设置页 magnet tab 加 ToggleItem,经 `MagnetPatch` 白名单 patch
热切换 BtSession。

## 4. 配置项

| 字段 | 默认 | 范围 | 说明 |
|------|------|------|------|
| `stall_timeout_secs` | 60 | 0-86400 | 单次 read 超时上限(有 peer 时) |
| `peer_wait_timeout_secs` | 300 | 0-3600 | 无 peer 时智能等待总限 |
| `disable_dht_persistence` | false | bool | 禁用 DHT 持久化 |

0 = 禁用(stall/peer_wait 用 `Duration::MAX` 零开销,向后兼容)。

## 5. 测试策略

### 5.1 协议层(tachyon-protocol)

| 测试 | 验证点 |
|------|--------|
| `test_make_chunk_stream_stall_timeout_triggers` | None 回退 stall_timeout |
| `test_make_chunk_stream_stall_disabled_reads_data` | Duration::MAX 零开销 |
| `test_make_chunk_stream_stall_does_not_fire_on_active_stream` | 活跃流不误触发 |
| `test_make_chunk_stream_peer_dead_triggers_peer_wait_timeout` | 无 peer → peer_wait 超时 |
| `test_make_chunk_stream_peer_recovered_avoids_peer_wait_timeout` | peer 恢复 → 不触发 peer_wait |
| `test_make_chunk_stream_peer_healthy_does_not_trigger` | 有 peer + 数据正常 |

### 5.2 引擎层(tachyon-engine)

| 测试 | 验证点 |
|------|--------|
| `test_fragments_exceeding_channel_capacity_do_not_deadlock` | 死锁回归(10 分片 > 容量 4) |
| `test_cancel_signal_interrupts_stalled_stream_read` | 取消穿透死 swarm |
| `test_cancel_signal_interrupts_blocked_fragment_storage_write` | 取消穿透存储写 |

### 5.3 配置层(tachyon-core)

| 测试 | 验证点 |
|------|--------|
| `test_magnet_patch_disable_dht_persistence_applies` | patch 往返 |
| `test_magnet_patch_peer_wait_timeout_applies` | patch 往返 |
| `test_magnet_config_validate_peer_wait_timeout` | 边界校验(0 合法,超限报错) |

### 5.4 前端(tachyon-app)

| 测试 | 验证点 |
|------|--------|
| `切换禁用 DHT 持久化开关后保存` | toggle → draft → buildPatch 往返 |
| `确认保存时调用 api.updateConfig` | magnet patch 含 disableDhtPersistence |

### 5.5 真实磁力链接诊断(已移除)

开发期间用真实磁力链接(`magnet:?xt=urn:btih:T5HBWLNLCLFKXZZ6GHZUZ4NX7O3U5F6P...`)
跑端到端诊断测试,捕获 debug 日志定位卡点。修复后确认:worker 立即启动 →
stall_timeout 15s 触发 → 重试 → 最终失败(state=Failed),不再永久挂起。
诊断测试文件已移除(依赖外部网络,不进 CI)。

## 6. 经验教训(已记入 AGENTS.md)

1. **dispatcher/consumer spawn 必须在 producer 入队之前**:channel 容量
   < item 数时必现死锁。producer 的 `send().await` 需要消费者已 spawn。
2. **BT 死 swarm 检测放协议层**:引擎层不感知 peer 概念,只看到 ByteStream。
   协议层用 `PeerHealthSource` trait 注入,符合"该加超时的地方自己加"原则。
3. **unfold 内 loop 而非 yield 空项**:智能等待期间不应产出空 Bytes
   (会污染下游空写入),应在 unfold 闭包内 loop + sleep 重试。
4. **stall_timeout 可重试是合理的**:BT swarm 瞬时 stall 后重试可能恢复,
   改不可重试会误杀。peer_wait 智能等待作为主要逃生,stall 降为二级保险。

## 7. BT 代理支持(2026-07-02 追加)

### 7.1 问题

迅雷/motrix 能下载的磁力链接,Tachyon 不能下载。诊断发现根因:

- 用户系统设了 `HTTP_PROXY=http://127.0.0.1:7897`(Clash 系统代理)
- librqbit 的 reqwest 默认读系统代理 → HTTP tracker 经 Clash 代理
- 但 **UDP tracker / DHT / peer TCP 连接直连**(socks5 未配置)
- 国内访问国外 BT tracker/peer 被墙 → 死 swarm → 下载不动
- 迅雷/motrix 让 BT 流量走代理或用私有 peer 源,所以能下

### 7.2 解决方案

librqbit 已支持 `SessionOptions.socks_proxy_url`(`socks5://host:port`),
配置后 HTTP tracker(reqwest proxy)和 peer TCP(StreamConnector)都走 socks5。

#### 7.2.1 配置项

`MagnetConfig.socks_proxy_url: Option<String>`:
- `None`:自动检测系统代理(`detect_socks_proxy()`)
- `Some("socks5://host:port")`:用户手动配置

#### 7.2.2 自动检测 `detect_socks_proxy()`

检测顺序(取首个非空):
1. `ALL_PROXY` —— `socks5://` 直接用;`http://host:port` 转 `socks5://host:port`
2. `HTTPS_PROXY` / `HTTP_PROXY` —— 同样转 `socks5://`

Clash/V2Ray 混合端口(如 7897)同时支持 HTTP 和 SOCKS5,已验证。
非混合端口代理连接失败时 librqbit 报错,不静默失败。

#### 7.2.3 应用

`BtSession::new` 中:`config.socks_proxy_url` 优先,None 时调 `detect_socks_proxy()`。
配置变更时 BtSession 热切换(`magnet_changed` 触发,已有逻辑)。

### 7.3 限制

- **UDP tracker/DHT 仍直连**:socks5 不代理 UDP。但 BT 主要 peer 来源是
  HTTP tracker + peer 交换,够用。DHT 在有 tracker 的情况下非必需。
- **自动检测有假设性**:把 `http://` 转 `socks5://` 假设端口支持 SOCKS5。
  Clash 混合端口支持,传统 HTTP 代理不支持(连接报错)。

### 7.4 测试

| 测试 | 验证点 |
|------|--------|
| `test_magnet_patch_socks_proxy_url_applies` | patch 往返(设值 + 清空) |
| `test_magnet_config_validate_socks_proxy_url` | scheme/host/port 校验 |
| `test_detect_socks_proxy_from_all_proxy_socks5` | ALL_PROXY socks5 直接用 |
| `test_detect_socks_proxy_from_http_proxy_convert` | HTTP_PROXY 转 socks5 |
| `test_detect_socks_proxy_none_when_unset` | 无代理返回 None |
| 前端 `填写 SOCKS5 代理后保存` | toggle → draft → buildPatch |
