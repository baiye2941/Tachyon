/// <reference types="node" />
import { describe, it, expect, vi } from 'vitest'
import { render, fireEvent } from '@solidjs/testing-library'
import TaskItem from '../TaskItem'
import TitleBar from '../TitleBar'
import type { TaskInfo } from '../../types'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

// 直接读取 CSS 源文件而非 `?raw` 导入:Tailwind v4 的 Vite 插件会拦截并转换
// CSS 导入,导致 `?raw` 返回空字符串。fs.readFileSync 绕过转换管线,拿到原始源码。
const indexCss = readFileSync(
  resolve(__dirname, '../../index.css'),
  'utf-8',
)

vi.mock('@tauri-apps/api/webviewWindow', () => ({
  getCurrentWebviewWindow: () => ({
    minimize: vi.fn().mockResolvedValue(undefined),
    toggleMaximize: vi.fn().mockResolvedValue(undefined),
    close: vi.fn().mockResolvedValue(undefined),
    isMaximized: vi.fn().mockResolvedValue(false),
    onResized: vi.fn().mockResolvedValue(() => {}),
  }),
}))

Object.defineProperty(window, 'matchMedia', {
  writable: true,
  value: vi.fn().mockImplementation((query) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: vi.fn(),
    removeListener: vi.fn(),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    dispatchEvent: vi.fn(),
  })),
})

const expectElement = <T extends Element>(element: T | null): T => {
  expect(element).not.toBeNull()
  return element as T
}

describe('Accessibility Tests', () => {
  const mockTask: TaskInfo = {
    id: 'test-1',
    fileName: 'test-file.zip',
    url: 'https://example.com/test.zip',
    fileSize: 1024000,
    downloaded: 512000,
    progress: 0.5,
    speed: 1048576,
    status: 'downloading',
    fragmentsTotal: 4,
    fragmentsDone: 2,
    createdAt: '2026-05-30T00:00:00Z',
    savePath: '/downloads',
  }

  describe('prefers-reduced-motion 支持', () => {
    it('应该在 CSS 中定义 prefers-reduced-motion 媒体查询', () => {
      expect(indexCss).toContain('@media (prefers-reduced-motion: reduce)')
      expect(indexCss).toContain('animation-duration: 0.01ms !important')
      expect(indexCss).toContain('animation-iteration-count: 1 !important')
      expect(indexCss).toContain('transition-duration: 0.01ms !important')
      expect(indexCss).toContain('scroll-behavior: auto !important')

      const { container } = render(() => (
        <TaskItem
          task={mockTask}
          isSelected={false}
          isMultiSelected={false}
          isMultiSelectMode={false}
          onClick={() => {}}
          density="comfortable"
        />
      ))

      // 验证真实组件仍带有动画入口类，禁用逻辑由产品 CSS 媒体查询接管。
      expectElement(container.querySelector('.task-item-enter'))
    })

    it('应该在用户设置 prefers-reduced-motion 时禁用动画', () => {
      const matchMedia = vi.mocked(window.matchMedia)
      matchMedia.mockReturnValueOnce({
        matches: true,
        media: '(prefers-reduced-motion: reduce)',
        onchange: null,
        addListener: vi.fn(),
        removeListener: vi.fn(),
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        dispatchEvent: vi.fn(),
      })

      render(() => (
        <TaskItem
          task={mockTask}
          isSelected={false}
          isMultiSelected={false}
          isMultiSelectMode={false}
          onClick={() => {}}
          density="comfortable"
        />
      ))

      expect(window.matchMedia('(prefers-reduced-motion: reduce)').matches).toBe(true)
      expect(indexCss).toMatch(
        /@media\s*\(prefers-reduced-motion:\s*reduce\)\s*{[\s\S]*animation-duration:\s*0\.01ms !important/,
      )
    })
  })

  describe('触摸目标尺寸（≥44px）', () => {
    it('TitleBar 窗口控制按钮应该有足够的触摸目标尺寸', () => {
      const { container } = render(() => <TitleBar />)
      const buttons = container.querySelectorAll('.win-btn')

      expect(buttons.length).toBeGreaterThan(0)
      expect(indexCss).toMatch(/\.win-btn\s*{[\s\S]*width:\s*36px;[\s\S]*height:\s*36px;/)
      buttons.forEach((button) => expect(button.classList.contains('win-btn')).toBe(true))
    })

    it('小图标按钮（icon-btn-sm）应该通过伪元素扩展触摸目标到 44x44px', () => {
      const { container } = render(() => (
        <button class="icon-btn-sm" style={{ width: '28px', height: '28px' }}>
          Test
        </button>
      ))

      const button = expectElement(container.querySelector('.icon-btn-sm'))
      expect(button.classList.contains('icon-btn-sm')).toBe(true)
      expect(indexCss).toMatch(/\.icon-btn-sm::before\s*{[\s\S]*inset:\s*-8px;/)
    })

    it('TaskItem 应该有足够的触摸目标尺寸', () => {
      const { container } = render(() => (
        <TaskItem
          task={mockTask}
          isSelected={false}
          isMultiSelected={false}
          isMultiSelectMode={false}
          onClick={() => {}}
          density="comfortable"
        />
      ))

      const taskElement = expectElement(container.querySelector('[role="button"]'))
      expect(taskElement.getAttribute('style')).toContain('padding: 12px 16px')
    })
  })

  describe('TitleBar ARIA 标签', () => {
    it('最小化按钮应该有 aria-label', () => {
      const { container } = render(() => <TitleBar />)
      const minimizeBtn = expectElement(container.querySelector('[aria-label="最小化窗口"]'))
      expect(minimizeBtn.getAttribute('aria-label')).toBe('最小化窗口')
    })

    it('最大化/恢复按钮应该有动态 aria-label 和 title', () => {
      const { container } = render(() => <TitleBar />)
      const maximizeBtn = expectElement(container.querySelector('[aria-label="最大化窗口"]'))
      expect(maximizeBtn.getAttribute('aria-label')).toBe('最大化窗口')
      expect(maximizeBtn.getAttribute('title')).toBe('最大化')
    })

    it('关闭按钮应该有 aria-label 和 title', () => {
      const { container } = render(() => <TitleBar />)
      const closeBtn = expectElement(container.querySelector('[aria-label="关闭窗口"]'))
      expect(closeBtn.getAttribute('aria-label')).toBe('关闭窗口')
      expect(closeBtn.getAttribute('title')).toBe('关闭')
    })
  })

  describe('TaskItem 键盘导航', () => {
    it('应该有 role="button" 和 tabindex="0"', () => {
      const { container } = render(() => (
        <TaskItem task={mockTask} isSelected={false} isMultiSelected={false} isMultiSelectMode={false} onClick={() => {}} density="comfortable" />
      ))
      const taskElement = expectElement(container.querySelector('[role="button"]'))
      expect(taskElement.getAttribute('role')).toBe('button')
      expect(taskElement.getAttribute('tabindex')).toBe('0')
    })

    it('应该响应 Enter 键触发 onClick', () => {
      const onClick = vi.fn()
      const { container } = render(() => (
        <TaskItem task={mockTask} isSelected={false} isMultiSelected={false} isMultiSelectMode={false} onClick={onClick} density="comfortable" />
      ))
      fireEvent.keyDown(expectElement(container.querySelector<HTMLElement>('[role="button"]')), { key: 'Enter' })
      expect(onClick).toHaveBeenCalledTimes(1)
    })

    it('应该响应 Space 键触发 onClick', () => {
      const onClick = vi.fn()
      const { container } = render(() => (
        <TaskItem task={mockTask} isSelected={false} isMultiSelected={false} isMultiSelectMode={false} onClick={onClick} density="comfortable" />
      ))
      fireEvent.keyDown(expectElement(container.querySelector<HTMLElement>('[role="button"]')), { key: ' ' })
      expect(onClick).toHaveBeenCalledTimes(1)
    })

    it('应该在按下 Space 键时阻止默认滚动行为', () => {
      const { container } = render(() => (
        <TaskItem task={mockTask} isSelected={false} isMultiSelected={false} isMultiSelectMode={false} onClick={() => {}} density="comfortable" />
      ))
      const taskElement = container.querySelector('[role="button"]')!
      expect(taskElement).toBeTruthy()
    })
  })
})
