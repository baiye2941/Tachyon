# Spec Brief：FT-12 Sparkline 响应式 + FT-13 拒绝 FTP

## 目标

1. **FT-12**：`speedHistory.pushSpeed` 更新必须使 StatusBar `createMemo` 失效；环形缓冲可保留，但需 Solid 可读信号（version 或 history signal）。
2. **FT-13**：前端 `validateUrl` 不再把 `ftp://` 标为 valid；协议集合与后端一致（http/https/magnet/hf）；提示 invalid。

## 非目标

- 完整 `supported_protocols` 动态驱动校验矩阵
- FT-14 ARIA 大改
- FTP 协议实现

## TDD

- vitest：`validateUrl('ftp://...')` → valid false
- vitest：`pushSpeed` 后 `getHistory` 依赖的 signal 变化（或 StatusBar/speedHistory unit）
