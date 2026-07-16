# Checkpoint — residual 继续(自动不间断)

- Baseline: `55afb7c`
- Status: **IN PROGRESS**（未提交）

## 本会话累计闭环(未提交)

| ID | 项 | 状态 |
|---|---|---|
| FT-03 | dialog:allow-save + notification 插件 + 去 shell | DONE |
| FT-04 | cold 字段 + TaskItem 读 hot | DONE |
| FT-05 | events reject + 重连/校准 | DONE |
| FT-06 | sniffer 虚拟列表 | DONE |
| FT-07 | force_mirror + HfTaskMeta | DONE |
| FT-09–13 | 建任务/竞态/回滚/sparkline/FTP | DONE |
| FT-14 | sniffer aria + HF roving + 去 columnheader 伪 grid | DONE(部分) |
| FT-16 | vite 独立 panel chunks | DONE |

## Verification

```
cargo check/clippy tachyon-app magnet: OK
vitest TaskList/HfBrowser/... 
tsc clean
```

## 诚实残余

- FT-01 可信确认 / FT-02 备份事务
- FT-08 Playwright Tauri E2E
- A-10/openat2/Authenticode/AsyncCancel
