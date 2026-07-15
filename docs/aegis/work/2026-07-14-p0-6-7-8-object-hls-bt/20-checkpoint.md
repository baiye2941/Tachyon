# Checkpoint — P0-6/7/8

- Status: **DONE**（含 residual: probe lifecycle + ops_gate）
- 临时垃圾文件 `NUL` / `bin_api_probe_tmp.rs` 已删除

## Progress

| 切片 | 状态 |
|---|---|
| P0-6 Object identity | **DONE** |
| P0-7 HLS wiring + e2e | **DONE** |
| P0-8 BT lifecycle + probe + ops_gate | **DONE** |

## Residual (honest)

1. session pause/delete 后台 spawn + 5s 超时；ops_gate 保证 add 等待 cleanup
2. raw URL 兼容键仍在 preferred=None 时写入
3. 未提交：需用户明确要求才 commit

## ResumeStateHint

本工作流完成。可选：`git commit` 或开启新切片。
