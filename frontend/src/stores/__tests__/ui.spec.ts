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
})
