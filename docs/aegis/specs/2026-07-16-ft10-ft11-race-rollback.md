# Spec Brief：FT-10 异步竞态 + FT-11 嗅探配置回滚

## 目标

1. **FT-10**：NewTaskModal HF 预览与 probeFilename
   - effect 任意路径 `clearTimeout` + `onCleanup`
   - HF/probe 使用单调 request token；响应前比对当前 URL/repo 快照
2. **FT-11**：`handleUpdateSnifferConfig` 保存 previous；IPC 失败回滚；可选 generation 丢弃乱序成功

## 非目标

- FT-14 ARIA 大改
- AbortController 跨所有 hub API（token 比对足够）

## TDD

- NewTaskModal / unit 测 token 或行为：快速切换 URL 不应用旧 probe（可测逻辑抽 helper）
- App 层 sniffer 回滚：失败后 config 回到 previous（抽纯函数或测试 handle）
