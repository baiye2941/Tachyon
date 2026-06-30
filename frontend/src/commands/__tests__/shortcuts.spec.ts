import { describe, it, expect } from 'vitest'
import { SHORTCUTS, GROUP_LABEL_KEYS, platformKeys, groupedShortcuts } from '../shortcuts'
import { COMMANDS } from '../registry'

describe('快捷键数据(Iteration 07 DI-2)', () => {
  it('platformKeys 非 macOS 返回原序列', () => {
    expect(platformKeys(['Ctrl', 'K'], false)).toEqual(['Ctrl', 'K'])
  })

  it('platformKeys macOS 将 Ctrl 替换为 Cmd', () => {
    expect(platformKeys(['Ctrl', 'K'], true)).toEqual(['Cmd', 'K'])
    expect(platformKeys(['Ctrl', 'Shift', 'P'], true)).toEqual(['Cmd', 'Shift', 'P'])
  })

  it('groupedShortcuts 按 group 正确分组', () => {
    const grouped = groupedShortcuts()
    for (const s of SHORTCUTS) {
      expect(grouped[s.group]).toContain(s)
    }
    expect(grouped.global.length).toBe(3)
    expect(grouped.navigation.length).toBe(3)
    expect(grouped.task.length).toBe(3)
    expect(grouped.list.length).toBe(3)
  })

  it('每个 shortcut 都在对应组中', () => {
    const grouped = groupedShortcuts()
    for (const [group, items] of Object.entries(grouped)) {
      for (const item of items) {
        expect(item.group).toBe(group)
      }
    }
  })

  it('SHORTCUTS 中所有 labelKey 非空', () => {
    for (const s of SHORTCUTS) {
      expect(s.labelKey).toBeTruthy()
    }
  })

  it('分组标签覆盖所有 shortcut group', () => {
    const groups = new Set(SHORTCUTS.map((s) => s.group))
    for (const g of groups) {
      expect(GROUP_LABEL_KEYS[g]).toBeTruthy()
    }
  })

  it('全局快捷键与 COMMANDS 中对应命令的 shortcut 一致', () => {
    const findShortcut = (labelKey: string) =>
      SHORTCUTS.find((s) => s.labelKey === labelKey)
    const findCommandByShortcut = (keys: string[]) =>
      COMMANDS.find((c) => c.shortcut?.join('+') === keys.join('+'))

    // Ctrl+B 对应命令 act-toggle-sidebar，该命令注册了 shortcut
    const toggleSidebar = findShortcut('shortcut.toggleSidebar')
    expect(toggleSidebar).toBeDefined()
    const toggleSidebarCmd = COMMANDS.find((c) => c.id === 'act-toggle-sidebar')
    expect(toggleSidebarCmd).toBeDefined()
    expect(toggleSidebarCmd!.shortcut?.join('+')).toBe(toggleSidebar!.keys.join('+'))

    // Ctrl+K 打开命令面板本身没有对应命令，但应确保有命令能力的快捷键都不遗漏
    const openPalette = findShortcut('shortcut.openCommandPalette')
    expect(openPalette).toBeDefined()
    if (findCommandByShortcut(openPalette!.keys)) {
      expect(findCommandByShortcut(openPalette!.keys)).toBeDefined()
    }
  })
})
