import { describe, it, expect, beforeEach, afterEach } from 'vitest'
import {
  loadShortcuts,
  getShortcutKeys,
  setShortcut,
  resetShortcut,
  resetAllShortcuts,
  findConflict,
  matchKeyboardEvent,
  getCommandShortcutKeys,
  DEFAULT_BINDINGS,
} from '../../stores/shortcuts'
import { SHORTCUTS } from '../shortcuts'

describe('快捷键配置 store', () => {
  beforeEach(() => {
    localStorage.clear()
    resetAllShortcuts()
  })

  afterEach(() => {
    localStorage.clear()
    resetAllShortcuts()
  })

  it('默认返回 SHORTCUTS 中的键位', () => {
    for (const s of SHORTCUTS) {
      expect(getShortcutKeys(s.labelKey)).toEqual(s.keys)
      expect(DEFAULT_BINDINGS[s.labelKey]).toEqual(s.keys)
    }
  })

  it('覆盖写入并持久化到 localStorage', () => {
    setShortcut('shortcut.openCommandPalette', ['Ctrl', 'Shift', 'P'])
    expect(getShortcutKeys('shortcut.openCommandPalette')).toEqual(['Ctrl', 'Shift', 'P'])

    const raw = localStorage.getItem('tachyon.shortcuts')
    expect(raw).toBeTruthy()
    const parsed = JSON.parse(raw!)
    expect(parsed.version).toBe(1)
    expect(parsed.overrides['shortcut.openCommandPalette']).toEqual(['Ctrl', 'Shift', 'P'])
  })

  it('loadShortcuts 从 localStorage 恢复覆盖,未覆盖项回退默认', () => {
    localStorage.setItem(
      'tachyon.shortcuts',
      JSON.stringify({
        version: 1,
        overrides: {
          'shortcut.toggleSidebar': ['Ctrl', 'Shift', 'B'],
        },
      }),
    )
    loadShortcuts()

    expect(getShortcutKeys('shortcut.toggleSidebar')).toEqual(['Ctrl', 'Shift', 'B'])
    expect(getShortcutKeys('shortcut.openCommandPalette')).toEqual(['Ctrl', 'K'])
  })

  it('resetShortcut 移除单条覆盖并回退默认', () => {
    setShortcut('shortcut.toggleSidebar', ['Ctrl', 'Shift', 'B'])
    resetShortcut('shortcut.toggleSidebar')

    expect(getShortcutKeys('shortcut.toggleSidebar')).toEqual(['Ctrl', 'B'])
    const raw = localStorage.getItem('tachyon.shortcuts')
    expect(JSON.parse(raw!).overrides['shortcut.toggleSidebar']).toBeUndefined()
  })

  it('resetAllShortcuts 清空所有覆盖', () => {
    setShortcut('shortcut.toggleSidebar', ['Ctrl', 'Shift', 'B'])
    setShortcut('shortcut.task.new', ['Ctrl', 'T'])
    resetAllShortcuts()

    expect(getShortcutKeys('shortcut.toggleSidebar')).toEqual(['Ctrl', 'B'])
    expect(getShortcutKeys('shortcut.task.new')).toEqual(['Ctrl', 'N'])
    const raw = localStorage.getItem('tachyon.shortcuts')
    expect(JSON.parse(raw!).overrides).toEqual({})
  })

  it('findConflict 返回冲突项的 labelKey', () => {
    // 默认 toggleSidebar 是 Ctrl+B，设置 openCommandPalette 为 Ctrl+B 应冲突
    const conflict = findConflict('shortcut.openCommandPalette', ['Ctrl', 'B'])
    expect(conflict).toBe('shortcut.toggleSidebar')
  })

  it('findConflict 与自身当前绑定相同不视为冲突', () => {
    setShortcut('shortcut.openCommandPalette', ['Ctrl', 'Shift', 'P'])
    const conflict = findConflict('shortcut.openCommandPalette', ['Ctrl', 'Shift', 'P'])
    expect(conflict).toBeUndefined()
  })

  it('findConflict 无冲突时返回 undefined', () => {
    const conflict = findConflict('shortcut.openCommandPalette', ['Ctrl', 'Shift', 'X'])
    expect(conflict).toBeUndefined()
  })

  it('matchKeyboardEvent 命中组合键', () => {
    const event = new KeyboardEvent('keydown', { key: 'K', ctrlKey: true })
    expect(matchKeyboardEvent(event, 'shortcut.openCommandPalette')).toBe(true)
  })

  it('matchKeyboardEvent 命中单键', () => {
    const event = new KeyboardEvent('keydown', { key: 'Enter' })
    expect(matchKeyboardEvent(event, 'shortcut.list.openDetail')).toBe(true)
  })

  it('matchKeyboardEvent 大小写不敏感', () => {
    const event = new KeyboardEvent('keydown', { key: 'k', ctrlKey: true })
    expect(matchKeyboardEvent(event, 'shortcut.openCommandPalette')).toBe(true)
  })

  it('matchKeyboardEvent 额外修饰键不命中', () => {
    const event = new KeyboardEvent('keydown', { key: 'K', ctrlKey: true, shiftKey: true })
    expect(matchKeyboardEvent(event, 'shortcut.openCommandPalette')).toBe(false)
  })

  it('matchKeyboardEvent 缺少主键/修饰键不命中', () => {
    const event = new KeyboardEvent('keydown', { key: 'B', ctrlKey: true })
    expect(matchKeyboardEvent(event, 'shortcut.openCommandPalette')).toBe(false)
  })

  it('matchKeyboardEvent macOS Cmd 命中 Ctrl 标记', () => {
    const originalUAData = Object.getOwnPropertyDescriptor(window.navigator, 'userAgentData')
    const originalUA = Object.getOwnPropertyDescriptor(window.navigator, 'userAgent')
    Object.defineProperty(window.navigator, 'userAgentData', {
      value: { platform: 'macOS' },
      configurable: true,
    })
    Object.defineProperty(window.navigator, 'userAgent', {
      value: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)',
      configurable: true,
    })

    try {
      const event = new KeyboardEvent('keydown', { key: 'K', metaKey: true })
      expect(matchKeyboardEvent(event, 'shortcut.openCommandPalette')).toBe(true)
    } finally {
      if (originalUAData) {
        Object.defineProperty(window.navigator, 'userAgentData', originalUAData)
      } else {
        // @ts-expect-error cleanup
        delete (window.navigator as any).userAgentData
      }
      if (originalUA) {
        Object.defineProperty(window.navigator, 'userAgent', originalUA)
      }
    }
  })

  it('损坏的 localStorage 数据被忽略', () => {
    localStorage.setItem('tachyon.shortcuts', JSON.stringify({ version: 2, overrides: {} }))
    loadShortcuts()
    expect(getShortcutKeys('shortcut.toggleSidebar')).toEqual(['Ctrl', 'B'])

    localStorage.setItem('tachyon.shortcuts', JSON.stringify({ version: 1, overrides: 'bad' }))
    loadShortcuts()
    expect(getShortcutKeys('shortcut.toggleSidebar')).toEqual(['Ctrl', 'B'])

    localStorage.setItem(
      'tachyon.shortcuts',
      JSON.stringify({
        version: 1,
        overrides: {
          'shortcut.toggleSidebar': ['Ctrl', 'Shift', 'B'],
          'shortcut.task.new': [1, 2] as unknown as string[],
          'shortcut.openCommandPalette': 'Ctrl+K' as unknown as string[],
        },
      }),
    )
    loadShortcuts()
    expect(getShortcutKeys('shortcut.toggleSidebar')).toEqual(['Ctrl', 'Shift', 'B'])
    expect(getShortcutKeys('shortcut.task.new')).toEqual(['Ctrl', 'N'])
    expect(getShortcutKeys('shortcut.openCommandPalette')).toEqual(['Ctrl', 'K'])
  })

  it('Space 键规范化保存为 Space', () => {
    setShortcut('shortcut.list.togglePause', [' '])
    expect(getShortcutKeys('shortcut.list.togglePause')).toEqual(['Space'])
  })

  it('仅按修饰键被忽略', () => {
    for (const key of ['Control', 'Alt', 'Shift', 'Meta']) {
      const event = new KeyboardEvent('keydown', { key, ctrlKey: key === 'Control' })
      expect(matchKeyboardEvent(event, 'shortcut.openCommandPalette')).toBe(false)
    }
  })

  it('findConflict 检测到 A 覆盖为 B 的默认值且 B 未被覆盖', () => {
    // toggleSidebar 默认 Ctrl+B，将其覆盖为 openCommandPalette 的默认值 Ctrl+K
    const conflict = findConflict('shortcut.toggleSidebar', ['Ctrl', 'K'])
    expect(conflict).toBe('shortcut.openCommandPalette')
  })

  it('getCommandShortcutKeys 返回 commandId 对应键位', () => {
    expect(getCommandShortcutKeys('act-toggle-sidebar')).toEqual(['Ctrl', 'B'])
    expect(getCommandShortcutKeys('task-new')).toEqual(['Ctrl', 'N'])
    expect(getCommandShortcutKeys('unknown')).toBeUndefined()
    expect(getCommandShortcutKeys(undefined)).toBeUndefined()
  })
})
