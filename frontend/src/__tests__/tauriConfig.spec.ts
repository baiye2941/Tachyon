import { describe, it, expect } from 'vitest'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

// Tauri 窗口配置回归测试:
// Tauri v2 窗口默认 dragDropEnabled=true,OS 文件拖入被原生捕获只发 tauri://drag-drop 事件,
// 前端 DOM drop(NewTaskModal/useDragDrop 拖 .txt 批量导入链接)在发布版静默失效,
// 必须显式禁用原生拖放让事件穿透到 webview。
describe('tauri.conf.json 窗口配置', () => {
  // vitest 的 cwd = frontend,配置文件在 ../crates/tachyon-app/
  const configPath = join(process.cwd(), '../crates/tachyon-app/tauri.conf.json')

  const readConfig = () => JSON.parse(readFileSync(configPath, 'utf8')) as {
    app: { windows: Array<Record<string, unknown>> }
  }

  it('配置文件为合法 JSON 且存在主窗口', () => {
    const config = readConfig()
    expect(Array.isArray(config.app.windows)).toBe(true)
    expect(config.app.windows.length).toBeGreaterThan(0)
  })

  it('守卫: windows[0] 是主窗口(label === "main")', () => {
    const config = readConfig()
    expect(config.app.windows[0]!.label).toBe('main')
  })

  it('主窗口禁用原生拖放(dragDropEnabled === false),DOM drop 才能收到文件', () => {
    const config = readConfig()
    expect(config.app.windows[0]!.dragDropEnabled).toBe(false)
  })

  it('无边框窗口设置最小尺寸(minWidth: 480 / minHeight: 360),补偿系统保护缺失', () => {
    // decorations: false 的无边框窗口没有系统级最小尺寸保护,
    // 用户可把窗口拖到任意小导致布局崩坏,必须在配置层兜底。
    const config = readConfig()
    expect(config.app.windows[0]!.minWidth).toBe(480)
    expect(config.app.windows[0]!.minHeight).toBe(360)
  })
})
