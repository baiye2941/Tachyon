import type { TaskInfo, AppConfig, ConfigPatch, SnifferResource, HubFileInfo, DownloadProgress, AppInfo, HfModelInfo, LocalModel, FileVerifyResult, ModelFavorite, TaskFragmentsView, CaptureConfig, BtProxyCoverageReport } from '../types'
import { confirmDestructive, getRiskTier } from '../utils/commandRisk'
import { tr } from '../i18n'
import { isBrowserDev, removeMockTask } from '../stores/mockData'
import type { RecoveryWarningPayload } from './events'

async function getInvoke(): Promise<typeof import('@tauri-apps/api/core').invoke> {
  try {
    const mod = await import('@tauri-apps/api/core')
    return mod.invoke
  } catch {
    throw new Error(tr('toast.tauriUnavailable'))
  }
}

/**
 * 请求一次性确认令牌(P1-11b)
 *
 * 前端在用户确认破坏性操作后调用,获取后端生成的一次性 token。
 * 该 token 需传入破坏性命令(delete_task/update_config)的
 * confirmationToken 参数,后端验证 token 有效且未过期后才执行操作。
 */
async function requestConfirmation(action: string): Promise<string> {
  const fn = await getInvoke()
  return fn<string>('request_confirmation', { action })
}

/**
 * 通用 invoke 包装器,带破坏性命令确认门(P1-11a)与后端 confirmation token(P1-11b)
 *
 * 对 destructive 级命令(如 delete_task/update_config):
 * 1. 弹出用户确认(P1-11a,前端纵深防御) — 除非 skipConfirm 为 true
 * 2. 用户确认后,调用 request_confirmation 获取后端一次性 token(P1-11b)
 * 3. 将 token 作为 confirmationToken 参数传入破坏性命令
 *
 * 后端验证 token 一次性+60秒过期,防止 XSS/供应链注入后静默调用。
 * safe/mutate 级命令直接放行,无需确认。
 *
 * @param skipConfirm - 跳过前端 window.confirm 对话框(调用方已自行确认)
 */
async function invoke<T>(cmd: string, args?: Record<string, unknown>, skipConfirm?: boolean): Promise<T> {
  if (!skipConfirm) {
    const confirmed = await confirmDestructive(cmd)
    if (!confirmed) {
      throw new Error(tr('toast.userCancelled', { cmd }))
    }
  }

  // P1-11b: 破坏性命令需附带后端确认令牌
  if (getRiskTier(cmd) === 'destructive') {
    const token = await requestConfirmation(cmd)
    const argsWithToken = { ...args, confirmationToken: token }
    const fn = await getInvoke()
    return fn(cmd, argsWithToken) as Promise<T>
  }

  const fn = await getInvoke()
  return fn(cmd, args) as Promise<T>
}

/**
 * 判断路径是否为合法本地文件系统路径(非带 scheme 的 URL)。
 *
 * 保留作为工具函数供其他模块复用;P1-21 后打开文件夹改由后端
 * `open_task_folder` 命令处理(后端校验路径在下载根目录内),
 * 前端不再直接调用 shell.open,该函数仅保留路径合法性校验语义。
 */
export function isLocalPath(path: string): boolean {
  // 拒绝任何 scheme 前缀(如 javascript:/http:/https:/file:/ftp:)。
  // 规则:形如 `scheme:` 且冒号后非反斜杠则视为 scheme 而拒绝。
  // Windows 盘符 `C:\` 冒号后是反斜杠,不匹配此规则,故放行。
  // Unix 绝对路径 `/home/x`、UNC `\\server`、相对路径均无 `scheme:` 前缀,放行。
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:(?!\\)/.test(path)) {
    return false
  }
  return true
}

export const api = {
  /** 获取应用信息(版本号、名称) */
  getAppInfo: () => invoke<AppInfo>('get_app_info'),
  /** 获取支持的协议列表 */
  getSupportedProtocols: () => invoke<string[]>('supported_protocols'),
  /** 拉取启动恢复告警(P1-22-3):setup 阶段 emit 的事件前端会漏接,主动拉取补全 */
  getRecoveryWarning: () => invoke<RecoveryWarningPayload | null>('get_recovery_warning'),
  /** 创建下载任务 */
  createTask: (url: string, downloadDir?: string, mirrorUrls?: string[], fileName?: string, autoStart?: boolean) =>
    invoke<string>('create_task', { url, downloadDir, mirrorUrls, fileName, autoStart }),
  /** 探测真实文件名(HEAD 请求获取 Content-Disposition / DHT 查询种子元数据) */
  probeFilename: (url: string) => invoke<string>('probe_filename', { url }),
  /** 获取任务列表 */
  getTaskList: () => invoke<TaskInfo[]>('get_task_list'),
  /** 获取任务详情 */
  getTaskDetail: (taskId: string) => invoke<TaskInfo>('get_task_detail', { taskId }),
  /** 设置任务标签(全量替换) */
  setTaskTags: (taskId: string, tags: string[]) => invoke<void>('set_task_tags', { taskId, tags }),
  /** 添加单个任务标签 */
  addTaskTag: (taskId: string, tag: string) => invoke<void>('add_task_tag', { taskId, tag }),
  /** 移除单个任务标签 */
  removeTaskTag: (taskId: string, tag: string) => invoke<void>('remove_task_tag', { taskId, tag }),
  /** 暂停任务 */
  pauseTask: (taskId: string) => invoke<void>('pause_task', { taskId }),
  /** 恢复任务 */
  resumeTask: (taskId: string) => invoke<void>('resume_task', { taskId }),
  /** 取消任务 */
  cancelTask: (taskId: string) => invoke<void>('cancel_task', { taskId }),
  /**
   * 删除任务(破坏性操作,需确认令牌)
   *
   * @param opts.skipConfirm - 调用方已在应用层 ConfirmDialog 确认过,
   *   跳过 invoke 内置的 window.confirm。后端 confirmation token 仍会请求,
   *   安全边界不受影响。
   */
  deleteTask: (taskId: string, opts?: { skipConfirm?: boolean; deleteLocalFile?: boolean }) => {
    if (isBrowserDev()) {
      removeMockTask(taskId)
      return Promise.resolve()
    }
    return invoke<void>('delete_task', { taskId, deleteLocalFile: opts?.deleteLocalFile }, opts?.skipConfirm)
  },
  /** 撤销取消任务(破坏性操作,需确认令牌) */
  undoCancelTask: (taskId: string) => invoke<void>('undo_cancel_task', { taskId }),
  /** 撤销删除任务(破坏性操作,需确认令牌) */
  undoDeleteTask: (taskId: string) => invoke<void>('undo_delete_task', { taskId }),
  /** 重排任务顺序 */
  reorderTasks: (orderedIds: string[]) => invoke<void>('reorder_tasks', { orderedIds }),
  /** 将任务移动到指定任务之前,beforeId 为空时移到末尾 */
  moveTask: (taskId: string, beforeId?: string) => invoke<void>('move_task', { taskId, beforeId }),
  /** 打开任务文件所在文件夹(后端校验路径在下载根目录内,P1-21) */
  openFolder: (taskId: string) => invoke<void>('open_task_folder', { taskId }),
  /** 打开指定路径所在文件夹(历史记录/本地模型等不在任务仓库中的场景,后端校验路径在下载根目录内,P1-21) */
  openFolderUnderRoot: (path: string) => invoke<void>('open_folder_under_download_root', { path }),
  /** 获取下载进度详情 */
  getDownloadProgress: (taskId: string) => invoke<DownloadProgress>('get_download_progress', { taskId }),
  /** 订阅进度更新(通过 Tauri 事件推送) */
  subscribeProgress: () => invoke<void>('subscribe_progress'),
  /** 获取任务分片状态(DetailPanel 打开时调用) */
  getTaskFragments: (taskId: string) => invoke<TaskFragmentsView>('get_task_fragments', { taskId }),
  /** 获取应用配置 */
  getConfig: () => invoke<AppConfig>('get_config'),
  /** FIX-16:获取 BT 各流量类别的代理覆盖状态(隐私可见性) */
  getBtProxyCoverage: () => invoke<BtProxyCoverageReport | null>('get_bt_proxy_coverage'),
  /** 更新应用配置(破坏性操作,需确认令牌。SettingsPanel 已自行确认,跳过 invoke 内 window.confirm) */
  updateConfig: (patch: ConfigPatch) => invoke<void>('update_config', { patch }, true),
  /** 获取嗅探资源列表 */
  getSnifferResources: () => invoke<SnifferResource[]>('get_sniffer_resources'),
  /** 添加嗅探过滤规则 */
  addSnifferFilter: (filter: string) => invoke<void>('add_sniffer_filter', { filter }),
  /** 手动添加嗅探资源 URL */
  addSnifferResource: (url: string) => invoke<void>('add_sniffer_resource', { url }),
  /** 清空所有嗅探资源 */
  clearSnifferResources: () => invoke<void>('clear_sniffer_resources'),
  /** 获取嗅探捕获配置 */
  getSnifferCaptureConfig: () => invoke<CaptureConfig>('get_sniffer_capture_config'),
  /** 更新嗅探捕获配置 */
  setSnifferCaptureConfig: (config: CaptureConfig) => invoke<void>('set_sniffer_capture_config', { config }),
  /** 列出 HuggingFace 仓库文件 */
  listRepoFiles: (repoId: string, revision?: string) => invoke<HubFileInfo[]>('list_repo_files', { repoId, revision }),
  /** 获取 HuggingFace 文件下载 URL */
  getHfDownloadUrl: (repoId: string, path: string, revision?: string) => invoke<string>('get_hf_download_url', { repoId, filePath: path, revision }),
  /** 获取 HF 模型元数据 */
  getModelInfo: (repoId: string, revision?: string) => invoke<HfModelInfo>('get_model_info', { repoId, revision }),
  /** 搜索 HF 模型 */
  searchModels: (query: string, limit?: number) => invoke<HfModelInfo[]>('search_models', { query, limit }),
  /** 扫描本地模型 */
  scanLocalModels: () => invoke<LocalModel[]>('scan_local_models'),
  /** 校验模型文件完整性 */
  verifyModel: (repoId: string, revision?: string) => invoke<FileVerifyResult[]>('verify_model', { repoId, revision }),
  /** 列出收藏 */
  listModelFavorites: () => invoke<ModelFavorite[]>('list_model_favorites'),
  /** 添加收藏 */
  addModelFavorite: (repoId: string) => invoke<void>('add_model_favorite', { repoId }),
  /** 移除收藏 */
  removeModelFavorite: (repoId: string) => invoke<void>('remove_model_favorite', { repoId }),
  /** 批量创建 HF 下载任务 */
  batchCreateHfTasks: (repoId: string, filePaths: string[], revision?: string, downloadDir?: string) =>
    invoke<string[]>('batch_create_hf_tasks', { repoId, revision, filePaths, downloadDir }),
  /** 导出配置与任务快照到 JSON 备份文件(破坏性操作,需确认令牌) */
  exportBackup: (path: string) =>
    invoke<void>('export_backup', { path }, true),
  /** 从 JSON 备份文件导入配置与任务快照(破坏性操作,需确认令牌) */
  importBackup: (path: string, overwrite: boolean) =>
    invoke<number>('import_backup', { path, overwrite }, true),
}
