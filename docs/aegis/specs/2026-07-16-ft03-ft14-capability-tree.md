# Spec Brief：FT-03 capability/插件闭合 + FT-14 HF tree roving

## FT-03

1. capability 增加 `dialog:allow-save`（导出备份）
2. 注册 `tauri-plugin-notification`，capability 授予最小通知权限集
3. 移除未使用的 `tauri-plugin-shell`（Rust + frontend dep）
4. capability description 与权限列表对齐

## FT-14（HF tree 最小）

1. treeitem 默认 `tabindex=-1`，仅当前焦点项 `0`
2. 在 `role=tree` 上处理 ArrowUp/Down/Home/End，在可见 treeitem DOM 间 roving focus
3. 保留 ←→ 展开/折叠、Space 勾选、Enter 下载

## 非目标

- FT-01 可信确认大改
- FT-02 备份事务化
- FT-08 Playwright Tauri E2E
- TaskList 完整 grid 语义

## 验证

- cargo check tachyon-app magnet
- vitest useTaskNotifications / SettingsPanel / HfBrowser
- capability JSON 含 allow-save 与 notification 权限，无 shell
