import { describe, it, expect, vi, afterEach } from 'vitest'
import { render, cleanup } from '@solidjs/testing-library'
import { Icon, getIconPath, ICON_NAMES } from '../../utils/icons'

afterEach(() => {
  cleanup()
})

describe('Icon proxy system', () => {
  it('渲染已知名称图标时不崩溃并输出 svg', () => {
    const { container } = render(() => <Icon name="plus" class="w-4 h-4" />)
    expect(container.querySelector('svg')).toBeTruthy()
  })

  it('批量工具栏与命令面板使用的图标名称均已注册', () => {
    const requiredNames = [
      'plus',
      'pause',
      'play',
      'trash',
      'list-bullet',
      'magnifying-glass',
      'cog-6-tooth',
      'clock',
      'chart-bar',
      'pause-circle',
    ]
    requiredNames.forEach((name) => {
      expect(ICON_NAMES).toContain(name)
    })
  })

  it('未知图标在 DEV 环境下返回 null 并输出 warn', () => {
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {})

    const { container } = render(() => <Icon name="unknown-icon" />)

    expect(container.querySelector('svg')).toBeFalsy()
    expect(warnSpy).toHaveBeenCalledWith('[Icon] 未知图标: unknown-icon')

    warnSpy.mockRestore()
  })

  it('getIconPath 对所有名称返回 undefined', () => {
    expect(getIconPath('plus')).toBeUndefined()
    expect(getIconPath('unknown')).toBeUndefined()
  })
})
