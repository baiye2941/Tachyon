import { describe, it, expect, beforeEach, vi } from 'vitest'
import { createRoot } from 'solid-js'

let uiModule: typeof import('../ui')

function read<T>(fn: () => T): T {
  return createRoot((dispose) => {
    try {
      return fn()
    } finally {
      dispose()
    }
  })
}

beforeEach(async () => {
  vi.resetModules()
  uiModule = await import('../ui')
  // 重置所有面板状态为关闭
  uiModule.closeSniffer()
  uiModule.closeHistory()
  uiModule.closeSettings()
  uiModule.closeNewTaskModal()
  uiModule.closeCommandPalette()
})

describe('ui store', () => {
  it('初始状态所有面板均关闭', () => {
    const state = uiModule.readUiState()
    expect(state.snifferVisible).toBe(false)
    expect(state.historyVisible).toBe(false)
    expect(state.settingsVisible).toBe(false)
    expect(state.newTaskModalOpen).toBe(false)
    expect(state.commandPaletteOpen).toBe(false)
  })

  it('openSniffer 打开嗅探面板并关闭其他面板', () => {
    uiModule.openSettings()
    uiModule.openSniffer()
    const state = uiModule.readUiState()
    expect(state.snifferVisible).toBe(true)
    expect(state.settingsVisible).toBe(false)
  })

  it('openHistory 打开历史面板', () => {
    uiModule.openHistory()
    expect(read(() => uiModule.$ui.historyVisible())).toBe(true)
  })

  it('openSettings 打开设置面板', () => {
    uiModule.openSettings()
    expect(read(() => uiModule.$ui.settingsVisible())).toBe(true)
  })

  it('toggleSniffer 切换嗅探面板状态', () => {
    uiModule.toggleSniffer()
    expect(read(() => uiModule.$ui.snifferVisible())).toBe(true)
    uiModule.toggleSniffer()
    expect(read(() => uiModule.$ui.snifferVisible())).toBe(false)
  })

  it('openNewTaskModal 打开新建任务弹窗', () => {
    uiModule.openNewTaskModal()
    expect(read(() => uiModule.$ui.newTaskModalOpen())).toBe(true)
  })

  it('openCommandPalette 打开命令面板', () => {
    uiModule.openCommandPalette()
    expect(read(() => uiModule.$ui.commandPaletteOpen())).toBe(true)
  })

  it('openView 统一打开指定视图', () => {
    uiModule.openView('sniffer')
    expect(read(() => uiModule.$ui.snifferVisible())).toBe(true)

    uiModule.openView('history')
    expect(read(() => uiModule.$ui.snifferVisible())).toBe(false)
    expect(read(() => uiModule.$ui.historyVisible())).toBe(true)

    uiModule.openView('settings')
    expect(read(() => uiModule.$ui.historyVisible())).toBe(false)
    expect(read(() => uiModule.$ui.settingsVisible())).toBe(true)
  })

  it('openView 切换到 downloads 关闭所有面板', () => {
    uiModule.openView('sniffer')
    uiModule.openView('downloads')
    const state = uiModule.readUiState()
    expect(state.snifferVisible).toBe(false)
    expect(state.historyVisible).toBe(false)
    expect(state.settingsVisible).toBe(false)
  })

  it('closeView 关闭对应视图', () => {
    uiModule.openView('sniffer')
    uiModule.closeView('sniffer')
    expect(read(() => uiModule.$ui.snifferVisible())).toBe(false)
  })

  it('closeView command 关闭命令面板', () => {
    uiModule.openCommandPalette()
    uiModule.closeView('command')
    expect(read(() => uiModule.$ui.commandPaletteOpen())).toBe(false)
  })

  // —— 侧边栏状态(Iteration 13)——
  it('toggleSidebarPin 切换 pinned 状态并同步 collapsed', () => {
    const initialPinned = read(() => uiModule.$ui.sidebarPinned())
    const initialCollapsed = read(() => uiModule.$ui.sidebarCollapsed())

    uiModule.$ui.toggleSidebarPin()

    expect(read(() => uiModule.$ui.sidebarPinned())).toBe(!initialPinned)
    // pinned 时 collapsed 必为 false;非 pinned 时 collapsed 必为 true
    if (!initialPinned) {
      expect(read(() => uiModule.$ui.sidebarCollapsed())).toBe(false)
    } else {
      expect(read(() => uiModule.$ui.sidebarCollapsed())).toBe(true)
    }

    // 恢复
    uiModule.$ui.toggleSidebarPin()
    expect(read(() => uiModule.$ui.sidebarPinned())).toBe(initialPinned)
    expect(read(() => uiModule.$ui.sidebarCollapsed())).toBe(initialCollapsed)
  })

  it('toggleSidebar 在非 pinned 时切换 collapsed', () => {
    // 确保非 pinned
    if (read(() => uiModule.$ui.sidebarPinned())) {
      uiModule.$ui.toggleSidebarPin()
    }
    uiModule.$ui.setSidebarCollapsed(true)
    expect(read(() => uiModule.$ui.sidebarCollapsed())).toBe(true)

    uiModule.toggleSidebar()
    expect(read(() => uiModule.$ui.sidebarCollapsed())).toBe(false)

    uiModule.toggleSidebar()
    expect(read(() => uiModule.$ui.sidebarCollapsed())).toBe(true)
  })

  it('commitSidebarWidth 更新宽度并持久化', () => {
    uiModule.$ui.commitSidebarWidth(250)
    expect(read(() => uiModule.$ui.sidebarWidth())).toBe(250)

    const stored = JSON.parse(localStorage.getItem('tachyon-sidebar-state') ?? '{}')
    expect(stored.width).toBe(250)
  })

  it('侧边栏常量已导出且符合设计约束', () => {
    // spec 8.3:轨道 48px,展开默认 240px,可拖拽 180-400px
    expect(uiModule.SIDEBAR_RAIL_WIDTH).toBe(48)
    expect(uiModule.SIDEBAR_MIN_EXPANDED_WIDTH).toBe(180)
    expect(uiModule.SIDEBAR_MAX_WIDTH).toBe(400)
    expect(uiModule.SIDEBAR_DEFAULT_WIDTH).toBe(240)
  })
})
