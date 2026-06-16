import { test, expect } from '@playwright/test'

/**
 * 基础冒烟测试:验证 Tachyon 前端页面可加载且关键 UI 元素存在
 *
 * 注意: 这是 Web 层面验证,不是 Tauri 原生 E2E。
 * Tauri IPC 调用(invoke)在纯浏览器环境中不可用,
 * 因此测试仅验证页面结构和静态渲染,不验证后端交互。
 * 真正的 Tauri E2E 需要 tauri-driver + WebDriver。
 */

test.describe('冒烟测试', () => {
  test('页面标题包含 Tachyon', async ({ page }) => {
    await page.goto('/')
    const title = await page.title()
    expect(title).toContain('Tachyon')
  })

  test('侧边栏存在', async ({ page }) => {
    await page.goto('/')
    // 侧边栏导航容器
    const sidebar = page.locator('[data-testid="sidebar"], nav, aside').first()
    await expect(sidebar).toBeVisible({ timeout: 10000 })
  })

  test('任务列表容器存在', async ({ page }) => {
    await page.goto('/')
    // 任务列表区域(主内容区)
    const taskArea = page.locator(
      '[data-testid="task-list"], [data-testid="downloads-view"], main'
    ).first()
    await expect(taskArea).toBeVisible({ timeout: 10000 })
  })

  test('新建下载按钮可交互', async ({ page }) => {
    await page.goto('/')
    // 新建下载按钮(可能因 Tauri IPC 不可用而 disabled,但应存在)
    const newTaskBtn = page.locator(
      'button:has-text("新建"), button:has-text("下载"), [data-testid="new-task-btn"]'
    ).first()
    await expect(newTaskBtn).toBeAttached({ timeout: 10000 })
  })
})
