export type DownloadStatus = 'pending' | 'connecting' | 'downloading' | 'paused' | 'resuming' | 'verifying' | 'completed' | 'failed' | 'cancelled'

/** 原型组件沿用的别名，与 DownloadStatus 等价 */
export type DownloadState = DownloadStatus

/** 校验策略 — 与后端 tachyon_core::config::VerifyStrategy 对齐 */
export type VerifyStrategy = 'require' | 'bestEffort' | 'skip'

/** I/O 存储后端策略 — 与后端 tachyon_core::config::IoStrategy 对齐 */
export type IoStrategy = 'standard' | 'winAligned' | 'iocp' | 'ioUring'

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
  savePath: string
  /** 失败原因原文(仅 status='failed' 时有值),后端 TaskInfo.error_reason */
  errorReason?: string
  /** 任务级重试计数(保留字段,当前恒为 0) */
  retryCount?: number
}

export interface DownloadConfig {
  downloadDir: string
  maxConcurrentFragments: number
  maxRetries: number
  requestTimeoutSecs: number
  connectTimeoutSecs: number
  verifyChecksum: boolean
  /** 校验策略(后端默认 bestEffort)。可选以兼容旧版无此字段的快照 */
  verifyStrategy?: VerifyStrategy
  pauseTimeoutSecs: number
  rateLimitBytesPerSec?: number | null
  maxFullStreamBytes: number
  authorizedDirs: string[]
  userAgent: string
  headers: Record<string, string>
  /** I/O 存储后端策略(后端默认 standard)。可选以兼容旧版无此字段的快照 */
  ioStrategy?: IoStrategy
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

/** 配置白名单补丁 — 仅包含允许前端修改的字段 */
export interface ConfigPatch {
  maxConcurrentTasks?: number
  download?: DownloadPatch
  connection?: ConnectionPatch
}

/** 下载配置白名单补丁 */
export interface DownloadPatch {
  downloadDir?: string
  maxConcurrentFragments?: number
  maxRetries?: number
  requestTimeoutSecs?: number
  connectTimeoutSecs?: number
  verifyChecksum?: boolean
  pauseTimeoutSecs?: number
  rateLimitBytesPerSec?: number | null
  ioStrategy?: IoStrategy
}

/** 连接配置白名单补丁 */
export interface ConnectionPatch {
  maxConnectionsPerHost?: number
  maxGlobalConnections?: number
  keepAliveTimeoutSecs?: number
  connectTimeoutSecs?: number
  enableHttp2?: boolean
  enableQuic?: boolean
}

export type SnifferResourceType = 'video' | 'audio' | 'document' | 'archive' | 'executable' | 'image' | 'model' | 'other'

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
  status: DownloadStatus
  fragmentsDone: number
}

export type ProgressEvent = Record<string, ProgressPayload>

export type ViewName = 'downloads' | 'sniffer' | 'settings' | 'history' | 'hub' | 'stats'

export type DownloadFilter = 'all' | 'downloading' | 'completed' | 'incomplete'

/** ---- 原型 UI 状态类型 ---- */

export type ListDensity = 'comfortable' | 'compact'

export type SidebarFilter = 'all' | 'downloading' | 'completed' | 'paused' | 'failed'

export type FileTypeFilter = 'all' | 'video' | 'audio' | 'document' | 'image' | 'archive' | 'executable' | 'model' | 'other'

export interface ToastMessage {
  id: string
  type: 'success' | 'error' | 'warning' | 'info'
  title: string
  description?: string
  actions?: { label: string; onClick: () => void }[]
  duration?: number
}

export interface HfLfsInfo {
  oid: string
  size: number
}

export interface HubFileInfo {
  type: string
  path: string
  size: number
  lfs?: HfLfsInfo | null
}

export interface SpeedDataPoint {
  timestamp: number
  speed: number
}

/** 下载进度详情 -- 与后端 DownloadProgress 对齐 */
export interface DownloadProgress {
  taskId: string
  status: DownloadStatus
  progress: number
  downloaded: number
  fileSize: number | null
  speed: number
  fragmentsTotal: number
  fragmentsDone: number
}

/** 应用信息 -- 与后端 AppInfo 对齐 */
export interface AppInfo {
  version: string
  name: string
}
