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
  /** 当前活跃并发分片数。后端 ProgressPayload 高频同步,可选以兼容旧版快照 */
  activeConcurrency?: number
  /** 失败原因原文(仅 status='failed' 时有值),后端 TaskInfo.error_reason */
  errorReason?: string
  /** 任务级重试计数(保留字段,当前恒为 0) */
  retryCount?: number
  /** 用户自定义任务标签,用于分组/过滤 */
  tags?: string[]
  /** HF 任务元数据(HuggingFace 模型下载任务特有) */
  hfMeta?: HfTaskMeta
  /** 任务在列表中的显示顺序,越小越靠前。手动拖拽排序后由后端写入 */
  displayOrder?: number
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
  /** I/O 存储后端策略。后端默认按平台自适应: Windows→iocp, Linux 5.4+→ioUring, 其他→standard。可选以兼容旧版无此字段的快照 */
  ioStrategy?: IoStrategy
  /** 显式代理 URL(http/socks5),null 时 reqwest 读取系统环境变量(HTTP_PROXY/HTTPS_PROXY/ALL_PROXY) */
  proxy?: string | null
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

/** HuggingFace 源模式 — 与后端 tachyon_core::config::HfSourceMode 对齐 */
export type HfSourceMode = 'official' | 'mirror' | 'race'

export interface HubConfig {
  sourceMode: HfSourceMode
}

export interface MagnetConfig {
  metadataTimeoutSecs: number
  downloadTimeoutSecs: number
  enableDht: boolean
  enableUpnp: boolean
  trackers: string[]
  disableDhtPersistence: boolean
  peerWaitTimeoutSecs: number
  socksProxyUrl: string | null
  /** peer 连接超时(秒),1-300,后端默认 8 */
  peerConnectTimeoutSecs: number
  /** peer 读写超时(秒),1-600,后端默认 10 */
  peerReadWriteTimeoutSecs: number
  /** 强制 tracker 重新 announce 间隔(秒),0=禁用 或 30-3600,后端默认 120 */
  forceTrackerIntervalSecs: number
  /** 延迟写入缓冲上限(MB),0=禁用(同步写入) 或 1-256,后端默认 16 */
  deferWritesUpToMb: number
  /** SOCKS5 启用时是否禁用 DHT(UDP 不可经 SOCKS5),后端默认 true */
  disableDhtWhenSocks: boolean
  /** 预置 peer 地址列表(host:port),从磁力链接 &pe= 解析 + 用户配置合并 */
  peerAddrs: string[]
}

export interface NotificationsConfig {
  enabled: boolean
}

/** 剪贴板监听配置(与后端 ClipboardConfig 对齐,camelCase) */
export interface ClipboardConfig {
  /** 是否启用剪贴板监听,默认 false */
  enableWatch?: boolean
  /** 轮询间隔(毫秒),默认 1000 */
  pollIntervalMs?: number
}

export interface AppConfig {
  maxConcurrentTasks: number
  download: DownloadConfig
  connection: ConnectionConfig
  scheduler: SchedulerConfig
  magnet: MagnetConfig
  hub: HubConfig
  notifications: NotificationsConfig
  /** 剪贴板监听配置(P1-23-A) */
  clipboard?: ClipboardConfig
}

/** 配置白名单补丁 - 仅包含允许前端修改的字段 */
export interface ConfigPatch {
  maxConcurrentTasks?: number
  download?: DownloadPatch
  connection?: ConnectionPatch
  magnet?: MagnetPatch
  scheduler?: SchedulerPatch
  hub?: HubPatch
  notifications?: NotificationsPatch
  /** 剪贴板监听配置补丁(P1-23-A) */
  clipboard?: ClipboardPatch
}

/** 剪贴板监听配置白名单补丁(P1-23-A) */
export interface ClipboardPatch {
  enableWatch?: boolean
  pollIntervalMs?: number
}

/** 系统通知配置白名单补丁 */
export interface NotificationsPatch {
  enabled?: boolean
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
  /** 显式代理 URL,null 表示清除(回退系统环境变量) */
  proxy?: string | null
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

/** 磁力链接配置白名单补丁 */
export interface MagnetPatch {
  metadataTimeoutSecs?: number
  downloadTimeoutSecs?: number
  enableDht?: boolean
  enableUpnp?: boolean
  trackers?: string[]
  disableDhtPersistence?: boolean
  peerWaitTimeoutSecs?: number
  socksProxyUrl?: string | null
  peerConnectTimeoutSecs?: number
  peerReadWriteTimeoutSecs?: number
  forceTrackerIntervalSecs?: number
  deferWritesUpToMb?: number
  disableDhtWhenSocks?: boolean
  peerAddrs?: string[]
}

/** 调度器配置白名单补丁 */
export interface SchedulerPatch {
  minFragmentSize?: number
  maxFragmentSize?: number
  ewmaAlpha?: number
}

/** HuggingFace Hub 配置白名单补丁 */
export interface HubPatch {
  sourceMode?: HfSourceMode
}

export type SnifferResourceType = 'video' | 'audio' | 'document' | 'archive' | 'executable' | 'image' | 'model' | 'other'

export interface SnifferResource {
  id: string
  url: string
  /** 资源原始 URL(含凭据,用于实际下载) */
  downloadUrl: string
  name: string
  type: SnifferResourceType
  size: number | null
  contentType?: string
  discoveredAt: number
  sourcePage?: string
}

/** 嗅探捕获配置(与后端 CaptureConfig 对齐,camelCase) */
export interface CaptureConfig {
  /** 启用的资源类型(小写字符串集合) */
  enabledTypes: SnifferResourceType[]
  /** 最小文件大小(字节),低于此值不捕获 */
  minSize: number
  /** URL 过滤关键词 */
  urlFilters: string[]
}

/** 剪贴板 URL 检测事件 payload(与后端 ClipboardUrlDetected 对齐,camelCase) */
export interface ClipboardUrlDetected {
  url: string
  resourceType: string
}

export interface ProgressPayload {
  id: string
  progress: number
  downloaded: number
  speed: number
  status: DownloadStatus
  fragmentsDone: number
  fragmentsTotal: number
  activeConcurrency: number
  /** 文件总大小,探测完成后由后端通过进度事件同步,避免详情页显示 0B */
  fileSize?: number | null
  completedDelta?: number[]
  /** 本周期新开始下载的分片索引增量(后端 Started 事件累积) */
  startedDelta?: number[]
  /** 任务失败原因。Failed 终态时由后端写入,UI 无需等待 get_task_list 全量刷新即可展示错误详情(P1-22-4) */
  errorReason?: string | null
}

export type ProgressEvent = Record<string, ProgressPayload>

/** get_task_fragments 返回:真实分片总数 + 已完成分片索引 + 正在下载分片索引 */
export interface TaskFragmentsView {
  total: number
  doneIndices: number[]
  downloadingIndices: number[]
}

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
  closing?: boolean
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
  /**
   * 当前活跃并发分片数。
   * 后端 #[serde(default)] active_concurrency: u32,旧版快照可能缺省(0)。
   * 与 ProgressPayload.activeConcurrency 对齐。
   */
  activeConcurrency: number
}

/** 应用信息 -- 与后端 AppInfo 对齐 */
export interface AppInfo {
  version: string
  name: string
}

/** HF 模型元数据 — 与后端 HfModelInfo 对齐 */
export interface HfModelInfo {
  id: string
  author?: string
  sha: string
  lastModified: string
  tags: string[]
  pipelineTag?: string
  libraryName?: string
  license?: string
  downloads: number
  likes: number
  siblings?: HubFileInfo[]
  cardData?: HfCardData
}

/** Model card 摘要 — 与后端 HfCardData 对齐 */
export interface HfCardData {
  description?: string
  language: string[]
  datasets: string[]
}

/** 文件类型分类 — 与后端 FileCategory 对齐 */
export type FileCategory = 'modelWeight' | 'config' | 'tokenizer' | 'code' | 'data' | 'document' | 'other'

/** HF 任务元数据 — 与后端 HfTaskMeta 对齐 */
export interface HfTaskMeta {
  repoId: string
  revision: string
  filePath: string
  lfsOid?: string
}

/** 本地模型记录 — 与后端 LocalModel 对齐 */
export interface LocalModel {
  repoId: string
  revision: string
  localPath: string
  files: LocalModelFile[]
  totalSize: number
  downloadedAt?: string
  metadata?: HfModelInfo
}

/** 本地模型文件 — 与后端 LocalModelFile 对齐 */
export interface LocalModelFile {
  path: string
  localPath: string
  size: number
  category: FileCategory
  lfsOid?: string
  verifyStatus: VerifyStatus
  exists: boolean
}

/** 校验状态 — 与后端 VerifyStatus 对齐 */
export type VerifyStatus = 'unverified' | 'verified' | { failed: string }

/** 文件校验结果 — 与后端 FileVerifyResult 对齐 */
export interface FileVerifyResult {
  path: string
  status: VerifyStatus
  elapsedMs: number
}

/** 收藏记录 — 与后端 ModelFavorite 对齐 */
export interface ModelFavorite {
  repoId: string
  addedAt: string
  cachedInfo?: HfModelInfo
}

/** 模型来源过滤器 */
export type ModelSourceFilter = 'local' | 'remote' | 'favorite'
