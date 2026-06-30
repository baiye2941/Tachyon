import { describe, it, expect, vi, afterEach } from 'vitest'
import { render, cleanup, fireEvent } from '@solidjs/testing-library'
import { useGlobalKeyboard } from '../useGlobalKeyboard'
import { openNewTaskModal } from '../../stores/ui'

vi.mock('../../stores/ui', () => ({
  openNewTaskModal: vi.fn(),
  toggleCommandPalette: vi.fn(),
  toggleShortcutHelp: vi.fn(),
  toggleSidebar: vi.fn(),
}))

function TestHarness() {
  useGlobalKeyboard()
  return <input aria-label="url" />
}

describe('useGlobalKeyboard', () => {
  afterEach(() => {
    cleanup()
    vi.clearAllMocks()
  })

  it('Ctrl+N 打开新建下载', () => {
    render(() => <TestHarness />)

    fireEvent.keyDown(window, { key: 'n', ctrlKey: true })

    expect(openNewTaskModal).toHaveBeenCalledTimes(1)
  })

  it('Cmd+N 打开新建下载', () => {
    render(() => <TestHarness />)

    fireEvent.keyDown(window, { key: 'N', metaKey: true })

    expect(openNewTaskModal).toHaveBeenCalledTimes(1)
  })

  it('输入框内 Ctrl+N 不拦截编辑行为', () => {
    render(() => <TestHarness />)
    const input = document.querySelector('input')!

    fireEvent.keyDown(input, { key: 'n', ctrlKey: true })

    expect(openNewTaskModal).not.toHaveBeenCalled()
  })
})
