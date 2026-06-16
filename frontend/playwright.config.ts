import { defineConfig } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  timeout: 30000,
  retries: 0,
  use: {
    // Tauri dev server 默认端口
    baseURL: 'http://localhost:1420',
  },
  // 不添加到 CI: Tauri E2E 需要 display server,CI 无头环境不支持
  // 真正的 Tauri E2E 需要 tauri-driver + WebDriver,超出脚手架范围
  // 本配置仅用于本地开发时验证 Web 层面功能
})
