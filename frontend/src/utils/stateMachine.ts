import type { DownloadStatus } from '../types'

export const STATE_ORDER: DownloadStatus[] = ['pending', 'connecting', 'downloading', 'completed']

export interface StateNode {
  id: DownloadStatus
  label: string
  x: number
  y: number
}

export interface StateEdge {
  from: DownloadStatus
  to: DownloadStatus
}

export const STATE_NODES: StateNode[] = [
  { id: 'pending', label: '等待中', x: 40, y: 40 },
  { id: 'connecting', label: '连接中', x: 160, y: 40 },
  { id: 'downloading', label: '下载中', x: 280, y: 40 },
  { id: 'completed', label: '已完成', x: 400, y: 40 },
]

export const STATE_EDGES: StateEdge[] = [
  { from: 'pending', to: 'connecting' },
  { from: 'connecting', to: 'downloading' },
  { from: 'downloading', to: 'completed' },
]

export function getVisitedStates(currentStatus: DownloadStatus): DownloadStatus[] {
  const currentIndex = STATE_ORDER.indexOf(currentStatus)
  if (currentIndex === -1) return []
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
