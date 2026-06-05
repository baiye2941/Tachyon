import { describe, it, expect } from 'vitest'
import { render } from '@solidjs/testing-library'
import StateMachine from '../StateMachine'
import type { DownloadStatus } from '../../types'

describe('StateMachine 组件', () => {
  it('渲染状态机容器', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="pending" />)
    expect(getByTestId('state-machine')).toBeTruthy()
  })

  it('渲染所有状态节点', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="pending" />)
    expect(getByTestId('node-pending')).toBeTruthy()
    expect(getByTestId('node-connecting')).toBeTruthy()
    expect(getByTestId('node-downloading')).toBeTruthy()
    expect(getByTestId('node-completed')).toBeTruthy()
  })

  it('渲染所有状态流转边', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="pending" />)
    expect(getByTestId('edge-pending-connecting')).toBeTruthy()
    expect(getByTestId('edge-connecting-downloading')).toBeTruthy()
    expect(getByTestId('edge-downloading-completed')).toBeTruthy()
  })

  it('pending 状态时 pending 节点高亮', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="pending" />)
    const node = getByTestId('node-pending')
    expect(node).toBeTruthy()
    const circle = node.querySelector('circle')
    expect(circle?.getAttribute('fill')).toBe('#00d2ff')
  })

  it('connecting 状态时 pending 节点为绿色已访问', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="connecting" />)
    const node = getByTestId('node-pending')
    const circle = node.querySelector('circle')
    expect(circle?.getAttribute('fill')).toBe('#10b981')
  })

  it('connecting 状态时 connecting 节点高亮', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="connecting" />)
    const node = getByTestId('node-connecting')
    const circle = node.querySelector('circle')
    expect(circle?.getAttribute('fill')).toBe('#00d2ff')
  })

  it('downloading 状态时 pending 和 connecting 节点为绿色', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="downloading" />)
    const pendingCircle = getByTestId('node-pending').querySelector('circle')
    const connectingCircle = getByTestId('node-connecting').querySelector('circle')
    expect(pendingCircle?.getAttribute('fill')).toBe('#10b981')
    expect(connectingCircle?.getAttribute('fill')).toBe('#10b981')
  })

  it('completed 状态时所有节点为绿色', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="completed" />)
    const ids: DownloadStatus[] = ['pending', 'connecting', 'downloading']
    for (const id of ids) {
      const circle = getByTestId(`node-${id}`).querySelector('circle')
      expect(circle?.getAttribute('fill')).toBe('#10b981')
    }
    // completed 节点既是当前状态也是已访问状态，显示为电青色
    const completedCircle = getByTestId('node-completed').querySelector('circle')
    expect(completedCircle?.getAttribute('fill')).toBe('#00d2ff')
  })

  it('当前状态节点有脉冲动画', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="downloading" />)
    const node = getByTestId('node-downloading')
    const circle = node.querySelector('circle')
    expect(circle?.classList.contains('animate-pulse')).toBe(true)
  })

  it('非当前状态节点无脉冲动画', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="downloading" />)
    const node = getByTestId('node-pending')
    const circle = node.querySelector('circle')
    expect(circle?.classList.contains('animate-pulse')).toBe(false)
  })

  it('当前状态节点半径更大', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="connecting" />)
    const currentNode = getByTestId('node-connecting').querySelector('circle')
    const otherNode = getByTestId('node-pending').querySelector('circle')
    expect(currentNode?.getAttribute('r')).toBe('14')
    expect(otherNode?.getAttribute('r')).toBe('10')
  })

  it('已激活的边使用实线', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="connecting" />)
    const edge = getByTestId('edge-pending-connecting')
    expect(edge?.getAttribute('stroke-dasharray')).toBe('0')
  })

  it('未激活的边使用虚线', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="pending" />)
    const edge = getByTestId('edge-connecting-downloading')
    expect(edge?.getAttribute('stroke-dasharray')).toBe('4 3')
  })
})

describe('StateMachine 状态流转', () => {
  it('pending -> connecting 流转正确', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="connecting" />)
    expect(getByTestId('node-pending').querySelector('circle')?.getAttribute('fill')).toBe('#10b981')
    expect(getByTestId('node-connecting').querySelector('circle')?.getAttribute('fill')).toBe('#00d2ff')
    expect(getByTestId('node-downloading').querySelector('circle')?.getAttribute('fill')).toBe('rgba(255,255,255,0.1)')
  })

  it('connecting -> downloading 流转正确', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="downloading" />)
    expect(getByTestId('node-pending').querySelector('circle')?.getAttribute('fill')).toBe('#10b981')
    expect(getByTestId('node-connecting').querySelector('circle')?.getAttribute('fill')).toBe('#10b981')
    expect(getByTestId('node-downloading').querySelector('circle')?.getAttribute('fill')).toBe('#00d2ff')
    expect(getByTestId('node-completed').querySelector('circle')?.getAttribute('fill')).toBe('rgba(255,255,255,0.1)')
  })

  it('downloading -> completed 流转正确', () => {
    const { getByTestId } = render(() => <StateMachine currentStatus="completed" />)
    expect(getByTestId('node-pending').querySelector('circle')?.getAttribute('fill')).toBe('#10b981')
    expect(getByTestId('node-connecting').querySelector('circle')?.getAttribute('fill')).toBe('#10b981')
    expect(getByTestId('node-downloading').querySelector('circle')?.getAttribute('fill')).toBe('#10b981')
    // completed 是当前状态，显示为电青色
    expect(getByTestId('node-completed').querySelector('circle')?.getAttribute('fill')).toBe('#00d2ff')
  })
})
