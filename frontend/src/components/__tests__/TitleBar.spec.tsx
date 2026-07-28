import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, fireEvent, cleanup, waitFor } from '@solidjs/testing-library'
import TitleBar from '../TitleBar'

// 窗口 API mock:组件在 onMount 中动态 import('@tauri-apps/api/webviewWindow'),
// vi.mock 对动态 import 同样生效;vi.hoisted 保证工厂执行前 mock 已初始化。
// 注:组件实际导入的是 webviewWindow(非 window)模块,mock 必须对准真实导入路径。
const windowMocks = vi.hoisted(() => ({
  minimize: vi.fn().mockResolvedValue(undefined),
  toggleMaximize: vi.fn().mockResolvedValue(undefined),
  close: vi.fn().mockResolvedValue(undefined),
  isMaximized: vi.fn().mockResolvedValue(false),
  startDragging: vi.fn().mockResolvedValue(undefined),
  onResized: vi.fn().mockResolvedValue(() => {}),
}))

vi.mock('@tauri-apps/api/webviewWindow', () => ({
  getCurrentWebviewWindow: () => windowMocks,
}))

describe('TitleBar 双击切换最大化', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    windowMocks.isMaximized.mockResolvedValue(false)
  })

  afterEach(() => {
    cleanup()
  })

  // onMount 的动态 import 是异步的,等 syncMaximized 调到 isMaximized 即 appWindow 就绪
  const renderReady = async () => {
    const utils = render(() => <TitleBar />)
    await waitFor(() => expect(windowMocks.isMaximized).toHaveBeenCalled())
    return utils
  }

  it('双击拖拽区空白(data-tauri-drag-region)调用 toggleMaximize', async () => {
    const { container } = await renderReady()
    const dragArea = container.querySelector('.flex-1[data-tauri-drag-region]')
    expect(dragArea).not.toBeNull()

    fireEvent.doubleClick(dragArea!)

    expect(windowMocks.toggleMaximize).toHaveBeenCalledTimes(1)
  })

  it('双击窗口控制按钮不触发最大化切换', async () => {
    const { container } = await renderReady()
    const maximizeBtn = container.querySelector('[aria-label="最大化窗口"]')
    expect(maximizeBtn).not.toBeNull()

    fireEvent.doubleClick(maximizeBtn!)

    expect(windowMocks.toggleMaximize).not.toHaveBeenCalled()
  })

  it('双击窗口控制按钮区容器空隙不触发最大化切换(stopPropagation)', async () => {
    // 按钮之间的空隙不属于任何 button,closest('button') 守卫覆盖不到,
    // 必须由按钮区容器 stopPropagation 拦截,否则双击空隙会误触最大化。
    const { container } = await renderReady()
    const minimizeBtn = container.querySelector('[aria-label="最小化窗口"]')
    const controlsArea = minimizeBtn!.parentElement
    expect(controlsArea).not.toBeNull()

    fireEvent.doubleClick(controlsArea!)

    expect(windowMocks.toggleMaximize).not.toHaveBeenCalled()
  })
})
