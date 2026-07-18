# Spec Brief：恢复声明的执行与校验完整性

## 背景

`resume-local-extent-admission` 已使快照 claims 仅在远端 `ObjectIdentity`、本地 extent、
当前分片几何均匹配时进入 `Done` / `resume_offset`。后续真实 `run()` 路径审查发现三个
执行层不变量尚未闭合：

1. 单分片任务被路由到整文件下载，即使协议支持 Range；这会绕过已准入的
   `resume_offset`，并会让已是 `Done` 的分片再次进入 `complete_download_fast`。
2. Range 流在动态 work-stealing 的 `effective_end` 裁剪前未检查原始 body 长度；服务端对
   本次请求多发的数据会被静默 slice 掉。
3. partial resume 的流式哈希仅覆盖新下载后缀，却被当作完整分片哈希与 expected hash 比较。

## 范围

1. Resume claim 的执行能力必须与协议能力一致：
   - `supports_range = true` 时，即使当前计划仅有一个分片，也必须走 range/fragment 执行路径；
     completed claim 跳过请求，partial claim 从 `start + resume_offset` 请求后缀。
   - `supports_range = false` 时，完成与部分 claims 均必须在应用 fragment 状态前整组丢弃，
     因整文件响应不能安全解释为任一 offset 的后缀；任务从零走 full-download。
2. `download_single_fragment` 为每一次 protocol range 请求冻结
   `{requested_start, requested_end, requested_len}`。在任何动态 `effective_end` 裁剪、buffer
   裁剪或写盘前，对原始 ByteStream 累计长度做 fail-closed 检查：已交付字节超过
   `requested_len` 必须返回 `DownloadError::Fragment`。
   - 动态 `effective_end` 仍只处理 work-stealing 后原 worker 已失去所有权的尾部写入，
     不得用它掩盖服务端对本次 Range 请求多发的数据。
3. 若分片 `resume_offset > 0`，下载阶段不得生成可代表完整分片的 `computed_hash`；
   `verify()` 必须从 storage 回读整个分片 `{start,size}` 再与 expected hash 比较。
   这样既验证已持久化前缀，也验证新下载后缀。
4. `verify()` 的 storage 回读循环遇到零字节读取时必须失败，不能无限循环；正短读可继续读取。
5. 对携带 whole-fragment expected hash 的分片，work-stealing 不得改变其验证范围；本次采用
   最小安全策略：禁止该类 fragment 进入 `try_split`。完整的 hash-range 重分配不是本次范围。

## 不变量

- 任何已应用的 completed/partial claim 都必须有一个与实际执行路径兼容的解释；不可 Range
  续传的 full-response 路径不得携带 offset 语义。
- Range body 是否越界以发送请求时冻结的范围判断，而不是以随后可变化的 work-stealing 边界或
  已裁剪写入字节判断。
- `computed_hash: Some` 的语义固定为“覆盖 `FragmentInfo` 整个当前验证范围的哈希”；不能把
  后缀或 split 前缀哈希保存为完整分片哈希。
- 验证读盘必须有进展；`read_at == 0` 且仍有剩余字节是数据/存储错误。
- 违反上述任何不变量时失败关闭的是当前下载或恢复 claims，不能静默截断、错位写入或标记完成。

## 非目标

- 不实现 H-01 完整 owner epoch、已提交 `write_at` 取消或旧 writer quiesce；`effective_end`
  裁剪与 queue-full rollback 已有，但 explicit `enable_work_stealing` 的 ownership TOCTOU 仍是
  已登记 residual，且默认配置保持关闭。
- 不引入跨进程 fragment-plan fingerprint、TaskSnapshot schema 变更、协议 trait API 变更或
  server-side checkpoint。
- 不实现 partial prefix 的增量流式 hash；回读完整分片是本次正确性优先的实现。
- 不实现 expected-hash fragment 的 hash-range 拆分/合并；此类分片本次不参与 work-stealing。

## TDD

严格 RED → GREEN → REFACTOR：

1. 固定后续分片数据的普通 overlong Range 和 partial overlong Range 均先 RED；
2. 单分片 Range completed/partial、非 Range completed/partial capability contract 先 RED；
3. partial resume expected hash 正确前缀、损坏前缀、verify 零读、hash fragment 禁止 split 先 RED；
4. 每轮最小 GREEN 后运行相关 engine suites 与 clippy。

## 验证

```bash
cargo nextest run -p tachyon-engine -- overlong
cargo nextest run -p tachyon-engine -- resume
cargo nextest run -p tachyon-engine -- verify
cargo nextest run -p tachyon-engine -- work_stealing
cargo clippy -p tachyon-engine --all-targets --all-features -- -D warnings
```
