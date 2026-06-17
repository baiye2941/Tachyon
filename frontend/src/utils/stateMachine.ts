import type { DownloadStatus } from '../types'
import type { MessageKey } from '../i18n'

// 完整状态顺序：理想路径 + 分支路径
export const STATE_ORDER: DownloadStatus[] = [
  'pending', 'connecting', 'downloading', 'verifying', 'completed',
]

export interface StateNode {
  id: DownloadStatus
  /** i18n key(渲染时翻译,避免模块加载时固化语言) */
  labelKey: MessageKey
  x: number
  y: number
}

export interface StateEdge {
  from: DownloadStatus
  to: DownloadStatus
}

export const STATE_NODES: StateNode[] = [
  { id: 'pending', labelKey: 'status.label.pending', x: 40, y: 80 },
  { id: 'connecting', labelKey: 'status.label.connecting', x: 160, y: 80 },
  { id: 'downloading', labelKey: 'status.label.downloading', x: 280, y: 80 },
  { id: 'paused', labelKey: 'status.label.paused', x: 280, y: 20 },
  { id: 'resuming', labelKey: 'status.label.resuming', x: 220, y: 20 },
  { id: 'verifying', labelKey: 'status.label.verifying', x: 400, y: 80 },
  { id: 'completed', labelKey: 'status.label.completed', x: 520, y: 80 },
  { id: 'failed', labelKey: 'status.label.failed', x: 400, y: 140 },
  { id: 'cancelled', labelKey: 'status.label.cancelled', x: 280, y: 140 },
]

export const STATE_EDGES: StateEdge[] = [
  // 理想路径
  { from: 'pending', to: 'connecting' },
  { from: 'connecting', to: 'downloading' },
  { from: 'downloading', to: 'verifying' },
  { from: 'verifying', to: 'completed' },
  // 暂停/恢复路径
  { from: 'downloading', to: 'paused' },
  { from: 'paused', to: 'resuming' },
  { from: 'resuming', to: 'downloading' },
  // 错误/取消路径
  { from: 'downloading', to: 'failed' },
  { from: 'downloading', to: 'cancelled' },
  { from: 'connecting', to: 'failed' },
]

export function getVisitedStates(currentStatus: DownloadStatus): DownloadStatus[] {
  const currentIndex = STATE_ORDER.indexOf(currentStatus)
  if (currentIndex === -1) {
    // 非理想路径状态：显示已走过的阶段
    const branchVisited: DownloadStatus[] = ['pending']
    if (currentStatus === 'paused' || currentStatus === 'resuming' || currentStatus === 'cancelled') {
      branchVisited.push('connecting', 'downloading')
    } else if (currentStatus === 'failed') {
      branchVisited.push('connecting', 'downloading')
    }
    return branchVisited
  }
  return STATE_ORDER.slice(0, currentIndex + 1)
}

export function isCurrentState(status: DownloadStatus, currentStatus: DownloadStatus): boolean {
  return status === currentStatus
}

export function isVisitedState(status: DownloadStatus, currentStatus: DownloadStatus): boolean {
  const visited = getVisitedStates(currentStatus)
  return visited.includes(status)
}

export interface NodeStyle {
  fill: string
  stroke: string
  radius: number
  strokeWidth: number
  hasPulse: boolean
}

export interface LabelStyle {
  fill: string
}

export function getNodeStyle(
  status: DownloadStatus,
  currentStatus: DownloadStatus,
): NodeStyle {
  const current = isCurrentState(status, currentStatus)
  const visited = isVisitedState(status, currentStatus)

  if (current) {
    return {
      fill: '#00d2ff',
      stroke: '#00d2ff',
      radius: 14,
      strokeWidth: 2,
      hasPulse: true,
    }
  }

  if (visited) {
    return {
      fill: '#10b981',
      stroke: '#10b981',
      radius: 10,
      strokeWidth: 1,
      hasPulse: false,
    }
  }

  return {
    fill: 'rgba(255,255,255,0.1)',
    stroke: 'rgba(255,255,255,0.2)',
    radius: 10,
    strokeWidth: 1,
    hasPulse: false,
  }
}

export function getLabelStyle(
  status: DownloadStatus,
  currentStatus: DownloadStatus,
): LabelStyle {
  const current = isCurrentState(status, currentStatus)
  const visited = isVisitedState(status, currentStatus)

  if (current) {
    return { fill: '#00d2ff' }
  }

  if (visited) {
    return { fill: '#10b981' }
  }

  return { fill: 'rgba(255,255,255,0.4)' }
}
