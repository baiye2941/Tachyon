export type DownloadStatus = 'pending' | 'connecting' | 'downloading' | 'paused' | 'resuming' | 'verifying' | 'completed' | 'failed' | 'cancelled'

/** 原型组件沿用的别名，与 DownloadStatus 等价 */
export type DownloadState = DownloadStatus

export interface TaskInfo {
  id: string
  url: string
  fileName: string
  fileSize: number | null
  downloaded: number
  speed: number
  status: DownloadStatus
  progress: number
  fragmentsTotal: number
  fragmentsDone: number
  createdAt: string
}

export interface DownloadConfig {
  downloadDir: string
  maxConcurrentFragments: number
  maxRetries: number
  requestTimeoutSecs: number
  connectTimeoutSecs: number
  verifyChecksum: boolean
  pauseTimeoutSecs: number
  rateLimitBytesPerSec?: number | null
  maxFullStreamBytes: number
  authorizedDirs: string[]
  userAgent: string
  headers: Record<string, string>
}

export interface ConnectionConfig {
  maxConnectionsPerHost: number
  maxGlobalConnections: number
  keepAliveTimeoutSecs: number
  connectTimeoutSecs: number
  enableHttp2: boolean
  enableQuic: boolean
}

export interface SchedulerConfig {
  minFragmentSize: number
  maxFragmentSize: number
  samplingIntervalSecs: number
  ewmaAlpha: number
}

export interface AppConfig {
  maxConcurrentTasks: number
  download: DownloadConfig
  connection: ConnectionConfig
  scheduler: SchedulerConfig
}

export type SnifferResourceType = 'video' | 'audio' | 'document' | 'archive' | 'executable' | 'image' | 'other'

export interface SnifferResource {
  id: string
  url: string
  name: string
  type: SnifferResourceType
  size: number | null
  contentType?: string
  discoveredAt: number
  sourcePage?: string
}

export interface ProgressPayload {
  id: string
  progress: number
  downloaded: number
  speed: number
  status: string
  fragmentsDone: number
}

export type ProgressEvent = Record<string, ProgressPayload>

export type ViewName = 'downloads' | 'sniffer' | 'settings' | 'history' | 'stats'

export type DownloadFilter = 'all' | 'downloading' | 'completed' | 'incomplete'

/** ---- 原型 UI 状态类型 ---- */

export type ListDensity = 'comfortable' | 'compact'

export type SidebarFilter = 'all' | 'downloading' | 'completed' | 'paused' | 'failed'

export type FileTypeFilter = 'all' | 'video' | 'audio' | 'document' | 'image' | 'archive' | 'executable' | 'other'

export interface ToastMessage {
  id: string
  type: 'success' | 'error' | 'warning' | 'info'
  title: string
  description?: string
  actions?: { label: string; onClick: () => void }[]
  duration?: number
}

export interface SpeedDataPoint {
  timestamp: number
  speed: number
}
