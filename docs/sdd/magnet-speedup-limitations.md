# 磁力链接提速限制与决策

## 背景

深度审查阶段评估了磁力链接下载提速的多个方向(T2/T5/T7/T8),基于
librqbit 8.1.1 公开 API 的实际能力做了决策记录。

## librqbit 8.1.1 API 限制

源码位置:`C:/cargo/registry/src/index.crates.io-*/librqbit-8.1.1/`

### T7: DHT bootstrap 节点自定义 — 不可行

librqbit 默认 DHT bootstrap 节点只有 2 个(国内网络下常不可达):

```rust
// librqbit-dht-5.3.1/src/lib.rs:38
pub static DHT_BOOTSTRAP: &[&str] = &[
    "dht.transmissionbt.com:6881",
    "dht.libbrew.org:25401",
];
```

底层 `DhtConfig.bootstrap_addrs: Option<Vec<String>>`(librqbit-dht-5.3.1/src/dht.rs:1138)
支持自定义,但 `SessionOptions.dht_config` 类型是 `Option<PersistentDhtConfig>`,
该结构只有 `dump_interval` 和 `config_filename` 两个字段,**不含 bootstrap_addrs**。

`Session::new_with_opts` 内部构造 `DhtConfig { ..Default::default() }` 时
显式取 `bootstrap_addrs: None`,用户无法通过 `SessionOptions` 注入。

**结论(8.1.1)**:要改 bootstrap,需 fork librqbit 或直接用 `librqbit-dht` crate 自建
`DhtConfig { bootstrap_addrs: Some(...) }`,但这绕过了 librqbit 的 Session 封装,
需自行实现 DHT + tracker + peer 管理的协调逻辑,工作量与维护风险不匹配。

**2026-07-24 更新**:`librqbit` **9.0.0-rc.0** 已新增 `DhtSessionConfig.bootstrap_addrs`
与 `SessionOptions.dht: Option<DhtSessionConfig>`,**无需 fork** 即可注入 bootstrap。
默认列表仍为 2 节点;升级 9.x stable 后由 Tachyon `MagnetConfig` 暴露可选列表即可关闭 P-04。
完整评估见 `docs/sdd/librqbit-upgrade-and-pgo-eval.md`(结论:**不 fork 8.1.1,等 9 stable 升级**)。

### T8: BEP-6 Fast Extension — 不可行

librqbit 8.1.1 **未实现** BEP-6 Fast Extension:
- 握手 reserved bits 只设置 extended messaging 位(bit 20),未设置 Fast Extension 位(bit 2)
- 无 `enable_fast_extension` / `fast_extension` / `allow_fast_extension` 字段
- peer 消息处理无 Fast Set / Allowed Fast 逻辑,piece 发送仍走标准 choke/unchoke + request 协商

全仓搜索 `fast`、`allowed_fast`、`AllowedFast`、`bep6`、`supports_fast` 零命中。

**结论**:BEP-6 需 librqbit 上游实现,当前版本不可用。

### T4: max_peer_count / max_connections — 不可行

`SessionOptions` 和 `PeerConnectionOptions` 均无最大 peer 连接数字段。
单 torrent 最大 live peer 数硬编码 128(`src/torrent_state/live/mod.rs:278`,
`Semaphore::new(128)`),无公开 API 可改。

### PEX (Peer Exchange) — 自动启用,无开关

PEX 是内置自动行为(非 private torrent + 扩展握手声明支持 ut_pex 时自动启动),
唯一"关闭"方式是 torrent 标记为 private。无配置项可强制开关。

## 已实施的提速优化

P0/P1 阶段已完成的磁力链接提速项(无需 librqbit 上游改动):

| 项 | 描述 | 文件 |
|---|---|---|
| T1 | tracker 列表扩展(6→9,HTTPS 优先 + UDP 补充) | `config.rs:default_trackers` |
| T3 | peer 连接/读写超时调优(8s/10s,快于 librqbit 默认 10s) | `config.rs:default_peer_*` |
| T6 | peer_wait_timeout 300s→120s(死 swarm 更快失败回退) | `config.rs:default_peer_wait_timeout_secs` |
| SOCKS5 | BT tracker+peer 走 SOCKS5 代理(国内墙环境必需) | `bt_session.rs:build_session_options` |
| force_tracker_interval | 强制 120s tracker 回连(默认 30min-2h) | `config.rs:default_force_tracker_interval_secs` |
| defer_writes | 16MB 延迟写入缓冲(慢盘 I/O 聚合) | `config.rs:default_defer_writes_up_to_mb` |
| 死 swarm 韧性 | stall_timeout + peer_wait 双层超时 + 进度看门狗 | `magnet.rs:make_chunk_stream` |

## 后续方向

如需突破 librqbit 8.1.1 限制,可选路径(按工作量排序):

1. **升级 librqbit 9.x**(推荐):9.0.0-rc 已暴露 DHT bootstrap + `peer_limit`;
   等 stable 后改 `bt_session::build_session_options`(破坏性映射见
   `docs/sdd/librqbit-upgrade-and-pgo-eval.md`)。**不要**为 bootstrap 单独 fork 8.1.1。
2. **Fork librqbit**:仅当 9.x 长期不可用或上游收回 bootstrap API 时再考虑
   (rebase 成本高于升级)。
3. **替换 BT 引擎**:自研或改用其他 Rust BT crate,完全控制协议层(最高工作量,不推荐)。
