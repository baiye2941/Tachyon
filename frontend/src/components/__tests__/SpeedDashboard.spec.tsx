import { describe, it, expect, beforeEach } from 'vitest'
import { createRoot } from 'solid-js'
import { render } from 'solid-js/web'
import SpeedDashboard from '../SpeedDashboard'
import * as speedHistory from '../../stores/speedHistory'

function renderToDiv(fn: () => unknown): { container: HTMLElement; unmount: () => void } {
  const div = document.createElement('div')
  document.body.appendChild(div)
  let dispose: (() => void) | null = null

  createRoot((d) => {
    dispose = d
    render(() => fn() as Element, div)
  })

  return {
    container: div,
    unmount: () => {
      dispose?.()
      div.remove()
    },
  }
}

describe('SpeedDashboard', () => {
  beforeEach(() => {
    speedHistory.clearHistory()
  })

  it('渲染仪表盘组件', () => {
    const { container, unmount } = renderToDiv(() => <SpeedDashboard />)
    expect(container.querySelector('[data-testid="speed-dashboard"]')).toBeTruthy()
    unmount()
  })

  it('渲染统计卡片', () => {
    const { container, unmount } = renderToDiv(() => <SpeedDashboard />)
    expect(container.textContent).toContain('总速度')
    expect(container.textContent).toContain('活跃')
    expect(container.textContent).toContain('均速')
    expect(container.textContent).toContain('峰值')
    unmount()
  })

  it('显示当前总速度', () => {
    speedHistory.pushSpeed(5242880)
    const { container, unmount } = renderToDiv(() => <SpeedDashboard />)
    const totalSpeed = container.querySelector('[data-testid="stat-total-speed"]')
    expect(totalSpeed?.textContent).toContain('5.0')
    unmount()
  })

  it('显示活跃任务数', () => {
    speedHistory.setActiveTasksCount(3)
    const { container, unmount } = renderToDiv(() => <SpeedDashboard />)
    const activeTasks = container.querySelector('[data-testid="stat-active-tasks"]')
    expect(activeTasks?.textContent).toBe('3')
    unmount()
  })

  it('计算平均速度', () => {
    speedHistory.pushSpeed(1048576)
    speedHistory.pushSpeed(2097152)
    speedHistory.pushSpeed(3145728)
    const { container, unmount } = renderToDiv(() => <SpeedDashboard />)
    const avgSpeed = container.querySelector('[data-testid="stat-avg-speed"]')
    expect(avgSpeed?.textContent).toContain('2.0')
    unmount()
  })

  it('计算峰值速度', () => {
    speedHistory.pushSpeed(1048576)
    speedHistory.pushSpeed(10485760)
    speedHistory.pushSpeed(5242880)
    const { container, unmount } = renderToDiv(() => <SpeedDashboard />)
    const peakSpeed = container.querySelector('[data-testid="stat-peak-speed"]')
    expect(peakSpeed?.textContent).toContain('10.0')
    unmount()
  })

  it('渲染 SVG 波形图', () => {
    const { container, unmount } = renderToDiv(() => <SpeedDashboard />)
    const svg = container.querySelector('svg[data-testid="speed-waveform"]')
    expect(svg).toBeTruthy()
    unmount()
  })

  it('波形图包含 path 元素', () => {
    const { container, unmount } = renderToDiv(() => <SpeedDashboard />)
    const path = container.querySelector('svg[data-testid="speed-waveform"] path')
    expect(path).toBeTruthy()
    unmount()
  })

  it('波形图有正确的 viewBox', () => {
    const { container, unmount } = renderToDiv(() => <SpeedDashboard />)
    const svg = container.querySelector('svg[data-testid="speed-waveform"]')
    expect(svg?.getAttribute('viewBox')).toBe('0 0 140 24')
    unmount()
  })

  it('支持 prefers-reduced-motion', () => {
    const { container, unmount } = renderToDiv(() => <SpeedDashboard />)
    const dashboard = container.querySelector('[data-testid="speed-dashboard"]')
    expect(dashboard?.classList.contains('motion-reduce:transition-none')).toBe(false)
    unmount()
  })
})

describe('speedHistory store', () => {
  beforeEach(() => {
    speedHistory.clearHistory()
  })

  it('环形缓冲区固定长度 60', () => {
    for (let i = 0; i < 70; i++) {
      speedHistory.pushSpeed(i)
    }
    const history = speedHistory.getHistory()
    expect(history.length).toBe(60)
    expect(history[0]).toBe(10)
    expect(history[59]).toBe(69)
  })

  it('getHistory 返回不可变副本', () => {
    speedHistory.pushSpeed(100)
    const h1 = speedHistory.getHistory()
    h1.push(200)
    const h2 = speedHistory.getHistory()
    expect(h2.length).toBe(1)
    expect(h2[0]).toBe(100)
  })

  it('clearHistory 清空数据', () => {
    speedHistory.pushSpeed(100)
    speedHistory.clearHistory()
    expect(speedHistory.getHistory().length).toBe(0)
  })

  it('setActiveTasksCount 更新活跃任务数', () => {
    speedHistory.setActiveTasksCount(5)
    expect(speedHistory.getActiveTasks()).toBe(5)
  })

  it('空历史时平均速度为 0', () => {
    expect(speedHistory.getAverageSpeed()).toBe(0)
  })

  it('空历史时峰值速度为 0', () => {
    expect(speedHistory.getPeakSpeed()).toBe(0)
  })

  it('正确计算平均速度', () => {
    speedHistory.pushSpeed(1000)
    speedHistory.pushSpeed(2000)
    speedHistory.pushSpeed(3000)
    expect(speedHistory.getAverageSpeed()).toBe(2000)
  })

  it('正确计算峰值速度', () => {
    speedHistory.pushSpeed(1000)
    speedHistory.pushSpeed(5000)
    speedHistory.pushSpeed(3000)
    expect(speedHistory.getPeakSpeed()).toBe(5000)
  })
})

describe('SpeedDashboard 波形图数据', () => {
  beforeEach(() => {
    speedHistory.clearHistory()
  })

  it('波形图 path 随数据变化', () => {
    speedHistory.pushSpeed(1048576)
    speedHistory.pushSpeed(2097152)
    const { container: c1, unmount: u1 } = renderToDiv(() => <SpeedDashboard />)
    const path1 = c1.querySelector('svg[data-testid="speed-waveform"] path')?.getAttribute('d')
    u1()

    speedHistory.pushSpeed(5242880)
    const { container: c2, unmount: u2 } = renderToDiv(() => <SpeedDashboard />)
    const path2 = c2.querySelector('svg[data-testid="speed-waveform"] path')?.getAttribute('d')
    u2()

    expect(path1).not.toBe(path2)
  })
})
