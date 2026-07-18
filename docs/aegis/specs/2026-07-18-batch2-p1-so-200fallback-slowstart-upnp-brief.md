# Spec Brief:第二批 P1 修复(BT-16 so= + 200 fallback + slow start + BT-15 UPnP)

## 背景
审计 `Document/PI/Tachyon-Deep-Audit-2026-07-14` 残留 P1 项。经探索确认:
- H-05 已修(progress_lock + revision CAS + tombstone),跳过
- BT-16 未处理:Tachyon 不解析 so=,probe 返回全部文件,engine 按全部规划
- 200 fallback 未处理:N 片时带宽放大 ~N/2 倍
- 冷启动 slow start 未处理:无样本直接开满并发
- BT-15 半问题:默认 enable_upnp=false 不触发,用户显式开启时静默不工作

## 范围与用户决策

### BT-16 so= 主动解析(方案 A)
- `magnet.rs` 新增 `parse_so_from_magnet(uri) -> Vec<usize>` 解析 so= 为 file_id 列表(BEP 9 逗号分隔 0-based)
- `layout_from_file_infos` 接受 `only_files: Option<&[usize]>` 过滤,file_size = 选中文件 len 之和
- `probe` / `download_range_stream` / `download_full_stream` 调用 `add_magnet_to_session` 时显式传 `AddTorrentOptions.only_files`
- `file_size` 重算为选中文件大小之和

### 200 fallback 运行时降级 + 预探测(方案 A+B)
- `http.rs` `probe` 阶段:GET Range:0-0 若返回 200(非 206),标记 `supports_range=false`
- `http.rs` `download_range_stream`:收到 200 时返回 `DownloadError::RangeNotSupported`(新变体)而非 fallback stream
- `downloader.rs` `execute_fragmented_download`:捕获 `RangeNotSupported`,设置 `self.metadata.supports_range=false`,re-plan 为单分片,转 `execute_full_download`
- 后续任务断点续传时 snapshot 记录 `supports_range=false`

### 冷启动 slow start(方案 A:渐进爬坡)
- `config.rs` `SchedulerConfig` 新增 `cold_start_initial_concurrency: u32`(默认 1)+ `cold_start_ramp_factor: f64`(默认 2.0)
- `download_scheduler.rs` `recommend`:无样本时 `concurrency = cold_start_initial_concurrency`
- `downloader.rs`:首个分片完成 → `observe_bandwidth` → `recommend` → `set_target` 爬坡(倍增 1→2→4→max)
- 现有 `reschedule_timer` 周期性 `set_target` 自动爬坡

### BT-15 UPnP 自动设默认端口(方案 A)
- `bt_session.rs` `build_session_options`:`enable_upnp=true` 且 `listen_port_range.is_none()` 时自动设 `6881..6889`
- `config.rs` 可选新增 `bt_listen_port_start/end: Option<u16>` 允许自定义(默认 None)

## 不变量
- BT-16:so= 存在时 probe 返回的 file_size/layout 仅含选中文件;engine plan 不覆盖未选文件
- 200 fallback:服务器不支持 Range 时只传输 1 次(整块),非 N/2 倍
- slow start:冷启动并发从 1 起步,样本到位后爬坡到 max
- BT-15:enable_upnp=true 时必有 listen_port_range,UPnP 真正启动

## 非目标
- BT-16:不处理 so= 与 torrent 文件内 file_infos 顺序不一致(BEP 9 规定 so= 按 file_infos 顺序)
- 200 fallback:不处理"部分 Range 支持,部分 200"的混合场景(罕见)
- slow start:不实现 BBR/Cubic 等拥塞控制算法(仅并发度爬坡)
- BT-15:不做 UPnP 设备发现/映射验证(依赖 librqbit 内部)

## TDD
严格 RED→GREEN→REFACTOR,垂直切片。

## 验证
```bash
cargo nextest run --all
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```
