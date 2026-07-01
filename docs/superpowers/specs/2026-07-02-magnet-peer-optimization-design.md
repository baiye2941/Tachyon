# 设计:磁力下载全流程 Peer 优化

> 日期: 2026-07-02
> 状态: 设计待审
> 范围: tachyon-core / tachyon-protocol / tachyon-engine / tachyon-app(前端)
> 方法论: 头脑风暴 + 论文/博客研究(2 个并行研究 Agent)+ 架构审查 Agent 交叉验证

## 1. 问题背景

Tachyon 磁力下载已有死 swarm 韧性(`stall_timeout` + `peer_wait` + `PeerHealthSource`)、
SOCKS5 代理、DHT 持久化配置开关(见 `docs/sdd/magnet-dead-swarm-resilience.md`)。
但全流程仍有三类未解决的瓶颈:

### 1.1 配置面浪费 — librqbit 已暴露但 Tachyon 未用的能力

librqbit 8.1.1 的 `SessionOptions`/`AddTorrentOptions` 暴露了 9 项未用配置
(`peer_opts`、`force_tracker_interval`、`defer_writes_up_to`、`initial_peers` 等),
Tachyon 只用了 `disable_dht`/`enable_upnp`/`socks_proxy_url`/`trackers` 4 项。
死 peer 占用连接槽位 10s(librqbit 默认 `connect_timeout`)、tracker 30min-2h 才重 announce、
慢盘写入阻塞 peer 读取循环,均可通过配置填充缓解。

### 1.2 协议层盲区 — 无可观测性 + SOCKS5 下 UDP tracker 失效

Tachyon 对 BT 层完全"盲"(只有 HTTP mirror 的 `SourceStats`)。死 swarm 时用户看到 0% 进度
无任何说明。SOCKS5 仅代理 TCP,UDP tracker 直连被墙,而 `default_trackers()` 含 UDP tracker。

### 1.3 冷启动无 peer 等待期 — P2SP 混合下载缺失

磁力链接冷启动需等 DHT bootstrap + tracker 查询 + peer 握手(通常 20-40s,死 swarm 60s+)。
若资源同时有 HTTP 直链(网盘镜像、CDN),HTTP 可立即提供数据,但当前 BT 与 HTTP 镜像互斥
(`with_mirrors` 硬编码 `bt_session: None`,downloader.rs:381)。

## 2. 研究结论(论文 + 博客交叉验证)

两个并行研究 Agent 一致表明:**高价值优化在 peer 发现与网络可达性,而非 choking/piece 算法替换**。

- **论文 cs/0609026**(Legout et al.):rarest-first + choke 已接近理论最优,替换算法收益有限。
  libtorrent 已把 `bittyrant_choker` 标 deprecated。→ 不投入算法替换。
- **librqbit 8.1.1 现状**:无 choking(无条件 unchoke)、纯顺序 piece 选择(有 `try_steal` 速度比
  偷片)、`peer_limit` 硬编码 128、SOCKS5 仅 TCP、不支持 BEP-19 webseed。→ 算法层改动需 fork,成本高。
- **BEP-19 webseed**:HTTP webseed 是"永久 unchoke 的 seed",可保证冷启动有数据源。但 librqbit
  8.1.1 不支持,需在 Tachyon 引擎层自研混合下载。
- **BEP-12 多 tracker + BEP-11 PEX**:tracker 冗余 + PEX 是死 swarm 恢复主力,不依赖算法改动。

## 3. 方案选择

### 3.1 候选方案对比

| 方案 | 思路 | 优点 | 缺点 |
|------|------|------|------|
| A 渐进式分层注入 | 配置层→协议层→P2SP 三阶段 | 复用 MirrorProtocol,每阶段独立交付 | probe 语义需处理 |
| B 协议层统一抽象 | 新建 HybridProtocol 枚举 | 语义清晰 | 与 MirrorProtocol 重叠,改动大 |
| C fork librqbit | 全栈算法优化 | 收益最高 | 论文证明收益有限,fork 维护成本高 |

**选定方案 A**(用户批准),因最大化复用现有调度框架,每阶段独立交付。

### 3.2 架构审查修正(交叉验证)

架构审查 Agent 基于真实源码核验,发现原设计 5 处缺陷,已修正:

| # | 原设计缺陷 | 核验依据 | 修正 |
|---|-----------|---------|------|
| 1 | `dht_config.bootstrap_addrs` | `PersistentDhtConfig` 只有 `dump_interval`+`config_filename`,无 `bootstrap_addrs` | 删除,依赖 librqbit 内置 2 节点 + DHT 持久化 |
| 2 | `force_reannounce` API | 整个 librqbit-8.1.1 源码树零命中 | 删除 SwarmRevival,改被动超时+引擎重试 |
| 3 | BT 当普通源塞进 least-in-flight | BT `download_range_stream` 立即返回 stream(in_flight+1)但 piece 可能 stall 60s;`in_flight` 是数量非带宽;`StatsStream` EOF 才记 quality(滞后) | P2SP 改为 HTTP 主源 + BT fallback |
| 4 | probe 主源优先等待 | BT metadata 需等 120s,HTTP 毫秒级 | HTTP 先返回,BT 后台拿 metadata |
| 5 | `Protocol::diagnostics()->serde_json::Value` | 无类型逃逸口,污染 core trait 依赖 | MagnetProtocol 直接暴露强类型方法 |

## 4. 设计

### 4.1 整体架构

```
┌─────────────────────────────────────────────────────────────┐
│ 阶段三: P2SP 混合下载(HTTP 主源 + BT fallback)             │
│   HTTP 镜像主源(消除冷启动等待)→ MirrorProtocol 调度       │
│   BT 整文件 fallback(HTTP 全失败时接管)                     │
│   layout 兼容校验:仅单文件+大小一致才混合                   │
├─────────────────────────────────────────────────────────────┤
│ 阶段二: 协议层增强(peer 发现 + 可观测性)                  │
│   ① MagnetProtocol 暴露强类型 peer_stats_snapshot()         │
│   ② SOCKS5 下过滤 UDP tracker + 追加 HTTPS tracker          │
│   ③ SOCKS5 下可选禁用 DHT(避免 UDP 超时无谓等待)           │
├─────────────────────────────────────────────────────────────┤
│ 阶段一: 配置层快速收益(librqbit 已暴露但 Tachyon 未用)     │
│   ① 预置公共 tracker 列表    ② peer_opts 超时调优            │
│   ③ force_tracker_interval   ④ defer_writes_up_to            │
│   ⑤ initial_peers 持久化(冷启动加速)                       │
└─────────────────────────────────────────────────────────────┘
```

### 4.2 设计原则

1. **复用优先**:P2SP 复用 `MirrorProtocol` 的 HTTP 多源 least-in-flight 调度,BT 走独立 fallback 路径
2. **该加的地方自己加**:协议层增强放 `tachyon-protocol`,配置层放 `tachyon-core::config`
3. **不强求平等调度**:承认 BT 的状态性(自管 storage + piece 哈希 + 异步慢启动)与
   MirrorProtocol 的无状态字节流假设不兼容,BT 不参与 per-fragment least-in-flight
4. **配置分级**:区分"需重建 Session"与"可热切换"配置,前端 UI 显式提示

### 4.3 阶段一:配置层快速收益

#### 4.3.1 新增 MagnetConfig 字段

```rust
// crates/tachyon-core/src/config.rs
pub struct MagnetConfig {
    // ... 现有字段保留 ...

    /// peer 连接超时(秒),默认 8(快于 librqbit 默认 10s 淘汰死 peer)
    #[serde(default = "default_peer_connect_timeout_secs")]
    pub peer_connect_timeout_secs: u64,

    /// peer 读写超时(秒),默认 10(与 librqbit 默认一致)
    #[serde(default = "default_peer_read_write_timeout_secs")]
    pub peer_read_write_timeout_secs: u64,

    /// 强制 tracker 重新 announce 间隔(秒),默认 120
    ///
    /// librqbit 默认遵循 tracker 返回的 interval(通常 30min-2h),
    /// 冷启动后 peer 池更新慢。强制较短间隔可更频繁发现新 peer。
    /// 0 表示禁用(遵循 tracker 默认 interval)。
    #[serde(default = "default_force_tracker_interval_secs")]
    pub force_tracker_interval_secs: u64,

    /// 延迟写入缓冲上限(MB),默认 16(慢盘优化)
    ///
    /// librqbit 攒到指定 MB 后批量落盘,减少 peer 读取循环的 I/O 等待。
    /// 0 表示禁用(同步写入)。
    #[serde(default = "default_defer_writes_up_to_mb")]
    pub defer_writes_up_to_mb: u64,

    /// SOCKS5 启用时是否禁用 DHT(默认 true)
    ///
    /// DHT 走 UDP 直连,SOCKS5 不代理 UDP,国内墙下 DHT 不可达。
    /// 禁用 DHT 避免无谓的 UDP 超时等待,完全依赖 HTTPS tracker + PEX + initial_peers。
    /// false 时保留 DHT(适合无墙环境或 UDP 可达场景)。
    #[serde(default = "default_true")]
    pub disable_dht_when_socks: bool,
}
```

默认值:
- `default_peer_connect_timeout_secs()` → `8`
- `default_peer_read_write_timeout_secs()` → `10`
- `default_force_tracker_interval_secs()` → `120`
- `default_defer_writes_up_to_mb()` → `16`
- `default_trackers()` → 预置 6 个公共 tracker(含 UDP + HTTPS)

预置 tracker 列表:
```rust
fn default_trackers() -> Vec<String> {
    vec![
        "udp://tracker.opentrackr.org:1337/announce".into(),
        "udp://open.demonii.com:1337/announce".into(),
        "udp://open.stealth.si:80/announce".into(),
        "udp://exodus.desync.com:6969/announce".into(),
        "udp://tracker.torrent.eu.org:451/announce".into(),
        "https://tracker.tamersunion.org:443/announce".into(),
    ]
}
```

#### 4.3.2 BtSession::new 填充 SessionOptions

```rust
// crates/tachyon-engine/src/bt_session.rs
pub async fn new(download_dir: PathBuf, config: MagnetConfig) -> DownloadResult<Self> {
    let socks_enabled = config.socks_proxy_url.is_some()
        || tachyon_core::config::detect_socks_proxy().is_some();

    // SOCKS5 下按配置决定是否禁用 DHT(UDP 不可达)
    let disable_dht = if socks_enabled && config.disable_dht_when_socks {
        true
    } else {
        !config.enable_dht
    };

    let mut opts = SessionOptions {
        disable_dht,
        enable_upnp_port_forwarding: config.enable_upnp,
        disable_dht_persistence: config.disable_dht_persistence,
        peer_opts: Some(PeerConnectionOptions {
            connect_timeout: Duration::from_secs(config.peer_connect_timeout_secs),
            read_write_timeout: Duration::from_secs(config.peer_read_write_timeout_secs),
            ..Default::default()
        }),
        defer_writes_up_to: if config.defer_writes_up_to_mb == 0 {
            None
        } else {
            Some(config.defer_writes_up_to_mb as usize)
        },
        ..Default::default()
    };
    // ... 现有 socks_proxy 逻辑保留 ...

    // tracker 注入:SOCKS5 下过滤 UDP,追加 HTTPS
    for tracker_url in &config.trackers {
        let is_udp = tracker_url.starts_with("udp://");
        if socks_enabled && is_udp {
            tracing::debug!(tracker = %tracker_url, "SOCKS5 启用,跳过 UDP tracker(不可达)");
            continue;
        }
        if let Ok(url) = url::Url::parse(tracker_url) {
            opts.trackers.insert(url);
        }
    }
    if socks_enabled {
        const HTTPS_TRACKERS_FOR_PROXY: &[&str] = &[
            "https://tracker.tamersunion.org:443/announce",
            "https://tracker.gbitt.info:443/announce",
        ];
        for https_tracker in HTTPS_TRACKERS_FOR_PROXY {
            if let Ok(url) = url::Url::parse(https_tracker) {
                opts.trackers.insert(url);
            }
        }
    }
    // ...
}
```

#### 4.3.3 add_magnet_to_session 填充 force_tracker_interval + initial_peers

```rust
// crates/tachyon-protocol/src/magnet.rs
async fn add_magnet_to_session(
    session: &Arc<Session>,
    url: &str,
    download_dir: &std::path::Path,
    force_tracker_interval: Duration,
    initial_peers: Vec<String>,  // 持久化的 peer 地址(info_hash 索引)
) -> DownloadResult<Arc<ManagedTorrent>> {
    let opts = AddTorrentOptions {
        overwrite: true,
        output_folder: Some(download_dir.to_string_lossy().into()),
        force_tracker_interval: Some(force_tracker_interval),
        initial_peers: if initial_peers.is_empty() {
            None
        } else {
            Some(initial_peers.iter()
                .filter_map(|s| s.parse().ok())
                .collect())
        },
        ..Default::default()
    };
    // ...
}
```

#### 4.3.4 initial_peers 持久化

任务结束时,`MagnetProtocol` 通过 `live().per_peer_stats_snapshot()`(librqbit 8.1.1 已确认存在)
导出活跃 peer 地址,按 info_hash 索引持久化到任务元数据。重启同 magnet 时注入
`AddTorrentOptions.initial_peers`,绕过冷启动 tracker/DHT 等待。

**注入点**:`DownloadTask` 在 verify(步骤 5)完成后,若 protocol 为 `MagnetProtocol`,调
`peer_stats_snapshot()` 导出 peer 列表,经 `ProgressBroker` 回传 app 层持久化。

**注意**:peer 地址可能过期,需 TTL 淘汰(默认 7 天)。app 层持久化路径与任务元数据一致。

#### 4.3.5 配置分级与热切换

| 配置项 | 生效方式 | 前端提示 |
|--------|---------|---------|
| `peer_connect_timeout`/`peer_read_write_timeout`/`defer_writes_up_to`/`disable_dht_when_socks` | 需重建 BtSession | "重启下载引擎生效" |
| `force_tracker_interval` | 仅影响新添加的 torrent | "对新任务生效" |
| `trackers`(全局) | 需重建 BtSession | "重启下载引擎生效" |
| `stall_timeout`/`peer_wait`/`metadata_timeout` | `MagnetProtocol` 每次 download 读 config | 可热切换 |

前端 SettingsPanel 对需重建的项打"重启生效"标记,避免用户误以为立即生效。

#### 4.3.6 校验

`MagnetConfig::validate()` 增加:
- `peer_connect_timeout_secs`: 1-300
- `peer_read_write_timeout_secs`: 1-600
- `force_tracker_interval_secs`: 0 或 30-3600
- `defer_writes_up_to_mb`: 0-256

`MagnetPatch` 增加对应 `Option<T>` 字段支持热切换(需重建的项热切换后标记 dirty,下次新建 Session 生效)。

### 4.4 阶段二:协议层增强

#### 4.4.1 stats 桥接 — 强类型 peer/piece 可观测性

`MagnetProtocol` 直接暴露强类型方法(不污染 `Protocol` trait):

```rust
// crates/tachyon-protocol/src/magnet.rs
impl MagnetProtocol {
    /// 采集 BT 层 peer/piece 统计快照
    ///
    /// 返回 None 表示 torrent 未 live 或快照获取失败(不影响下载流程)。
    /// 由 tachyon-app 层持有 MagnetProtocol 具体类型时调用(不经 dyn Protocol)。
    pub fn peer_stats_snapshot(&self, url: &str) -> Option<BtPeerStats> {
        let entry = self.handle_cache.get(url)?;
        let live = entry.0.live()?;
        let snap = live.stats_snapshot();
        Some(BtPeerStats {
            live_peers: snap.peer_stats.live,
            connecting_peers: snap.peer_stats.connecting,
            queued_peers: snap.peer_stats.queued,
            downloaded_bytes: snap.downloaded_bytes,
            uploaded_bytes: snap.uploaded_bytes,
        })
    }

    /// 导出活跃 peer 地址(供 initial_peers 持久化)
    ///
    /// 调用 librqbit 的 per_peer_stats_snapshot,提取已连接 peer 的 SocketAddr。
    pub fn export_peer_addrs(&self, url: &str) -> Vec<String> {
        let Some(entry) = self.handle_cache.get(url) else { return vec![] };
        let Some(live) = entry.0.live() else { return vec![] };
        live.per_peer_stats_snapshot()
            .into_iter()
            .filter_map(|(addr, _)| Some(addr.to_string()))
            .collect()
    }
}

/// BT 层 peer/piece 统计快照(跨 crate 传递,不依赖 librqbit 类型)
#[derive(Debug, Clone, Default)]
pub struct BtPeerStats {
    pub live_peers: usize,
    pub connecting_peers: usize,
    pub queued_peers: usize,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
}
```

**注入点**:app 层在构造 `DownloadTask` 时,若用 `with_hybrid_sources` 则同时持有
`Arc<MagnetProtocol>` 引用(构造时已创建),直接调 `peer_stats_snapshot()` 附加到 `ProgressEvent`。
不经 `dyn Protocol` 向下转型,避免抽象泄漏。纯 BT 路径(`with_pool_and_scheduler`)同理:
app 层在构造 `BtSession` 时已持有 `MagnetConfig`,可按需创建 `MagnetProtocol` 引用供诊断查询
(或 `DownloadTask` 新增 `bt_diagnostics: Option<Arc<MagnetProtocol>>` 字段,由 with_hybrid_sources
和 with_pool_and_scheduler 填充)。

#### 4.4.2 SOCKS5 tracker 过滤 + HTTPS 追加

见 4.3.2 `BtSession::new` 逻辑(已合并到阶段一,因两者强相关)。

#### 4.4.3 死 swarm 恢复策略(被动)

放弃主动重建(`force_reannounce` 不存在)。改为被动策略:

- `peer_wait_timeout`(默认 300s)超时 → 产出 `Err(Timeout)` → 引擎 cancel-aware 重试
- 重试时 `force_tracker_interval_secs`(默认 120s)确保 tracker 较频繁 announce
- `initial_peers` 持久化确保重试时跳过 tracker 等待直接连已知 peer

这仍达成死 swarm 恢复目标,且无 API 不存在风险、无线程安全问题。

### 4.5 阶段三:P2SP 混合下载

#### 4.5.1 核心模型:HTTP 主源 + BT fallback

经架构审查修正,放弃"BT 与 HTTP 平等参与 least-in-flight 分片调度"。新模型:

```
P2SP 混合下载流程:
  1. probe:HTTP 镜像并行竞速,首成功返回(毫秒级,消除冷启动等待)
           BT 后台继续拿 metadata(不阻塞 probe)
  2. layout 兼容校验:
     - 仅当 BT 单文件 + HTTP 单文件 + 大小一致 → 允许混合
     - BT 多文件或大小不一致 → 仅走 BT(不混合,避免偏移错位)
  3. 下载:
     - HTTP 镜像间走 MirrorProtocol 的 least-in-flight 调度(现有逻辑零改动)
     - BT 不参与 per-fragment 调度,作为整文件 fallback:
       所有 HTTP 镜像 probe 失败 或 所有 HTTP 镜像连续熔断 → 切 BT download_full_stream
  4. fallback 触发:
     - MirrorProtocol 的 probe_ok 为空 → 回退 BT
     - 下载中所有 HTTP 源 circuit breaker 熔断 → 回退 BT
```

**为何如此设计**:BT 的 `download_range_stream` 立即返回 stream 但数据依赖 piece 就绪可能 stall
60s。若塞进 least-in-flight,BT 会误占 fragment 槽位(in_flight 是数量非带宽,StatsStream EOF 才
记 quality,滞后严重),拖慢整体。HTTP 是无状态字节流,与 MirrorProtocol 假设天然兼容。

**仍达成"消除冷启动无 peer 等待期"目标**:HTTP 立即可用,无需等 BT metadata。

#### 4.5.2 新增构造路径 DownloadTask::with_hybrid_sources

```rust
// crates/tachyon-engine/src/downloader.rs
impl DownloadTask {
    /// 混合源下载(P2SP):HTTP 镜像主源 + BT fallback
    ///
    /// `magnet_url`:磁力链接(BT fallback 源)
    /// `http_mirrors`:HTTP 直链列表(主源,可为空 → 退化为纯 BT)
    /// `bt_session`:BT Session 单例(提供 MagnetProtocol 构造依赖)
    pub fn with_hybrid_sources(
        magnet_url: String,
        http_mirrors: Vec<String>,
        bt_session: Arc<BtSession>,
        config: Arc<Config>,
        pool: Option<Arc<ConnectionPool>>,
    ) -> DownloadResult<Self> {
        if http_mirrors.is_empty() {
            // 无 HTTP 镜像:退化为纯 BT
            return Self::with_pool_and_scheduler(
                magnet_url, bt_session, config, pool,
            );
        }

        // HTTP 镜像主源:塞入 MirrorProtocol(least-in-flight 调度)
        let mut http_sources: Vec<(String, Arc<dyn Protocol>)> = Vec::new();
        for (i, mirror_url) in http_mirrors.iter().enumerate() {
            let http_client = HttpClient::new(pool.clone(), config.http.clone())?;
            // index 0 = 主 HTTP 源
            http_sources.push((mirror_url.clone(), Arc::new(http_client)));
        }
        let mirror_proto = MirrorProtocol::new(
            http_mirrors[0].clone(),
            http_sources,
        );

        // BT fallback:MagnetProtocol 独立持有,不塞入 MirrorProtocol
        let magnet_proto = Arc::new(MagnetProtocol::new(
            bt_session.session(),
            bt_session.config().clone(),
            bt_session.download_dir().clone(),
        ));

        Self::with_protocol_and_bt_fallback(
            Arc::new(mirror_proto),   // 主协议(HTTP 多源)
            magnet_proto,             // BT fallback
            magnet_url,
            bt_session,
            config,
            pool,
        )
    }
}
```

#### 4.5.3 probe 语义:HTTP 先返回 + layout 兼容校验

保持 `MirrorProtocol::probe` 现有"首成功即返回"语义(HTTP 毫秒级返回),BT 后台拿 metadata。

新增 `with_protocol_and_bt_fallback` 在 probe 后做 layout 兼容校验:

```rust
// crates/tachyon-engine/src/downloader.rs
async fn run_inner(&mut self) -> DownloadResult<()> {
    // 步骤1: HTTP probe(主源)
    let http_meta = self.protocol.probe(&self.url).await?;

    // 步骤1.5: BT 后台 probe(不阻塞,fallback 用)
    if let Some(bt_proto) = &self.bt_fallback {
        let bt_proto = Arc::clone(bt_proto);
        let url = self.url.clone();
        tokio::spawn(async move {
            // 后台拿 BT metadata,不阻塞 HTTP 下载
            let _ = bt_proto.probe(&url).await;
        });
    }

    // 步骤2: init_storage(用 HTTP meta 的 layout)
    self.init_storage(&http_meta)?;

    // 步骤3-4: execute(HTTP 主源分片下载)
    // 若 HTTP 全失败 → 切 BT fallback(见 4.5.4)
    // ...
}
```

**layout 兼容校验**:BT metadata 就绪后(后台),若 BT 是多文件或大小与 HTTP 不一致,
标记 BT fallback 为"不可用"(仅 HTTP)。仅单文件 + 大小一致才允许 BT fallback。

#### 4.5.4 BT fallback 触发

```rust
// crates/tachyon-engine/src/downloader.rs — execute 阶段
async fn execute_fragmented_download(&mut self) -> DownloadResult<()> {
    // 现有 HTTP 多源分片调度逻辑...
    // worker 失败重试时,若所有 HTTP 源熔断:

    match self.execute_with_http_sources().await {
        Ok(()) => Ok(()),
        Err(e) if self.all_http_sources_circuit_broken() => {
            tracing::warn!("所有 HTTP 源熔断,回退 BT fallback");
            self.execute_with_bt_fallback().await
        }
        Err(e) => Err(e),
    }
}

async fn execute_with_bt_fallback(&mut self) -> DownloadResult<()> {
    let bt_proto = self.bt_fallback.as_ref()
        .ok_or_else(|| DownloadError::Other("无 BT fallback".into()))?;
    // BT 走 download_full_stream(整文件,不参与分片调度)
    let stream = bt_proto.download_full_stream(&self.url).await?;
    // 写入 engine storage(与 HTTP 路径相同的 write_all_at_mut)
    self.write_stream_to_storage(stream).await
}
```

#### 4.5.5 配置层入口

```rust
// app 层 task_commands.rs — build_download_task 扩展
pub async fn build_download_task(
    url: String,
    mirror_urls: Option<Vec<String>>,
    bt_session: Option<Arc<BtSession>>,
    ...
) -> DownloadResult<DownloadTask> {
    let is_magnet = url.starts_with("magnet:?");
    let has_mirrors = mirror_urls.as_ref().is_some_and(|v| !v.is_empty());

    if is_magnet && has_mirrors && bt_session.is_some() {
        // P2SP 混合:HTTP 镜像主源 + BT fallback
        DownloadTask::with_hybrid_sources(
            url,
            mirror_urls.unwrap(),
            bt_session.unwrap(),
            config,
            pool,
        )
    } else if is_magnet {
        // 纯 BT(现有路径)
        DownloadTask::with_pool_and_scheduler(...)
    } else {
        // 纯 HTTP 或 HTTP 多源(现有路径)
        ...
    }
}
```

#### 4.5.6 错误处理与降级

- **所有 HTTP 镜像 probe 失败**:回退纯 BT(`with_pool_and_scheduler`)。此判定在 `with_hybrid_sources` 构造时完成,不进入混合路径。
- **BT fallback 不可用**(后台 probe 未就绪或多文件 layout 冲突):仅走 HTTP 多源(现有 `with_mirrors` 行为)。BT probe 是后台 spawn 的(4.5.3),HTTP 全熔断触发 fallback 时若 BT metadata 未就绪,`download_full_stream` 自然报错(不阻塞 HTTP 已下载部分)。
- **混合下载中 HTTP 全熔断**:切 BT `download_full_stream`。
- **BT fallback 也失败**:返回最后一个错误。

### 4.6 双存储说明

纯 BT 路径已存在双存储:librqbit 写 BT storage(`download_dir`),引擎读 FileStream 再写
engine StorageSet(用户保存路径)。P2SP 混合未让 BT 双写变得更糟——BT fallback 时走
`download_full_stream`(与纯 BT 相同的双写路径)。HTTP 主源直接写 engine storage(无 BT storage)。

**磁盘空间**:BT fallback 触发时,BT storage 会占用额外空间(与纯 BT 相同)。下载完成后
BT storage 可清理(`download_dir` 下的 torrent 文件)。前端可提示用户清理。

## 5. 测试策略

### 5.1 配置层(tachyon-core)

| 测试 | 验证点 |
|------|--------|
| `test_magnet_patch_peer_connect_timeout_applies` | patch 往返 |
| `test_magnet_patch_force_tracker_interval_applies` | patch 往返 |
| `test_magnet_patch_defer_writes_up_to_applies` | patch 往返 |
| `test_magnet_patch_disable_dht_when_socks_applies` | patch 往返 |
| `test_magnet_config_validate_peer_connect_timeout` | 边界校验(1-300) |
| `test_magnet_config_validate_force_tracker_interval` | 边界校验(0 或 30-3600) |
| `test_default_trackers_not_empty` | 默认 tracker 预置 |
| `test_default_trackers_contains_https` | 含 HTTPS(SOCKS5 可达) |

### 5.2 协议层(tachyon-protocol)

| 测试 | 验证点 |
|------|--------|
| `test_peer_stats_snapshot_returns_none_for_unknown_url` | 未知 url 返回 None |
| `test_peer_stats_snapshot_returns_some_for_live_torrent` | 离线预置 torrent 返回 Some |
| `test_export_peer_addrs_empty_for_offline_torrent` | 离线预置(无真实 peer)返回空 |
| `test_add_magnet_with_force_tracker_interval` | AddTorrentOptions 含 force_tracker_interval |
| `test_add_magnet_with_initial_peers` | AddTorrentOptions 含 initial_peers |

### 5.3 引擎层(tachyon-engine)

| 测试 | 验证点 |
|------|--------|
| `test_bt_session_new_with_peer_opts` | SessionOptions 含 peer_opts |
| `test_bt_session_new_with_defer_writes` | SessionOptions 含 defer_writes_up_to |
| `test_bt_session_filters_udp_trackers_when_socks` | SOCKS5 下 UDP tracker 被过滤 |
| `test_bt_session_appends_https_trackers_when_socks` | SOCKS5 下追加 HTTPS tracker |
| `test_bt_session_disables_dht_when_socks` | SOCKS5 + disable_dht_when_socks → DHT 禁用 |
| `test_with_hybrid_sources_falls_back_to_bt` | HTTP 全熔断 → BT fallback |
| `test_with_hybrid_sources_layout_mismatch_rejects_bt` | BT 多文件 + HTTP 单文件 → 拒绝 BT |
| `test_with_hybrid_sources_no_mirrors_degrades_to_bt` | 无 HTTP 镜像 → 纯 BT |

### 5.4 P2SP 离线测试

用 `FileBackedMockProtocol`(读真实文件的 HTTP mock)+ `MagnetProtocol`(离线预置 BT)验证:
- HTTP 主源快速完成,BT fallback 不触发
- HTTP mock 返回错误 → BT fallback 接管
- BT 多文件 + HTTP 单文件 layout 冲突 → 拒绝混合

### 5.5 前端(tachyon-app)

| 测试 | 验证点 |
|------|--------|
| `修改 peer 连接超时后保存` | toggle → draft → buildPatch |
| `修改延迟写入缓冲后保存` | toggle → draft → buildPatch |
| `切换 SOCKS5 禁用 DHT 开关后保存` | toggle → draft → buildPatch |
| `需重建生效的配置项显示标记` | UI 显示"重启生效"标记 |

## 6. 变更范围

| 阶段 | crate | 改动文件 | 复杂度 | 工作量 |
|------|-------|---------|--------|--------|
| 一 | tachyon-core | `config.rs`(新增字段+默认值+校验+MagnetPatch) | 低 | 0.5 天 |
| 一 | tachyon-engine | `bt_session.rs`(填充 SessionOptions + tracker 过滤) | 低 | 0.5 天 |
| 一 | tachyon-protocol | `magnet.rs`(force_tracker_interval + initial_peers) | 低 | 0.5 天 |
| 二 | tachyon-protocol | `magnet.rs`(peer_stats_snapshot + export_peer_addrs) | 中 | 0.5 天 |
| 二 | tachyon-engine | `downloader.rs`(BT 层指标桥接到 ProgressEvent) | 中 | 0.5 天 |
| 三 | tachyon-engine | `downloader.rs`(with_hybrid_sources + bt_fallback 字段 + execute 降级) | 中-高 | 1.5 天 |
| 三 | tachyon-app | `task_commands.rs`(build_download_task 路由) | 低 | 0.5 天 |
| 前端 | tachyon-app | `SettingsPanel.tsx` + i18n + types.ts | 低 | 0.5 天 |
| 测试 | 多 crate | 补单测 + 离线集成测试 + FileBackedMockProtocol | — | 1.5 天 |
| **合计** | | | | **~6 天** |

## 7. 经验教训

1. **API 可用性必须源码核验**:设计阶段基于文档/记忆假设的 librqbit API
   (`dht_config.bootstrap_addrs`、`force_reannounce`)经源码核验不存在。架构审查 Agent
   逐字段核实 8.1.1 源码是关键防线。→ 任何依赖第三方库 API 的设计 MUST 源码核验。
2. **BT 的状态性与无状态调度假设不兼容**:BT 自管 storage + piece 哈希 + 异步慢启动,
   不是"普通 Protocol 源"。强行塞进为 HTTP 设计的 least-in-flight 会导致误分配
   (in_flight 是数量非带宽,stats 滞后)。→ P2SP 应让 HTTP 主导,BT 作 fallback。
3. **论文指导方向**:cs/0609026 证明算法替换收益有限,高价值在 peer 发现与网络可达性。
   → 不投入 choking/picker 算法 fork。
4. **配置分级避免用户困惑**:需重建 Session 的配置项前端 MUST 显式提示"重启生效"。
5. **initial_peers 持久化是死 swarm 恢复的高性价比方案**:不依赖不存在的 API,
   利用 `AddTorrentOptions.initial_peers`(8.1.1 已确认存在)+ `per_peer_stats_snapshot`
   导出,绕过冷启动 tracker/DHT 等待。

## 8. 后续演进(本期不做)

1. **SOCKS5 UDP associate**:fork librqbit 让 DHT/uTP 走 SOCKS5 UDP,解决国内 DHT 不可达根因。
   成本高,本期用"禁用 DHT + HTTPS tracker + initial_peers"绕过。
2. **rate-based choking + rarest-first**:fork librqbit 实现,论文证明收益有限,优先级低。
3. **BT piece 注入 HTTP 数据**:让 HTTP range 下载的数据写入 BT storage 并触发哈希校验,
   实现"HTTP 预填 piece + BT 补带宽"的真正 P2SP。librqbit 8.1.1 未公开 piece 级写入 API,成本高。
