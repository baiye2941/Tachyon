# Design Spec：P0-6 对象身份 / P0-7 HLS 最小接入 / P0-8 BT 生命周期

- 日期：2026-07-14
- 基线：Phase0 F1–F6 之后工作树；审计基线 `5dd8bc7c37e0440c6ccc85aae8724ab9c6751a62`
- 状态：**用户已批准设计方向，进入 writing-plans / 实现**
- 父工作流：`docs/aegis/work/2026-07-14-phase0-correctness-fixes/`
- 审计源：`Document/PI/Tachyon-Deep-Audit-2026-07-14/`（P0-6/7/8）

## 1. 目标与非目标

### 目标

1. **P0-6**：消除同长度不同版本对象的静默拼接（resume + range + 镜像）。
2. **P0-7**：`.m3u8` 产品路径走 `HlsProtocol`，VOD 产出媒体分片拼接；live 明确失败。
3. **P0-8**：BT 自定义 storage 无嵌套 `block_on`；preferred 名与写盘一致；cancel/fail/complete 停止 torrent；handle cache 绑定 owner。

### 非目标

- 吞吐优化、work-stealing 重开
- HLS live/EVENT reload、MAP/BYTERANGE/fMP4、多音轨/ remux
- BT SOCKS 内嵌 UDP tracker 过滤、private torrent metadata DHT 隐私
- 公网 swarm / CDN 实验

## 2. 已锁定产品决策

| 项 | 决策 |
|---|---|
| P0-6 | **完整闭环**：probe 身份 + resume 准入 + `If-Range` + 镜像兼容筛选 |
| P0-7 | **正式最小接入** VOD-only；URL 嗅探；镜像+m3u8 拒绝 |
| P0-8 | **所有权+取消** 四条；隐私依赖项延后 |
| 实现顺序 | **P0-6 → P0-8 → P0-7**（Protocol 签名先定） |
| TDD | strict RED→GREEN→交叉复核 |

## 3. P0-6 对象身份

### 3.1 类型与规则

`tachyon-core` 新增：

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ObjectIdentity {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub file_size: Option<u64>,
}
```

兼容判定（`compatible` / resume / mirror）：

1. **strong ETag**（非 `W/` 前缀，忽略两侧引号差异规范化后比较）为主键：双方 strong 且不等 → 不兼容。
2. **weak ETag** 不得作为 `If-Range` 值；也不得单独作为 resume/mirror 同义证明。
3. 无 strong ETag 时：双方均有 `Last-Modified` 且相等 → 兼容；不等 → 不兼容。
4. **仅 size**（无 strong ETag、无可用 Last-Modified）：
   - resume：**不得**沿用 completed/partial（丢弃后全量重下）
   - mirror：**不得**混拼（剔除候选源）
5. 一方完全无身份字段且 size 也缺失：视为未知 → resume 不沿用；mirror 不混拼。

### 3.2 Protocol 契约

```rust
fn download_range(
    &self,
    url: &str,
    start: u64,
    end: u64,
    identity: Option<ObjectIdentity>,
) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>>;

fn download_range_stream(
    &self,
    url: &str,
    start: u64,
    end: u64,
    identity: Option<ObjectIdentity>,
) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>>;
```

- `ObjectIdentity` 按值传递（`Clone` 便宜）。
- 非 HTTP 实现忽略 identity（`_identity`）。
- HTTP：有 strong ETag 或 Last-Modified 时发 `If-Range`；weak 不发。
- 已发 `If-Range` 却收到 **200** 全对象：按验证失败处理，**禁止**截取前缀与旧分片拼接；返回可重试/可触发身份失效的错误（`DownloadError::Protocol` 语义）。
- 206 仍校验 Content-Range start/end。

### 3.3 Resume 准入（canonical owner = engine）

- App `inject_resume_snapshot`：除 fragments 外注入 snapshot 的 `ObjectIdentity`（由 etag/last_modified/file_size 派生）。
- `TaskRunner` / `DownloadTask`：`set_resume_object_identity`。
- `probe()` 成功后、`plan()` 使用 completed/partial 之前：与 `ObjectIdentity::from_metadata` 比较；不兼容则清空 completed/partial 与 resume identity 失效标记，并 log。
- 零 schema bump：沿用既有 snapshot 字段。

### 3.4 镜像筛选

- 首成功 probe 建立 **baseline identity**。
- 后台/后续源 probe 成功后，仅 `compatible(baseline, source)` 的 index 进入 `probe_ok`/可混拼候选。
- 不兼容源永不进入 per-fragment 混拼；可保留作“全失败后单源重探”策略时须文档化——**本切片默认直接剔除**。
- 下载候选：**不得**再把未通过身份筛选的源一律补入。

### 3.5 验收测试

1. wiremock：range 带 strong ETag 的 `If-Range`。
2. 部分写入后 ETag 变更：resume 不跳过旧片。
3. 双镜像同长不同 ETag：禁止混拼，最终字节=单身份对象。
4. weak ETag 不发 `If-Range`。

## 4. P0-8 BT 生命周期

### 4.1 嵌套 block_on

- `TachyonStorageFactory::open_storages_from_metadata` 多文件分支改 `TokioFile::open_sync`（与单文件一致）。
- 禁止在 async runtime worker 上 `Handle::block_on` 打开文件。

### 4.2 preferred 名

- Factory 持有 `preferred_root_name: Option<String>`（或构造时注入）。
- 生产构造：`set_preferred_file_name` **必须**在带 factory 的 `probe` 之前生效；factory 打开路径用最终名。
- 单文件：`download_dir/<preferred|torrent_name>`。
- 多文件根：`download_dir/<sanitize(preferred|torrent_name)>/...`。
- 与 `DownloadTask::init_storage` 路径规则一致。

### 4.3 取消 / 失败 / 完成

- 任务 Cancel、失败终态、正常完成：对对应 `ManagedTorrent` 执行 pause + session remove（或项目现有等价 API），并从 handle cache 剔除。
- `probe_filename` 路径若会 start torrent：须有明确生命周期（本切片至少：下载任务退出清理自己添加/命中的 handle；UI 探测泄漏可记 residual）。

### 4.4 Handle cache 绑定

缓存值扩展为绑定：

- `ManagedTorrent` + `FileLayout`
- `download_dir`
- storage factory 身份（有/无 factory 或 factory key）
- `preferred_root_name`（最终根名）

命中条件全部匹配；否则 invalidate 并重建。

### 4.5 验收测试

1. 多文件 factory 打开无嵌套 block_on（实现为 open_sync + 单测/静态路径）。
2. preferred rename 后写盘路径 = 用户名。
3. cancel 后 cache/session 无活跃 handle（mock 或可观测 hook）。
4. 不同 download_dir 不误命中 cache。

## 5. P0-7 HLS 最小接入

### 5.1 协议选择

- `DownloadTask::with_pool_and_scheduler`：http(s) 且 URL 路径（去 query/fragment）以 `.m3u8`/`.m3u` 结尾 → `HlsProtocol`，否则 `HttpClient`。
- `with_mirrors` 若主 URL 为 m3u8 → `DownloadError::Config` 明确拒绝。

### 5.2 VOD 门

- 无 `#EXT-X-ENDLIST`（live/EVENT）→ probe 或首下载返回明确错误，**不得** Completed。
- 本切片不实现 live reload。

### 5.3 验收测试

1. `DownloadTask::run` + mock master/media/segment：磁盘产物 = TS 拼接，≠ playlist 文本。
2. 无 ENDLIST：失败。
3. 普通 HTTP 非 m3u8 回归。

## 6. 文件地图

| 区域 | 文件 |
|---|---|
| core | `types.rs`, `traits.rs`, `lib.rs`, `test_harness.rs` |
| HTTP | `protocol/src/http.rs` |
| mirror | `engine/src/mirror.rs` |
| engine | `downloader.rs`（identity 传递、resume 比较、HLS 选择） |
| app | `task_commands.rs` inject identity |
| BT | `bt_storage.rs`, `magnet.rs`, `bt_session.rs` |
| HLS | `hls.rs` VOD 门 |

## 7. 兼容与反熵

- Snapshot schema 不 bump。
- Protocol 签名变更：全部实现者同步。
- 删除：多文件 factory 嵌套 `block_on`；未绑定 owner 的 cache 裸命中；镜像无差别补源。
- 保留：HlsProtocol 孤立 API 行为（在 VOD 门内）；SOCKS/private 现状诚实。

## 8. 风险

- `downloader.rs` 体量：身份比较/HLS 选择保持最小 diff；规则放 core。
- Protocol 实现者面广：先改 trait + harness，一次 `cargo check` 清编译错误。
- librqbit remove/pause API 以锁定版本为准，测试优先 mock/hook。

## 9. 批准记录

- 用户选择：P0-6 完整闭环；P0-7 正式最小接入；P0-8 所有权+取消。
- 用户指令：`开始` → 本 spec 进入实现。
