/**
 * Tauri IPC 命令风险分层与破坏性命令确认门 (P1-11a + P1-11b)
 *
 * 背景：Tauri v2 的 capability/permission 体系不覆盖用户自定义的
 * `#[tauri::command]`,所有自定义命令默认对前端全量放行。XSS 或供应链
 * 注入后可静默调用全部命令。本模块提供**前端纵深防御层**(P1-11a):
 * 风险分层表 + window.confirm 确认门。当前全部已登记 destructive 命令
 * 均在调用方自带应用内确认(ConfirmDialog/原生文件对话框/撤销按钮手势)
 * 并以 skipConfirm 跳过弹门;window.confirm 仅作为**未登记新命令**
 * (默认 destructive,白名单原则)的兜底,防止漏登记时静默放行。
 *
 * 后端 confirmation token 机制(P1-11b)提供真正的安全边界:
 * 破坏性命令(delete_task/update_config 等)执行前需先通过
 * request_confirmation 获取一次性 token,再将 token 传入命令。
 * invoke 包装器对 destructive 级命令自动附加 token,与 skipConfirm 无关。
 * Token 一次性使用+60秒过期,防止 XSS/供应链注入后静默调用。
 */

import { tr } from '../i18n'

/** 命令风险等级 */
export type RiskTier = 'safe' | 'mutate' | 'destructive'

/**
 * 命令风险分层表
 *
 * - safe:只读查询,无副作用
 * - mutate:状态变更或触发网络/文件操作,但可恢复
 * - destructive:数据删除或安全策略变更,需用户二次确认
 *
 * 表必须覆盖 invoke.ts 中暴露的全部命令,新增命令时同步更新。
 */
export const COMMAND_RISK: Record<string, RiskTier> = {
  // 只读查询
  get_app_info: 'safe',
  supported_protocols: 'safe',
  get_task_list: 'safe',
  get_task_detail: 'safe',
  get_download_progress: 'safe',
  subscribe_progress: 'safe',
  get_task_fragments: 'safe',
  get_sniffer_resources: 'safe',
  get_config: 'safe',
  list_repo_files: 'safe',
  get_hf_download_url: 'safe',
  get_model_info: 'safe',
  search_models: 'safe',
  scan_local_models: 'safe',
  verify_model: 'safe',
  list_model_favorites: 'safe',
  request_confirmation: 'safe',
  // P1-21/P1-22-3: 后端校验型只读/打开命令,无数据变更
  open_task_folder: 'safe',
  open_folder_under_download_root: 'safe',
  get_recovery_warning: 'safe',
  // 只读查询补登:未登记会默认 destructive 误弹确认(连接 tab 曾因此闪确认框)
  get_quic_capability: 'safe',
  get_bt_proxy_coverage: 'safe',
  get_sniffer_capture_config: 'safe',
  // 状态变更 / 网络触发
  pause_task: 'mutate',
  resume_task: 'mutate',
  cancel_task: 'mutate',
  add_sniffer_filter: 'mutate',
  create_task: 'mutate',
  probe_filename: 'mutate',
  add_model_favorite: 'mutate',
  remove_model_favorite: 'mutate',
  batch_create_hf_tasks: 'mutate',
  // 状态变更补登:标签/排序/嗅探器操作(可恢复,非破坏性)
  add_task_tag: 'mutate',
  remove_task_tag: 'mutate',
  set_task_tags: 'mutate',
  move_task: 'mutate',
  reorder_tasks: 'mutate',
  add_sniffer_resource: 'mutate',
  clear_sniffer_resources: 'mutate',
  create_task_from_sniffer: 'mutate',
  set_sniffer_capture_config: 'mutate',
  // 破坏性:数据删除 / 安全策略变更 / 备份导入导出 / 撤销类恢复
  delete_task: 'destructive',
  undo_cancel_task: 'destructive',
  undo_delete_task: 'destructive',
  update_config: 'destructive',
  authorize_download_directory: 'destructive',
  export_backup: 'destructive',
  import_backup: 'destructive',
}

/** 破坏性命令的确认提示 i18n key */
const DESTRUCTIVE_CONFIRM_KEYS: Record<
  string,
  | 'confirm.destructive.deleteTask'
  | 'confirm.destructive.updateConfig'
  | 'confirm.destructive.exportBackup'
  | 'confirm.destructive.importBackup'
> = {
  delete_task: 'confirm.destructive.deleteTask',
  update_config: 'confirm.destructive.updateConfig',
  export_backup: 'confirm.destructive.exportBackup',
  import_backup: 'confirm.destructive.importBackup',
}

/** 获取命令风险等级,未登记的命令默认为 destructive(白名单原则) */
export function getRiskTier(command: string): RiskTier {
  return COMMAND_RISK[command] ?? 'destructive'
}

/**
 * 确认是否执行破坏性命令
 *
 * 对 safe/mutate 命令直接放行(resolve true);
 * 对 destructive 命令弹出确认提示,用户需主动确认。
 *
 * @returns true 表示放行,false 表示用户取消
 */
export async function confirmDestructive(command: string): Promise<boolean> {
  const tier = getRiskTier(command)
  if (tier !== 'destructive') return true

  const key = DESTRUCTIVE_CONFIRM_KEYS[command]
  const description = key ? tr(key) : tr('confirm.destructive.default')

  // 使用浏览器原生确认对话框:同步、可靠、在 Tauri WebView 中可用,
  // 且易于在测试中 mock(window.confirm)。
  // 不使用 toast 系统,因为 toast 的自动消失与 Promise 解析难以可靠关联。
  if (typeof window !== 'undefined' && typeof window.confirm === 'function') {
    return window.confirm(tr('confirm.destructive.prompt', { description }))
  }

  // 非浏览器环境(如 SSR)默认拒绝,安全优先
  return false
}
