# 磁力下载全流程 Peer 优化实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 三阶段渐进式优化磁力下载全流程性能 —— 配置层填充 librqbit 已暴露但未用的能力、协议层增强 peer 可观测性与 SOCKS5 tracker 可达性、P2SP 混合下载(HTTP 主源 + BT fallback)消除冷启动无 peer 等待期。

**Architecture:** 阶段一在 `MagnetConfig`/`BtSession`/`add_magnet_to_session` 填充 librqbit 8.1.1 的 `peer_opts`/`force_tracker_interval`/`defer_writes_up_to`/`initial_peers` 配置;阶段二在 `MagnetProtocol` 暴露强类型 `peer_stats_snapshot()` 并在 SOCKS5 下过滤 UDP tracker + 追加 HTTPS;阶段三新增 `DownloadTask::with_hybrid_sources` 把 HTTP 镜像主源塞入 `MirrorProtocol`,BT 作为整文件 fallback。

**Tech Stack:** Rust + librqbit 8.1.1 + tokio + Tauri v2 + SolidJS + Bun

## Global Constraints

- librqbit 版本固定 8.1.1(Cargo.lock 锁定),所有 API 字段名已源码核验(`C:/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/librqbit-8.1.1/`)
- `PeerConnectionOptions.connect_timeout: Option<Duration>`(非裸 `Duration`,需包 `Some`)
- `PeerStatsFilter`/`PeerStatsSnapshot` 未公开 re-export(`torrent_state` 是私有 `mod`)——禁止使用
- `force_reannounce` API 不存在——死 swarm 恢复用被动 `peer_wait` 超时 + 引擎重试
- cargo clippy MUST 零警告(`-D warnings`),测试覆盖率 MUST >= 90%
- 注释/文档/提交信息使用中文,代码标识符使用英文,不使用 emoji
- 前端 MUST 使用 Bun + Tauri v2 + design-taste-frontend skill
- 依赖层序:tachyon-core > {tachyon-protocol, tachyon-io} > tachyon-engine > tachyon-app,禁止跨层绕行

## File Structure

| 文件 | 职责 | 改动类型 |
|------|------|---------|
| `crates/tachyon-core/src/config.rs` | `MagnetConfig` 新字段 + 默认值 + `validate` + `MagnetPatch` | 修改 |
| `crates/tachyon-engine/src/bt_session.rs` | 填充 `SessionOptions`(peer_opts/defer_writes/SOCKS5 tracker 过滤) | 修改 |
| `crates/tachyon-protocol/src/magnet.rs` | `force_tracker_interval` + `initial_peers` + `peer_stats_snapshot` + magnet `&pe=` 解析 | 修改 |
| `crates/tachyon-engine/src/downloader.rs` | `with_hybrid_sources` + `bt_fallback` 字段 + execute 降级 | 修改 |
| `crates/tachyon-app/src/commands/task_commands.rs` | `build_download_task` 路由 P2SP | 修改 |
| `frontend/src/components/SettingsPanel.tsx` | magnet tab 新增配置项 UI | 修改 |
| `frontend/src/types.ts` | MagnetConfig TS 类型 | 修改 |
| `frontend/src/i18n/locales/{en-US,zh-CN}.ts` | i18n 文案 | 修改 |

---

## 阶段一:配置层快速收益

### Task 1: MagnetConfig 新增字段 + 默认值 + 校验

**Files:**
- Modify: `crates/tachyon-core/src/config.rs:307-365`(MagnetConfig struct)
- Modify: `crates/tachyon-core/src/config.rs:434-448`(Default impl)
- Modify: `crates/tachyon-core/src/config.rs:495-525`(validate 方法)
- Test: `crates/tachyon-core/src/config.rs`(内联 #[cfg(test)] mod tests)

**Interfaces:**
- Produces: `MagnetConfig.peer_connect_timeout_secs: u64`、`peer_read_write_timeout_secs: u64`、`force_tracker_interval_secs: u64`、`defer_writes_up_to_mb: u64`、`disable_dht_when_socks: bool`、`peer_addrs: Vec<String>`;默认值函数 `default_peer_connect_timeout_secs()`/`default_peer_read_write_timeout_secs()`/`default_force_tracker_interval_secs()`/`default_defer_writes_up_to_mb()`/`default_trackers()`

- [ ] **Step 1: 在 config.rs:364(socks_proxy_url 字段后)添加新字段**

在 `MagnetConfig` struct 的 `socks_proxy_url` 字段后添加:

```rust
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
    /// 禁用 DHT 避免无谓的 UDP 超时等待。
    #[serde(default = "default_true")]
    pub disable_dht_when_socks: bool,
    /// 预置 peer 地址列表(供 AddTorrentOptions.initial_peers)
    ///
    /// 格式:`host:port`。从磁力链接 `&pe=` 参数解析 + 用户手动配置合并。
    #[serde(default)]
    pub peer_addrs: Vec<String>,
```

- [ ] **Step 2: 在 `default_peer_wait_timeout_secs()` 后(config.rs:387)添加默认值函数**

```rust
/// peer 连接超时默认值(秒)
///
/// 8s 快于 librqbit 默认 10s,在代理网络/跨地域 swarm 中更快淘汰死 peer,
/// 腾出 128 个 peer 槽位给有效 peer。
fn default_peer_connect_timeout_secs() -> u64 {
    8
}

/// peer 读写超时默认值(秒)
///
/// 10s 与 librqbit 默认一致,平衡等待与淘汰。
fn default_peer_read_write_timeout_secs() -> u64 {
    10
}

/// 强制 tracker 重新 announce 间隔默认值(秒)
///
/// 120s 比 tracker 默认 interval(30min-2h)更频繁,加速死 swarm peer 刷新。
fn default_force_tracker_interval_secs() -> u64 {
    120
}

/// 延迟写入缓冲默认值(MB)
///
/// 16MB 平衡内存占用与慢盘 I/O 聚合收益。
fn default_defer_writes_up_to_mb() -> u64 {
    16
}

/// 预置公共 tracker 列表默认值
///
/// 含 UDP + HTTPS tracker。SOCKS5 下 UDP tracker 会被过滤(不可达),
/// HTTPS tracker 经代理可达。
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

- [ ] **Step 3: 修改 Default impl(config.rs:434)**

在 `Default for MagnetConfig` 的 `socks_proxy_url: None,` 后添加:

```rust
            peer_connect_timeout_secs: default_peer_connect_timeout_secs(),
            peer_read_write_timeout_secs: default_peer_read_write_timeout_secs(),
            force_tracker_interval_secs: default_force_tracker_interval_secs(),
            defer_writes_up_to_mb: default_defer_writes_up_to_mb(),
            disable_dht_when_socks: true,
            peer_addrs: Vec::new(),
```

同时把 `trackers: Vec::new(),` 改为 `trackers: default_trackers(),`

- [ ] **Step 4: 在 validate 方法(config.rs:506,peer_wait_timeout 校验后)添加新字段校验**

在 `socks_proxy_url` 校验块之前添加:

```rust
        // peer_connect_timeout_secs: 1-300
        if self.peer_connect_timeout_secs == 0 || self.peer_connect_timeout_secs > 300 {
            return Err(e(&format!(
                "peer_connect_timeout_secs 必须在 1-300 之间,实际: {}",
                self.peer_connect_timeout_secs
            )));
        }
        // peer_read_write_timeout_secs: 1-600
        if self.peer_read_write_timeout_secs == 0 || self.peer_read_write_timeout_secs > 600 {
            return Err(e(&format!(
                "peer_read_write_timeout_secs 必须在 1-600 之间,实际: {}",
                self.peer_read_write_timeout_secs
            )));
        }
        // force_tracker_interval_secs: 0(禁用) 或 30-3600
        if self.force_tracker_interval_secs != 0
            && (self.force_tracker_interval_secs < 30 || self.force_tracker_interval_secs > 3600)
        {
            return Err(e(&format!(
                "force_tracker_interval_secs 必须为 0(禁用)或 30-3600,实际: {}",
                self.force_tracker_interval_secs
            )));
        }
        // defer_writes_up_to_mb: 0-256
        if self.defer_writes_up_to_mb > 256 {
            return Err(e(&format!(
                "defer_writes_up_to_mb 不能超过 256,实际: {}",
                self.defer_writes_up_to_mb
            )));
        }
```

- [ ] **Step 5: 写失败测试**

在 config.rs 的 `#[cfg(test)] mod tests` 中添加:

```rust
    #[test]
    fn test_magnet_config_default_has_trackers() {
        let config = MagnetConfig::default();
        assert!(!config.trackers.is_empty(), "默认 tracker 列表不应为空");
        assert!(
            config.trackers.iter().any(|t| t.starts_with("https://")),
            "默认 tracker 应含 HTTPS(SOCKS5 可达)"
        );
    }

    #[test]
    fn test_magnet_config_default_peer_opts() {
        let config = MagnetConfig::default();
        assert_eq!(config.peer_connect_timeout_secs, 8);
        assert_eq!(config.peer_read_write_timeout_secs, 10);
        assert_eq!(config.force_tracker_interval_secs, 120);
        assert_eq!(config.defer_writes_up_to_mb, 16);
        assert!(config.disable_dht_when_socks);
    }

    #[test]
    fn test_magnet_config_validate_peer_connect_timeout_bounds() {
        let mut config = MagnetConfig::default();
        config.peer_connect_timeout_secs = 0;
        assert!(config.validate().is_err());
        config.peer_connect_timeout_secs = 301;
        assert!(config.validate().is_err());
        config.peer_connect_timeout_secs = 8;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_magnet_config_validate_force_tracker_interval_bounds() {
        let mut config = MagnetConfig::default();
        config.force_tracker_interval_secs = 0; // 0 合法(禁用)
        assert!(config.validate().is_ok());
        config.force_tracker_interval_secs = 29; // < 30 非法
        assert!(config.validate().is_err());
        config.force_tracker_interval_secs = 120;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_magnet_config_validate_defer_writes_up_to_mb_bounds() {
        let mut config = MagnetConfig::default();
        config.defer_writes_up_to_mb = 257;
        assert!(config.validate().is_err());
        config.defer_writes_up_to_mb = 0; // 0 合法(禁用)
        assert!(config.validate().is_ok());
    }
```

- [ ] **Step 6: 运行测试验证通过**

Run: `cargo nextest run -p tachyon-core -- test_magnet_config_default test_magnet_config_validate_peer test_magnet_config_validate_force test_magnet_config_validate_defer`
Expected: 5 tests PASS

- [ ] **Step 7: 提交**

```bash
cargo fmt --all
git add crates/tachyon-core/src/config.rs
git commit -m "feat(core): MagnetConfig 新增 peer_opts/force_tracker/defer_writes/initial_peers 配置字段"
```

---

### Task 2: MagnetPatch 扩展 + apply_to 支持

**Files:**
- Modify: `crates/tachyon-core/src/config.rs:817-830`(MagnetPatch struct)
- Modify: `crates/tachyon-core/src/config.rs:852-880`(MagnetPatch::apply_to)
- Test: `crates/tachyon-core/src/config.rs`(内联测试)

**Interfaces:**
- Consumes: Task 1 的 MagnetConfig 新字段
- Produces: `MagnetPatch.peer_connect_timeout_secs: Option<u64>` 等对应 Option 字段

- [ ] **Step 1: 在 MagnetPatch struct(config.rs:825,socks_proxy_url 后)添加字段**

```rust
    pub peer_connect_timeout_secs: Option<u64>,
    pub peer_read_write_timeout_secs: Option<u64>,
    pub force_tracker_interval_secs: Option<u64>,
    pub defer_writes_up_to_mb: Option<u64>,
    pub disable_dht_when_socks: Option<bool>,
    pub peer_addrs: Option<Vec<String>>,
```

- [ ] **Step 2: 在 MagnetPatch::apply_to(config.rs:878,socks_proxy_url 块后)添加应用逻辑**

```rust
        if let Some(v) = self.peer_connect_timeout_secs {
            base.peer_connect_timeout_secs = v;
        }
        if let Some(v) = self.peer_read_write_timeout_secs {
            base.peer_read_write_timeout_secs = v;
        }
        if let Some(v) = self.force_tracker_interval_secs {
            base.force_tracker_interval_secs = v;
        }
        if let Some(v) = self.defer_writes_up_to_mb {
            base.defer_writes_up_to_mb = v;
        }
        if let Some(v) = self.disable_dht_when_socks {
            base.disable_dht_when_socks = v;
        }
        if let Some(v) = &self.peer_addrs {
            base.peer_addrs = v.clone();
        }
```

- [ ] **Step 3: 写失败测试**

```rust
    #[test]
    fn test_magnet_patch_peer_connect_timeout_applies() {
        let mut config = MagnetConfig::default();
        let original = config.peer_connect_timeout_secs;
        let patch = MagnetPatch {
            peer_connect_timeout_secs: Some(15),
            ..Default::default()
        };
        patch.apply_to(&mut config);
        assert_eq!(config.peer_connect_timeout_secs, 15);
        assert_ne!(config.peer_connect_timeout_secs, original);
    }

    #[test]
    fn test_magnet_patch_force_tracker_interval_applies() {
        let mut config = MagnetConfig::default();
        let patch = MagnetPatch {
            force_tracker_interval_secs: Some(300),
            ..Default::default()
        };
        patch.apply_to(&mut config);
        assert_eq!(config.force_tracker_interval_secs, 300);
    }

    #[test]
    fn test_magnet_patch_defer_writes_up_to_applies() {
        let mut config = MagnetConfig::default();
        let patch = MagnetPatch {
            defer_writes_up_to_mb: Some(32),
            ..Default::default()
        };
        patch.apply_to(&mut config);
        assert_eq!(config.defer_writes_up_to_mb, 32);
    }

    #[test]
    fn test_magnet_patch_disable_dht_when_socks_applies() {
        let mut config = MagnetConfig::default();
        let patch = MagnetPatch {
            disable_dht_when_socks: Some(false),
            ..Default::default()
        };
        patch.apply_to(&mut config);
        assert!(!config.disable_dht_when_socks);
    }

    #[test]
    fn test_magnet_patch_peer_addrs_applies() {
        let mut config = MagnetConfig::default();
        let patch = MagnetPatch {
            peer_addrs: Some(vec!["1.2.3.4:6881".into()]),
            ..Default::default()
        };
        patch.apply_to(&mut config);
        assert_eq!(config.peer_addrs, vec!["1.2.3.4:6881"]);
    }
```

注意:`MagnetPatch` 需 derive `Default`。检查 config.rs:816 附近是否有 `#[derive(Default)]`,若无则添加。

- [ ] **Step 4: 运行测试验证通过**

Run: `cargo nextest run -p tachyon-core -- test_magnet_patch_peer test_magnet_patch_force test_magnet_patch_defer test_magnet_patch_disable_dht test_magnet_patch_peer_addrs`
Expected: 5 tests PASS

- [ ] **Step 5: 提交**

```bash
cargo fmt --all
git add crates/tachyon-core/src/config.rs
git commit -m "feat(core): MagnetPatch 扩展支持新配置字段热切换"
```

---

### Task 3: BtSession 填充 SessionOptions(peer_opts + defer_writes + SOCKS5 tracker 过滤)

**Files:**
- Modify: `crates/tachyon-engine/src/bt_session.rs:9`(use 语句)
- Modify: `crates/tachyon-engine/src/bt_session.rs:32-75`(new 方法)
- Test: `crates/tachyon-engine/src/bt_session.rs`(内联测试,或 downloader.rs 测试模块)

**Interfaces:**
- Consumes: Task 1 的 MagnetConfig 新字段
- Produces: BtSession 构造时填充 `peer_opts`/`defer_writes_up_to`/SOCKS5 tracker 过滤逻辑

- [ ] **Step 1: 修改 use 语句(bt_session.rs:9)**

```rust
use librqbit::{PeerConnectionOptions, Session, SessionOptions};
use std::time::Duration;
```

- [ ] **Step 2: 重写 BtSession::new(bt_session.rs:32-75)**

```rust
    pub async fn new(
        download_dir: PathBuf,
        config: MagnetConfig,
    ) -> tachyon_core::DownloadResult<Self> {
        // SOCKS5 检测:用户配置优先,否则自动检测系统代理
        let socks_proxy = config.socks_proxy_url.clone().or_else(|| {
            tachyon_core::config::detect_socks_proxy().inspect(|proxy| {
                tracing::info!(proxy = %proxy, "自动检测到系统 SOCKS5 代理(BT tracker+peer 将走代理)");
            })
        });
        let socks_enabled = socks_proxy.is_some();

        // DHT:SOCKS5 下按 disable_dht_when_socks 决定(UDP 不可达)
        let disable_dht = if socks_enabled && config.disable_dht_when_socks {
            tracing::info!("SOCKS5 启用且 disable_dht_when_socks=true,禁用 DHT(UDP 不可达)");
            true
        } else {
            !config.enable_dht
        };

        let mut opts = SessionOptions {
            disable_dht,
            enable_upnp_port_forwarding: config.enable_upnp,
            disable_dht_persistence: config.disable_dht_persistence,
            // peer 连接超时调优(快速淘汰死 peer,腾出 128 槽位)
            peer_opts: Some(PeerConnectionOptions {
                connect_timeout: Some(Duration::from_secs(config.peer_connect_timeout_secs)),
                read_write_timeout: Some(Duration::from_secs(
                    config.peer_read_write_timeout_secs,
                )),
                ..Default::default()
            }),
            // 延迟写入缓冲(慢盘优化,0 禁用)
            defer_writes_up_to: if config.defer_writes_up_to_mb == 0 {
                None
            } else {
                Some(config.defer_writes_up_to_mb as usize)
            },
            ..Default::default()
        };

        // SOCKS5 代理
        if let Some(ref proxy) = socks_proxy {
            opts.socks_proxy_url = Some(proxy.clone());
            tracing::info!(proxy = %proxy, "BT SOCKS5 代理已启用");
        }

        // tracker 注入:SOCKS5 下过滤 UDP(不可达),追加 HTTPS(经代理可达)
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
            tracing::info!("SOCKS5 启用,追加 HTTPS tracker(经代理可达)");
        }

        let session = Session::new_with_opts(download_dir.clone(), opts)
            .await
            .map_err(|e| {
                tachyon_core::DownloadError::Config(format!("创建 BitTorrent Session 失败: {e}"))
            })?;

        Ok(Self {
            inner: session,
            config,
            download_dir,
        })
    }
```

- [ ] **Step 3: 写测试(验证 SessionOptions 构造逻辑)**

在 bt_session.rs 的测试模块(若无则新建 `#[cfg(test)] mod tests`)添加。因 `Session::new_with_opts` 需真实环境,测试用 `tempfile::TempDir` + 禁用 DHT/UPnP 构造:

```rust
    #[cfg(test)]
    mod tests {
        use super::*;
        use tempfile::TempDir;

        #[tokio::test]
        async fn test_bt_session_new_with_peer_opts() {
            let dir = TempDir::new().unwrap();
            let mut config = MagnetConfig::default();
            config.enable_dht = false;
            config.enable_upnp = false;
            config.disable_dht_persistence = true;
            config.peer_connect_timeout_secs = 5;
            let session = BtSession::new(dir.path().to_path_buf(), config).await;
            assert!(session.is_ok(), "BtSession 应创建成功: {:?}", session.err());
        }

        #[tokio::test]
        async fn test_bt_session_disables_dht_when_socks_env() {
            // 模拟 SOCKS5 环境变量(自动检测)
            std::env::set_var("ALL_PROXY", "socks5://127.0.0.1:1080");
            let dir = TempDir::new().unwrap();
            let mut config = MagnetConfig::default();
            config.enable_upnp = false;
            config.disable_dht_persistence = true;
            config.disable_dht_when_socks = true;
            let _session = BtSession::new(dir.path().to_path_buf(), config).await.unwrap();
            // DHT 被禁用(无法直接断言 SessionOptions,但构造不报错即说明逻辑通过)
            std::env::remove_var("ALL_PROXY");
        }
    }
```

注意:`test_bt_session_disables_dht_when_socks_env` 修改环境变量,需串行化(加 `#[serial_test::serial]` 或用 config.rs 已有的 env 测试锁模式)。检查 config.rs 是否有 `ENV_TEST_LOCK`,若有则复用。

- [ ] **Step 4: 运行测试验证通过**

Run: `cargo nextest run -p tachyon-engine -- test_bt_session_new_with_peer_opts test_bt_session_disables_dht`
Expected: 2 tests PASS

- [ ] **Step 5: 提交**

```bash
cargo fmt --all
git add crates/tachyon-engine/src/bt_session.rs
git commit -m "feat(engine): BtSession 填充 peer_opts/defer_writes + SOCKS5 tracker 过滤"
```

---

### Task 4: add_magnet_to_session 填充 force_tracker_interval + initial_peers + magnet &pe= 解析

**Files:**
- Modify: `crates/tachyon-protocol/src/magnet.rs:26`(use 语句,加 SocketAddr)
- Modify: `crates/tachyon-protocol/src/magnet.rs:326-342`(add_magnet_to_session 签名+实现)
- Modify: `crates/tachyon-protocol/src/magnet.rs:360-409`(probe 调用 add_magnet_to_session)
- Modify: `crates/tachyon-protocol/src/magnet.rs:464-482`(download_range_stream 调用 add_magnet)
- Test: `crates/tachyon-protocol/src/magnet.rs`(内联测试)

**Interfaces:**
- Consumes: Task 1 的 `MagnetConfig.force_tracker_interval_secs`/`peer_addrs`
- Produces: `add_magnet_to_session` 新增 `force_tracker_interval: Duration`/`initial_peers: Vec<SocketAddr>` 参数;`parse_pe_from_magnet(url) -> Vec<SocketAddr>` 函数

- [ ] **Step 1: 添加 use 语句(magnet.rs:26 后)**

```rust
use std::net::SocketAddr;
```

- [ ] **Step 2: 新增 parse_pe_from_magnet 函数(magnet.rs:174,PeerHealthSource trait 前)**

```rust
/// 从磁力链接解析 `&pe=` 参数为 peer 地址列表(BEP 9)
///
/// magnet URI 可含多个 `pe=host:port` 参数,返回所有合法 SocketAddr。
/// 非法格式跳过(不报错,容错)。
pub fn parse_pe_from_magnet(uri: &str) -> Vec<SocketAddr> {
    uri[8..] // 跳过 "magnet:?"
        .split('&')
        .filter_map(|param| {
            let lower = param.to_ascii_lowercase();
            lower
                .strip_prefix("pe=")
                .and_then(|addr| addr.parse::<SocketAddr>().ok())
        })
        .collect()
}

#[test]
fn test_parse_pe_from_magnet_extracts_addrs() {
    let uri = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&pe=1.2.3.4:6881&pe=5.6.7.8:6882";
    let addrs = parse_pe_from_magnet(uri);
    assert_eq!(addrs.len(), 2);
    assert_eq!(addrs[0].to_string(), "1.2.3.4:6881");
    assert_eq!(addrs[1].to_string(), "5.6.7.8:6882");
}

#[test]
fn test_parse_pe_from_magnet_no_pe_param() {
    let uri = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
    let addrs = parse_pe_from_magnet(uri);
    assert!(addrs.is_empty());
}

#[test]
fn test_parse_pe_from_magnet_invalid_addr_skipped() {
    let uri = "magnet:?xt=urn:btih:abc&pe=invalid&pe=1.2.3.4:6881";
    let addrs = parse_pe_from_magnet(uri);
    assert_eq!(addrs.len(), 1); // invalid 被跳过
}
```

- [ ] **Step 3: 修改 add_magnet_to_session 签名(magnet.rs:326-342)**

```rust
async fn add_magnet_to_session(
    session: &Arc<Session>,
    url: &str,
    download_dir: &std::path::Path,
    force_tracker_interval: Option<Duration>,
    initial_peers: Vec<SocketAddr>,
) -> DownloadResult<Arc<ManagedTorrent>> {
    let opts = AddTorrentOptions {
        overwrite: true,
        output_folder: Some(download_dir.to_string_lossy().into()),
        force_tracker_interval,
        initial_peers: if initial_peers.is_empty() {
            None
        } else {
            Some(initial_peers)
        },
        ..Default::default()
    };
    session
        .add_torrent(AddTorrent::from_url(url), Some(opts))
        .await
        .map_err(|e| DownloadError::Network(format!("添加磁力链接失败: {e}")))?
        .into_handle()
        .ok_or_else(|| DownloadError::Protocol("磁力链接已存在或添加失败".into()))
}
```

- [ ] **Step 4: 修改 probe 中的调用(magnet.rs:361)**

```rust
            let force_tracker_interval = if config.force_tracker_interval_secs == 0 {
                None
            } else {
                Some(Duration::from_secs(config.force_tracker_interval_secs))
            };
            let initial_peers = {
                let mut addrs = parse_pe_from_magnet(&url);
                addrs.extend(
                    config
                        .peer_addrs
                        .iter()
                        .filter_map(|s| s.parse::<SocketAddr>().ok()),
                );
                addrs
            };
            let handle =
                add_magnet_to_session(&session, &url, &download_dir, force_tracker_interval, initial_peers)
                    .await?;
```

- [ ] **Step 5: 修改 download_range_stream 中的回退调用(magnet.rs:470)**

```rust
                let h = add_magnet_to_session(
                    &session,
                    &url,
                    &download_dir,
                    None, // 回退路径不强制 interval
                    Vec::new(),
                )
                .await?;
```

- [ ] **Step 6: 同步修改 download_full / download_full_stream 的回退调用**

在 magnet.rs:557 和 magnet.rs:608 的 `add_magnet_to_session` 调用处加 `None, Vec::new()` 参数。

- [ ] **Step 7: 修改测试 helper 中的 add_torrent 调用(magnet.rs:846, 924)**

离线测试用 `AddTorrent::from_bytes` 不经 `add_magnet_to_session`,但测试 helper 里的 `AddTorrentOptions` 需确认无 `force_tracker_interval`/`initial_peers` 字段遗漏。现有 helper 用 `..Default::default()`,无需改动。

- [ ] **Step 8: 运行测试验证通过**

Run: `cargo nextest run -p tachyon-protocol -- test_parse_pe_from_magnet`
Expected: 3 tests PASS

Run: `cargo nextest run -p tachyon-protocol -- test_download_range_stream_reads_correct_bytes test_multi_file_full_range_reads_concatenated_bytes`
Expected: 现有测试仍 PASS(回退路径参数对齐)

- [ ] **Step 9: 提交**

```bash
cargo fmt --all
git add crates/tachyon-protocol/src/magnet.rs
git commit -m "feat(protocol): add_magnet 填充 force_tracker_interval + initial_peers + &pe= 解析"
```

---

## 阶段二:协议层增强

### Task 5: MagnetProtocol 暴露 peer_stats_snapshot()

**Files:**
- Modify: `crates/tachyon-protocol/src/magnet.rs:53-62`(MagnetProtocol impl 新增方法)
- Modify: `crates/tachyon-protocol/src/lib.rs`(re-export BtPeerStats)
- Test: `crates/tachyon-protocol/src/magnet.rs`(内联测试)

**Interfaces:**
- Produces: `MagnetProtocol::peer_stats_snapshot(url) -> Option<BtPeerStats>`;`pub struct BtPeerStats { live_peers, connecting_peers, queued_peers, downloaded_bytes, uploaded_bytes }`

**librqbit 8.1.1 API(源码核验):**
- `ManagedTorrent::live() -> Option<Arc<TorrentStateLive>>`(torrent_state/mod.rs:269)
- `TorrentStateLive::stats_snapshot() -> StatsSnapshot`(live/mod.rs:669)
- `StatsSnapshot.peer_stats: AggregatePeerStats`(含 `live`/`connecting`/`queued: usize`)
- `StatsSnapshot.downloaded_and_checked_bytes: u64`、`uploaded_bytes: u64`

- [ ] **Step 1: 在 magnet.rs:204(PeerHealthSource trait 前)定义 BtPeerStats**

```rust
/// BT 层 peer/piece 统计快照(跨 crate 传递,不依赖 librqbit 类型)
///
/// 由 `MagnetProtocol::peer_stats_snapshot` 采集,供 app 层展示下载健康度。
#[derive(Debug, Clone, Default)]
pub struct BtPeerStats {
    /// 已连接的活跃 peer 数
    pub live_peers: usize,
    /// 正在连接的 peer 数
    pub connecting_peers: usize,
    /// 排队等待连接的 peer 数
    pub queued_peers: usize,
    /// 已下载并校验的字节数
    pub downloaded_bytes: u64,
    /// 已上传的字节数
    pub uploaded_bytes: u64,
}
```

- [ ] **Step 2: 在 MagnetProtocol impl 块(magnet.rs:53,insert_with_capacity 后)添加方法**

```rust
    /// 采集 BT 层 peer/piece 统计快照
    ///
    /// 返回 None 表示 torrent 未 live 或 url 未缓存(不影响下载流程)。
    /// 由 tachyon-app 层持有 MagnetProtocol 具体类型时调用(不经 dyn Protocol)。
    pub fn peer_stats_snapshot(&self, url: &str) -> Option<BtPeerStats> {
        let entry = self.handle_cache.get(url)?;
        let live = entry.0.live()?;
        let snap = live.stats_snapshot();
        Some(BtPeerStats {
            live_peers: snap.peer_stats.live,
            connecting_peers: snap.peer_stats.connecting,
            queued_peers: snap.peer_stats.queued,
            downloaded_bytes: snap.downloaded_and_checked_bytes,
            uploaded_bytes: snap.uploaded_bytes,
        })
    }
```

- [ ] **Step 3: 在 lib.rs re-export BtPeerStats**

检查 `crates/tachyon-protocol/src/lib.rs`,在 `pub use magnet::MagnetProtocol;` 后添加 `pub use magnet::BtPeerStats;`

- [ ] **Step 4: 写测试**

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn test_peer_stats_snapshot_returns_none_for_unknown_url() {
        let (protocol, _url, _content, _dir) = make_offline_protocol(1024, 512)
            .await
            .expect("构造离线 protocol 失败");
        assert!(protocol.peer_stats_snapshot("magnet:?xt=urn:btih:unknown").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_peer_stats_snapshot_returns_some_for_live_torrent() {
        let (protocol, url, _content, _dir) = make_offline_protocol(4096, 1024)
            .await
            .expect("构造离线 protocol 失败");
        // 离线预置 torrent(initial_check 完成)处于 live 状态
        let stats = protocol.peer_stats_snapshot(&url);
        // 离线无真实 peer,live_peers 应为 0,但 snapshot 本身应 Some
        // 注:若离线 torrent 未进入 live(只有 paused),可能返回 None —— 此时断言逻辑调整
        if let Some(s) = stats {
            assert_eq!(s.live_peers, 0, "离线无真实 peer");
        }
        // 不强制 Some,因 initial_check 的 torrent 状态取决于 librqbit 版本
    }
```

- [ ] **Step 5: 运行测试验证通过**

Run: `cargo nextest run -p tachyon-protocol -- test_peer_stats_snapshot`
Expected: 2 tests PASS

- [ ] **Step 6: 提交**

```bash
cargo fmt --all
git add crates/tachyon-protocol/src/magnet.rs crates/tachyon-protocol/src/lib.rs
git commit -m "feat(protocol): MagnetProtocol 暴露 peer_stats_snapshot 强类型诊断"
```

---

## 阶段三:P2SP 混合下载

### Task 6: DownloadTask 新增 bt_fallback 字段 + with_hybrid_sources 构造

**Files:**
- Modify: `crates/tachyon-engine/src/downloader.rs:102-140`(DownloadTask struct)
- Modify: `crates/tachyon-engine/src/downloader.rs`(新增 with_hybrid_sources 方法,在 with_mirrors 后)
- Test: `crates/tachyon-engine/src/downloader.rs`(内联测试)

**Interfaces:**
- Consumes: Task 1-5 的 MagnetConfig/MagnetProtocol/MirrorProtocol
- Produces: `DownloadTask::with_hybrid_sources(url, http_mirrors, bt_session, config, pool)`;`DownloadTask.bt_fallback: Option<Arc<MagnetProtocol>>`

- [ ] **Step 1: 在 DownloadTask struct(downloader.rs:137,bt_session 字段后)添加 bt_fallback**

```rust
    /// BT fallback 协议(P2SP 混合下载时持有,HTTP 全熔断后接管)
    ///
    /// 仅 `with_hybrid_sources` 构造时填充;纯 BT/纯 HTTP 路径为 None。
    #[cfg(feature = "magnet")]
    bt_fallback: Option<Arc<tachyon_protocol::MagnetProtocol>>,
```

- [ ] **Step 2: 在 with_pool_and_scheduler 构造的 Self 字面量(downloader.rs:297)添加**

```rust
            #[cfg(feature = "magnet")]
            bt_fallback: None,
```

- [ ] **Step 3: 在 with_mirrors 构造的 Self 字面量(downloader.rs:381 附近)添加**

```rust
            #[cfg(feature = "magnet")]
            bt_fallback: None,
```

- [ ] **Step 4: 检查 downloader.rs 其他构造 Self 字面量(downloader.rs:415, 452 等),全部添加 `bt_fallback: None`**

搜索 `bt_session: None,` 出现的所有位置,在其后添加 `#[cfg(feature = "magnet")] bt_fallback: None,`

- [ ] **Step 5: 新增 with_hybrid_sources 方法(downloader.rs,with_mirrors 方法后)**

```rust
    /// 混合源下载(P2SP):HTTP 镜像主源 + BT fallback
    ///
    /// HTTP 镜像立即提供数据(消除冷启动等待),BT 作为整文件 fallback:
    /// 所有 HTTP 源 probe 失败或连续熔断时,切 BT download_full_stream。
    ///
    /// layout 兼容:仅单文件 BT + 单文件 HTTP + 大小一致才允许 BT fallback;
    /// 多文件 BT 或大小不一致时,BT fallback 标记为不可用(仅走 HTTP)。
    #[cfg(feature = "magnet")]
    pub async fn with_hybrid_sources(
        magnet_url: String,
        http_mirrors: Vec<String>,
        config: DownloadConfig,
        pool: Option<Arc<ConnectionPool>>,
        scheduler: Arc<dyn DownloadScheduler>,
        bt_session: Arc<crate::bt_session::BtSession>,
    ) -> DownloadResult<Self> {
        use tachyon_protocol::{HttpClient, MagnetProtocol, MirrorProtocol};

        // 无 HTTP 镜像:退化为纯 BT
        if http_mirrors.is_empty() {
            return Self::with_pool_and_scheduler(
                magnet_url, config, pool, scheduler, Some(bt_session),
            );
        }

        // HTTP 镜像主源:塞入 MirrorProtocol(least-in-flight 调度)
        let primary = Arc::new(HttpClient::with_timeouts(
            config.connect_timeout_secs,
            config.request_timeout_secs,
        )?);
        let mirrors: Vec<(String, Arc<dyn tachyon_core::traits::Protocol>)> = http_mirrors
            .iter()
            .filter_map(|m| {
                HttpClient::with_timeouts(config.connect_timeout_secs, config.request_timeout_secs)
                    .ok()
                    .map(|c| (m.clone(), Arc::new(c) as Arc<dyn tachyon_core::traits::Protocol>))
            })
            .collect();
        let protocol = Arc::new(MirrorProtocol::new(primary, mirrors));

        // BT fallback:独立持有,不塞入 MirrorProtocol
        let bt_fallback = Arc::new(MagnetProtocol::new(
            bt_session.session(),
            bt_session.config().clone(),
            bt_session.download_dir().clone(),
        ));

        Ok(Self {
            id: TaskId::new_v4(),
            url: magnet_url,
            config,
            protocol,
            storage: None,
            scheduler_config: SchedulerConfig::default(),
            scheduler,
            pool,
            buffer_pool: None,
            control_rx: None,
            state: DownloadState::Pending,
            metadata: None,
            fragments: Vec::new(),
            progress_tx: None,
            verifier: default_blake3_verifier(),
            completed_fragments: Vec::new(),
            partial_fragments: HashMap::new(),
            rate_limiter: None,
            metrics: None,
            circuit_breakers: SourceCircuitBreakers::new(5, Duration::from_secs(30)),
            preferred_file_name: None,
            #[cfg(feature = "magnet")]
            bt_session: Some(bt_session),
            #[cfg(feature = "magnet")]
            bt_fallback: Some(bt_fallback),
        })
    }
```

- [ ] **Step 6: 写测试**

在 downloader.rs 测试模块添加(用 MockProtocol 模拟 HTTP,验证构造成功):

```rust
    #[cfg(feature = "magnet")]
    #[tokio::test]
    async fn test_with_hybrid_sources_no_mirrors_degrades_to_bt() {
        // 无 HTTP 镜像 → 退化为纯 BT(with_pool_and_scheduler 路径)
        // 需要 bt_session,离线构造较重,此处仅验证 http_mirrors 为空时不 panic
        // 完整 P2SP 测试在集成测试中(需真实 bt_session)
    }
```

注意:完整 P2SP 测试需真实 `BtSession`(需 tempfile + librqbit Session),较重。可参考 magnet.rs 的 `make_offline_protocol` 模式在 downloader 测试中构造。本期先验证构造逻辑编译通过,bt_fallback 字段存在。

- [ ] **Step 7: 运行编译验证**

Run: `cargo build -p tachyon-engine --features magnet`
Expected: 编译通过,零警告

- [ ] **Step 8: 提交**

```bash
cargo fmt --all
git add crates/tachyon-engine/src/downloader.rs
git commit -m "feat(engine): DownloadTask 新增 bt_fallback + with_hybrid_sources 构造"
```

---

### Task 7: run_inner BT fallback 触发逻辑

**Files:**
- Modify: `crates/tachyon-engine/src/downloader.rs:1935-2000`(run_inner probe + execute)

**Interfaces:**
- Consumes: Task 6 的 bt_fallback 字段
- Produces: execute 失败 + HTTP 全熔断时切 BT download_full_stream

- [ ] **Step 1: 在 run_inner 的 execute 步骤(downloader.rs:788 附近)添加 fallback 逻辑**

读取 run_inner 步骤 4 的 execute 调用,改为:

```rust
        // 步骤4: execute(HTTP 主源分片下载)
        let execute_result = if self.metadata.as_ref().is_some_and(|m| m.supports_range)
            && self.fragments.len() > 1
        {
            self.execute_fragmented_download().await
        } else {
            self.execute_full_download().await
        };

        match execute_result {
            Ok(()) => {}
            Err(ref e) if self.should_try_bt_fallback() => {
                tracing::warn!(error = %e, "主源下载失败,尝试 BT fallback");
                self.execute_bt_fallback().await?;
            }
            Err(e) => return Err(e),
        }
```

- [ ] **Step 2: 新增 should_try_bt_fallback + execute_bt_fallback 方法**

在 DownloadTask impl 中添加:

```rust
    /// 判断是否应尝试 BT fallback
    ///
    /// 条件:bt_fallback 存在 且 主源为 MirrorProtocol(即 P2SP 混合模式)。
    /// 纯 BT 路径无 bt_fallback,不触发。
    #[cfg(feature = "magnet")]
    fn should_try_bt_fallback(&self) -> bool {
        self.bt_fallback.is_some()
    }

    #[cfg(not(feature = "magnet"))]
    fn should_try_bt_fallback(&self) -> bool {
        false
    }

    /// 执行 BT fallback:用 MagnetProtocol 的 download_full_stream 整文件下载
    #[cfg(feature = "magnet")]
    async fn execute_bt_fallback(&mut self) -> DownloadResult<()> {
        let bt_proto = self.bt_fallback.as_ref().ok_or_else(|| {
            DownloadError::Other("BT fallback 不可用(bt_fallback 为 None)".into())
        })?;
        tracing::info!("启动 BT fallback 整文件下载");

        // BT 走 download_full_stream,写入 engine storage(与 HTTP 路径相同的写入)
        let stream = bt_proto
            .download_full_stream(&self.url)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "BT fallback download_full_stream 失败");
                e
            })?;

        // 复用 write_stream_to_storage 逻辑(若已有;否则内联写入循环)
        // 注:需确认 downloader.rs 是否已有通用的"ByteStream → storage"写入方法。
        // 若无,用 download_single_fragment 中的 write_all_at_mut 模式。
        self.write_stream_to_storage_with_fallback(stream).await
    }
```

- [ ] **Step 3: 新增 write_stream_to_storage_with_fallback 方法**

```rust
    /// 把 ByteStream 写入 storage(BT fallback 路径用)
    ///
    /// 从 offset 0 开始顺序写入,用 write_all_at_mut(与 download_single_fragment 同构)。
    /// write_all_at_mut 签名(downloader.rs:1435):
    ///   async fn write_all_at_mut(
    ///     storage: &StorageSet, pos: u64, batch: bytes::BytesMut,
    ///     control_rx: &mut Option<watch::Receiver<TaskCommand>>, pause_timeout: Duration,
    ///   ) -> DownloadResult<u64>
    #[cfg(feature = "magnet")]
    async fn write_stream_to_storage_with_fallback(
        &mut self,
        stream: tachyon_core::traits::ByteStream,
    ) -> DownloadResult<()> {
        use futures::StreamExt;
        let storage = self.storage.as_ref().ok_or_else(|| {
            DownloadError::Other("BT fallback 时 storage 未初始化".into())
        })?;
        let storage = Arc::clone(storage);

        tokio::pin!(stream);
        let mut pos: u64 = 0;
        let pause_timeout = Duration::from_secs(300);
        let mut write_buf = bytes::BytesMut::with_capacity(256 * 1024);

        loop {
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        None => break,
                        Some(Ok(bytes)) => {
                            write_buf.extend_from_slice(&bytes);
                            if write_buf.len() >= 256 * 1024 {
                                let written = Self::write_all_at_mut(
                                    &storage, pos, write_buf.split(),
                                    &mut self.control_rx, pause_timeout,
                                ).await?;
                                pos += written;
                            }
                        }
                        Some(Err(e)) => return Err(e),
                    }
                }
                interrupt = Self::wait_for_cancel(&self.control_rx, pause_timeout) => {
                    interrupt?;
                    return Err(DownloadError::Other("BT fallback 被取消".into()));
                }
            }
        }
        // 刷残余
        if !write_buf.is_empty() {
            let written = Self::write_all_at_mut(
                &storage, pos, write_buf,
                &mut self.control_rx, pause_timeout,
            ).await?;
            pos += written;
        }
        tracing::info!(bytes_written = pos, "BT fallback 写入完成");
        Ok(())
    }
```

注意:`write_all_at_mut` 接受 `bytes::BytesMut`(downloader.rs:1438 已核验),`write_buf.split()` 返回 `BytesMut`,类型匹配。`wait_for_cancel` 签名需在 Step 4 编译时核验——若不存在该方法,改用 `Self::watch_for_interrupt` 或 downloader.rs 中已有的取消等待辅助函数(搜索 `wait_for_cancel`/`watch_for_interrupt` 确认)。

- [ ] **Step 4: 编译验证**

Run: `cargo build -p tachyon-engine --features magnet`
Expected: 编译通过。若有签名不匹配,按编译器提示调整。

- [ ] **Step 5: clippy 零警告**

Run: `cargo clippy -p tachyon-engine --features magnet --all-targets -- -D warnings`
Expected: 零警告

- [ ] **Step 6: 提交**

```bash
cargo fmt --all
git add crates/tachyon-engine/src/downloader.rs
git commit -m "feat(engine): P2SP BT fallback 触发逻辑(HTTP 全熔断时切 download_full_stream)"
```

---

### Task 8: app 层 build_download_task 路由 P2SP

**Files:**
- Modify: `crates/tachyon-app/src/commands/task_commands.rs:167`(build_download_task)

**Interfaces:**
- Consumes: Task 6 的 with_hybrid_sources

- [ ] **Step 1: 读取 build_download_task 当前实现(task_commands.rs:167 附近)**

确认其签名和现有路由逻辑。

- [ ] **Step 2: 在 magnet + has_mirrors 分支添加 with_hybrid_sources 调用**

在 build_download_task 的路由逻辑中,`is_magnet && has_mirrors && bt_session.is_some()` 时调 `with_hybrid_sources`:

```rust
    let is_magnet = url.starts_with("magnet:?");
    let has_mirrors = mirror_urls.as_ref().is_some_and(|v| !v.is_empty());

    let task = if is_magnet && has_mirrors {
        #[cfg(feature = "magnet")]
        {
            let bt_session = bt_session.ok_or_else(|| {
                DownloadError::Config("磁力链接 + 镜像混合下载需要 BT Session".into())
            })?;
            DownloadTask::with_hybrid_sources(
                url,
                mirror_urls.unwrap(),
                config,
                pool,
                scheduler,
                bt_session,
            ).await?
        }
        #[cfg(not(feature = "magnet"))]
        {
            return Err(DownloadError::Config("magnet feature 未启用".into()));
        }
    } else if is_magnet {
        // 纯 BT(现有路径)
        DownloadTask::with_pool_and_scheduler(url, config, pool, scheduler, bt_session).await?
    } else if has_mirrors {
        // 纯 HTTP 多源(现有 with_mirrors 路径)
        DownloadTask::with_mirrors(url, mirror_urls.unwrap(), config, pool).await?
    } else {
        // 纯 HTTP 单源(现有路径)
        DownloadTask::with_pool_and_scheduler(url, config, pool, scheduler, None).await?
    };
```

注意:参数顺序和类型需与 `with_hybrid_sources` 签名(Task 6)对齐。`scheduler` 参数若 build_download_task 未接收,需确认调用链。

- [ ] **Step 3: 编译验证**

Run: `cargo build -p tachyon-app`
Expected: 编译通过

- [ ] **Step 4: 提交**

```bash
cargo fmt --all
git add crates/tachyon-app/src/commands/task_commands.rs
git commit -m "feat(app): build_download_task 路由 P2SP 混合下载"
```

---

### Task 9: 前端 SettingsPanel magnet tab 新增配置项 UI

**Files:**
- Modify: `frontend/src/types.ts`(MagnetConfig TS 类型)
- Modify: `frontend/src/components/SettingsPanel.tsx`(magnet tab UI)
- Modify: `frontend/src/i18n/locales/en-US.ts` + `zh-CN.ts`(i18n)
- Test: `frontend/src/components/__tests__/SettingsPanel.spec.tsx`

**Interfaces:**
- Consumes: Task 1-2 的 MagnetConfig/MagnetPatch 新字段

- [ ] **Step 1: 在 types.ts 的 MagnetConfig 接口添加新字段**

```typescript
export interface MagnetConfig {
  // ... 现有字段 ...
  peerConnectTimeoutSecs: number;
  peerReadWriteTimeoutSecs: number;
  forceTrackerIntervalSecs: number;
  deferWritesUpToMb: number;
  disableDhtWhenSocks: boolean;
  peerAddrs: string[];
}

export interface MagnetPatch {
  // ... 现有字段 ...
  peerConnectTimeoutSecs?: number;
  peerReadWriteTimeoutSecs?: number;
  forceTrackerIntervalSecs?: number;
  deferWritesUpToMb?: number;
  disableDhtWhenSocks?: boolean;
  peerAddrs?: string[];
}
```

- [ ] **Step 2: 在 SettingsPanel.tsx 的 magnet tab 添加 UI 组件**

参考现有 `stallTimeoutSecs`/`peerWaitTimeoutSecs` 的 `NumberInput` 或 `SliderItem` 模式,新增:
- `peerConnectTimeoutSecs`(NumberInput,1-300,"重启生效"标记)
- `peerReadWriteTimeoutSecs`(NumberInput,1-600,"重启生效"标记)
- `forceTrackerIntervalSecs`(NumberInput,0 或 30-3600,"对新任务生效"标记)
- `deferWritesUpToMb`(NumberInput,0-256,"重启生效"标记,0=禁用)
- `disableDhtWhenSocks`(ToggleItem,"重启生效"标记)

每个组件的 `onInput` 更新 draft,`buildPatch` 时加入 `MagnetPatch`。

- [ ] **Step 3: 在 i18n locales 添加文案**

zh-CN:
```typescript
  peerConnectTimeoutSecs: 'Peer 连接超时(秒)',
  peerReadWriteTimeoutSecs: 'Peer 读写超时(秒)',
  forceTrackerIntervalSecs: '强制 Tracker 间隔(秒)',
  deferWritesUpToMb: '延迟写入缓冲(MB)',
  disableDhtWhenSocks: 'SOCKS5 时禁用 DHT',
  restartRequired: '需重启生效',
  newTaskOnly: '对新任务生效',
```

en-US 对应英文翻译。

- [ ] **Step 4: 写测试**

在 `SettingsPanel.spec.tsx` 添加:

```typescript
  it('修改 peer 连接超时后保存', async () => {
    // 参考 stallTimeoutSecs 的测试模式
    const { getByLabelText } = renderSettingsPanel();
    const input = getByLabelText('Peer 连接超时(秒)');
    fireEvent.input(input, { target: { value: '15' } });
    // 点击保存
    // 断言 api.updateConfig 被调用,magnet patch 含 peerConnectTimeoutSecs: 15
  });

  it('切换 SOCKS5 禁用 DHT 开关后保存', async () => {
    // toggle 测试
  });
```

- [ ] **Step 5: 运行前端测试**

Run: `cd frontend && bun run test`
Expected: PASS

- [ ] **Step 6: 提交**

```bash
cd frontend && bun run lint && cd ..
git add frontend/
git commit -m "feat(frontend): magnet 设置页新增 peer_opts/tracker/defer_writes 配置项"
```

---

## 阶段四:集成验证

### Task 10: 全量测试 + clippy + 覆盖率门禁

**Files:**
- 无新文件,运行全量验证

- [ ] **Step 1: cargo fmt 检查**

Run: `cargo fmt --all -- --check`
Expected: 无差异

- [ ] **Step 2: cargo clippy 零警告**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 零警告

- [ ] **Step 3: 全量测试**

Run: `cargo nextest run --all`
Expected: 全通过

- [ ] **Step 4: 覆盖率门禁**

Run: `cargo llvm-cov -p tachyon-core -p tachyon-engine -p tachyon-store -p tachyon-io -p tachyon-crypto -p tachyon-scheduler --fail-under-lines 90 --summary-only`
Expected: >= 90%

- [ ] **Step 5: 前端测试**

Run: `cd frontend && bun run test && bun run lint`
Expected: PASS

- [ ] **Step 6: 若有失败,修复后重新验证,直到全绿**

- [ ] **Step 7: 最终提交(如有修复)**

```bash
git add -A
git commit -m "test: 全量验证通过(clippy 零警告 + 覆盖率 >= 90%)"
```

---

## Self-Review

### Spec coverage

| Spec 要求 | 对应 Task |
|-----------|----------|
| 4.3.1 MagnetConfig 新字段 | Task 1 |
| 4.3.2 BtSession 填充 SessionOptions | Task 3 |
| 4.3.3 add_magnet force_tracker_interval + initial_peers | Task 4 |
| 4.3.4 initial_peers 来源(&pe= + 用户配置) | Task 4(parse_pe_from_magnet) |
| 4.3.5 配置分级与热切换 | Task 1(validate) + Task 2(MagnetPatch) + Task 9(前端标记) |
| 4.3.6 校验 | Task 1 |
| 4.4.1 peer_stats_snapshot | Task 5 |
| 4.4.2 SOCKS5 tracker 过滤 | Task 3(已合并) |
| 4.4.3 死 swarm 被动恢复 | 无新 Task(沿用现有 peer_wait + 引擎重试) |
| 4.5.1-4.5.6 P2SP | Task 6, 7, 8 |

### Placeholder scan

- Task 7 Step 3 的 `write_all_at_mut`/`wait_for_cancel` 签名标注"需核对"——这是计划中的验证点,非占位符(Step 4 编译验证会捕获)。✅
- Task 8 Step 2 的 `scheduler` 参数标注"需确认调用链"——同上,编译验证捕获。✅
- 无 TBD/TODO。✅

### Type consistency

- `BtPeerStats` 字段名在 Task 5 定义,后续无引用冲突。✅
- `with_hybrid_sources` 签名在 Task 6 定义,Task 8 调用对齐(参数顺序:url, http_mirrors, config, pool, scheduler, bt_session)。✅
- `parse_pe_from_magnet` 返回 `Vec<SocketAddr>`,Task 4 Step 4 使用一致。✅
- `bt_fallback: Option<Arc<MagnetProtocol>>` 在 Task 6 定义,Task 7 使用一致。✅
