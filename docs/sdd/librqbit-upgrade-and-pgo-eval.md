# librqbit 升级/Fork 评估 + PGO 实测

日期: 2026-07-24  
基线: Tachyon `c845c72`, librqbit **8.1.1**, Windows MSVC + rustc 1.96

## 1. 结论（先看这个）

| 议题 | 决策 | 理由 |
|------|------|------|
| **Fork librqbit 8.1.1 暴露 bootstrap** | **不做** | v9.0.0-rc 已原生暴露 `DhtSessionConfig.bootstrap_addrs`；fork 的 rebase 成本高于升级 |
| **升级 librqbit 9.x** | **等 stable 后做**（现为 rc.0） | 能关闭 P-04；API 有中等破坏面，rc 不进生产主线 |
| **默认开启 PGO release** | **不做** | 同会话实测关键路径 `e2e_execute_download` **无 >10% 收益**（噪声内 / 略回退）；工具链保留 `scripts/ci/pgo.sh` |

---

## 2. librqbit：Fork vs 升级

### 2.1 问题陈述（P-04）

8.1.1 路径：

- DHT 默认 bootstrap 仅 2 节点：`dht.transmissionbt.com:6881`、`dht.libtorrent.org:25401`
- 底层 `DhtConfig.bootstrap_addrs: Option<Vec<String>>` 支持自定义
- 但 `SessionOptions.dht_config: Option<PersistentDhtConfig>` **只有** `dump_interval` / `config_filename`，**不含** bootstrap
- `Session::new_with_opts` 构造 DHT 时无法注入 bootstrap

国内冷启动 metadata 常 20–40s，自定义国内可达 bootstrap 是合理诉求。

### 2.2 8.1.1 Fork 成本（不推荐）

最小 fork 补丁：

1. 扩展 `PersistentDhtConfig` 或 `SessionOptions` 增加 `bootstrap_addrs`
2. 改 `session.rs` DHT 创建处把字段传入 `DhtConfig`
3. 长期 **rebase 上游**（协议/安全修复）

维护面：

- 需同时 pin `librqbit` + 传递依赖（`librqbit-dht`/`core`/…）
- CI 要编译 fork（git dep 或 path/patch）
- 与 9.x 上游已公开的 API **重复劳动**

### 2.3 9.0.0-rc.0 已解决 bootstrap（推荐方向）

源码（registry `librqbit-9.0.0-rc.0`）新增：

```rust
pub struct DhtSessionConfig {
    /// Bootstrap nodes (host:port or ip:port). Uses built-in defaults if None.
    pub bootstrap_addrs: Option<Vec<String>>,
    pub port: Option<u16>,
    pub persistence: Option<DhtPersistenceConfig>,
}

pub struct SessionOptions {
    /// DHT configuration. Set to None to disable DHT entirely.
    pub dht: Option<DhtSessionConfig>,
    // ...
    pub connect: Option<ConnectionOptions>, // 含 proxy_url
    pub peer_limit: Option<usize>,          // 8.x 无公开 max peer
    // ...
}
```

`Session::new_with_opts` 将 `dht_config.bootstrap_addrs` 传给 `PersistentDht::create` / `DhtBuilder::with_config`。

**默认 bootstrap 列表仍是那 2 个**——但 **API 已可配置**，P-04 的“不可行”在 9.x 失效。

### 2.4 8 → 9 破坏面（Tachyon 接入点）

主要落在 `crates/tachyon-engine/src/bt_session.rs` 的 `build_session_options`：

| 8.1.1 字段 | 9.0.0-rc 对应 | 备注 |
|------------|---------------|------|
| `disable_dht` / `disable_dht_persistence` / `dht_config: PersistentDhtConfig` | `dht: Option<DhtSessionConfig>` | `None`=关 DHT；persistence/bootstrap 进子结构 |
| `socks_proxy_url: Option<String>` | `connect: Some(ConnectionOptions { proxy_url, peer_opts, ... })` | SOCKS 仍在，路径变了 |
| `peer_opts: Option<PeerConnectionOptions>` | 挂到 `ConnectionOptions.peer_opts` | 超时配置需搬家 |
| `listen_port_range` + `enable_upnp_port_forwarding` | `listen: Option<ListenerOptions>` | 结构重写 |
| `defer_writes_up_to` | 需再核对 9.x 是否保留/改名 | 迁移时编译器会指明 |
| （无） | `peer_limit: Option<usize>` | 可顺带关掉 T4 部分限制 |
| （无） | `bootstrap_addrs` | **P-04 收益点** |

协议层 `tachyon-protocol` magnet 生命周期 / FileStream 适配也要在升级分支上全量 `nextest`（含 seeder fixture）。

### 2.5 推荐路径

1. **现在**: 不 fork 8.1.1；文档继续指向 `docs/sdd/magnet-speedup-limitations.md`，并注明 **9.x 已解 API 封锁**。
2. **9.0.0 stable 发布后**: 开 `feat/librqbit-9`：
   - 升级 workspace `librqbit`/`librqbit-core`
   - 重写 `build_session_options`
   - `MagnetConfig` 增加可选 `dht_bootstrap_addrs: Vec<String>`（默认内置 + 可选国内列表）
   - 回归：magnet 单测 + 国内 SOCKS 手工冷启动计时
3. **仅当** 9.x 长期不稳或上游收回 bootstrap API：再评估 fork/patch-crate。

工作量粗估（升级，非 fork）：**1–2 人日** API 适配 + **0.5–1 人日** 测试/文档；fork 长期维护 **> 升级**。

---

## 3. PGO 实测收益对比

### 3.1 方法

- 平台: Windows 11 + MSVC, rustc 1.96
- 工具: rustup `llvm-tools` 自带 `llvm-profdata.exe`
- 训练: `RUSTFLAGS=-Cprofile-generate=target/pgo/raw` + `TACHYON_BENCH_MODE=ci cargo bench --bench e2e_download`
- 合并: `llvm-profdata merge -o target/pgo/merged.profdata raw/*.profraw`（~19MB）
- 对比: 同会话 **先 baseline release，再 PGO use** 各跑一轮  
  `e2e_download --sample-size 15 --warm-up-time 1 --measurement-time 3`
- 主路径: `e2e_execute_download/4MiB_4frag_chunked_mock`（真实 `DownloadTask` 分片路径）

> AGENTS.md: Windows criterion 相对变化噪声大；收益须 **>10%** 才保留复杂度；同会话连续对比优先。

### 3.2 绝对时间（criterion 中位）

| Bench | Baseline | PGO use | Δ (中位) |
|-------|----------|---------|----------|
| **e2e_execute_download/4MiB_4frag_chunked_mock** | **2.419 ms** | **2.450 ms** | **+1.3%** |
| e2e_bandwidth/record_estimate_1000 | 2.152 µs | 2.149 µs | −0.1% |
| e2e_fragment_lifecycle/full_lifecycle | 90.2 ns | 86.2 ns | −4.4% |
| e2e_fragment_size/compute/* | ~216 ps | ~214 ps | ~−1% |

插桩训练轮本身比 baseline 慢约 5–12%（符合 profile-generate 开销预期）。

### 3.3 解读

1. **热路径下载 mock 无显著加速**（+1.3% 落在噪声/回归侧）。
2. 微基准生命周期 −4% 不够 10% 门槛，且不代表端到端吞吐。
3. 当前 release 已是 `lto=true` + `codegen-units=1` + `opt-level=3`，PGO 边际空间被吃掉是常见现象。
4. 训练 workload 偏 mock/CPU 调度，**未**覆盖真实 HTTP/磁盘/H2；若将来做 PGO，应用 **e2e_http_real + 代表性用户脚本** 再测。
5. **决策**: 默认 release **不**开 PGO；保留 `scripts/ci/pgo.sh` 供本地实验。不满足 AGENTS「收益 >10% 否则 revert」的引入标准。

### 3.4 工具链注意（Windows）

- `llvm-profdata` 不在 PATH，在  
  `$(rustc --print sysroot)/lib/rustlib/x86_64-pc-windows-msvc/bin/llvm-profdata.exe`
- `scripts/ci/pgo.sh` 应以 rustup 路径为回退（见同提交脚本修订）
- 大量 `pgo-warn-missing-function` 来自未训练到的依赖泛型实例，可忽略或收窄训练二进制

---

## 4. 后续可选工单

1. **跟踪** `librqbit` 9.0.0 stable；开升级 PR 模板（SessionOptions 映射表见 §2.4）。
2. **配置**: `MagnetConfig.dht_bootstrap_addrs` + 文档推荐国内节点列表（升级后）。
3. **PGO**: 仅在 Linux CI runner 上对 `e2e_http_real` 再做一次同会话对比；若仍 <10%，关闭话题。
4. **不**建长期 8.1.1 fork。

## 5. 证据路径

- 本机 profile: `target/pgo/merged.profdata`（本地，不入库）
- Bench 日志: `target/pgo/baseline-e2e.txt`, `target/pgo/pgo-e2e.txt`
- 上游源码: `C:/cargo/registry/src/index.crates.io-*/librqbit-{8.1.1,9.0.0-rc.0}`
