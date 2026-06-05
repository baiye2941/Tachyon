import { createSignal, onMount, Show } from 'solid-js'
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow'
import { Icon } from '../utils/icons'
import { btnIcon } from '../utils/styles'

function getPlatform(): 'macos' | 'windows' | 'linux' {
  const p = navigator.platform
  if (p.startsWith('Mac')) return 'macos'
  if (p.startsWith('Win')) return 'windows'
  return 'linux'
}

interface TitleBarProps {
  onNewDownload?: () => void
  onPauseAll?: () => void
  onOpenSearch?: () => void
}

export default function TitleBar(props: TitleBarProps) {
  const appWindow = getCurrentWebviewWindow()
  const [isMaximized, setIsMaximized] = createSignal(false)
  const [platform] = createSignal(getPlatform())

  onMount(async () => {
    try {
      setIsMaximized(await appWindow.isMaximized())
      const unlisten = await appWindow.onResized(async () => {
        setIsMaximized(await appWindow.isMaximized())
      })
      return () => {
        unlisten()
      }
    } catch {
      // Tauri API 在浏览器环境中不可用，静默忽略
    }
  })

  /** Windows/Linux 窗口控制按钮基础样式 */
  const winBtn =
    'flex items-center justify-center h-full px-3 text-text-secondary transition-colors duration-150 hover:bg-white/[0.06] active:bg-white/[0.08]'

  return (
    <div class="h-9 bg-surface border-b border-border flex items-center select-none shrink-0">
      {/* ---- 左侧 ---- */}
      <Show when={platform() === 'macos'}>
        {/* macOS: 交通灯按钮 */}
        <div class="flex items-center gap-2 px-3 shrink-0">
          <button
            onClick={() => appWindow.close()}
            class="w-3 h-3 rounded-full bg-[#ff5f57] hover:brightness-110 active:brightness-90 transition-all duration-150"
            aria-label="关闭窗口"
            title="关闭"
          />
          <button
            onClick={() => appWindow.minimize()}
            class="w-3 h-3 rounded-full bg-[#febc2e] hover:brightness-110 active:brightness-90 transition-all duration-150"
            aria-label="最小化窗口"
            title="最小化"
          />
          <button
            onClick={() => appWindow.toggleMaximize()}
            class="w-3 h-3 rounded-full bg-[#28c840] hover:brightness-110 active:brightness-90 transition-all duration-150"
            aria-label="最大化/还原窗口"
            title="最大化"
          />
        </div>
      </Show>

      <Show when={platform() !== 'macos'}>
        {/* Windows/Linux: 应用名称（可拖拽） */}
        <div
          class="flex items-center px-3 shrink-0 text-[12px] text-text-tertiary font-semibold tracking-wide select-none"
          data-tauri-drag-region
        >
          Tachyon
        </div>
      </Show>

      {/* ---- 中间: 搜索触发器（不可拖拽，占满剩余空间） ---- */}
      <div class="flex-1 flex justify-center px-2 min-w-0">
        <button
          onClick={() => props.onOpenSearch?.()}
          class="flex items-center gap-2 h-7 px-3 max-w-xs w-full rounded-md bg-surface-elevated border border-border text-text-tertiary hover:text-text-secondary hover:border-border-strong transition-colors duration-150"
          aria-label="打开搜索"
          title="搜索命令 (Ctrl+K)"
        >
          <Icon name="magnifying-glass" class="w-3.5 h-3.5 shrink-0" />
          <span class="text-[12px] flex-1 text-left truncate">搜索...</span>
          <kbd class="text-[10px] font-mono bg-canvas/50 rounded px-1 py-0.5 border border-border shrink-0">
            Ctrl+K
          </kbd>
        </button>
      </div>

      {/* ---- 右侧: 快捷操作 + 窗口控制 ---- */}
      <div class="flex items-center h-full shrink-0">
        {/* 快捷操作按钮 */}
        <button
          onClick={() => props.onNewDownload?.()}
          class={`${btnIcon} mx-0.5`}
          aria-label="新建下载"
          title="新建下载"
        >
          <Icon name="plus" class="w-4 h-4" />
        </button>
        <button
          onClick={() => props.onPauseAll?.()}
          class={`${btnIcon} mx-0.5`}
          aria-label="全部暂停"
          title="全部暂停"
        >
          <Icon name="pause-circle" class="w-4 h-4" />
        </button>

        {/* 分隔线 */}
        <Show when={platform() !== 'macos'}>
          <div class="w-px h-4 bg-border mx-1" />
        </Show>

        {/* 窗口控制 */}
        <Show when={platform() !== 'macos'}>
          <button
            onClick={() => appWindow.minimize()}
            class={winBtn}
            aria-label="最小化窗口"
            title="最小化"
          >
            <Icon name="minus" class="w-3.5 h-3.5" />
          </button>
          <button
            onClick={() => appWindow.toggleMaximize()}
            class={winBtn}
            aria-label="最大化/还原窗口"
            title={isMaximized() ? '还原' : '最大化'}
          >
            <Show
              when={isMaximized()}
              fallback={<Icon name="square" class="w-3.5 h-3.5" />}
            >
              <Icon name="window" class="w-3.5 h-3.5" />
            </Show>
          </button>
          <button
            onClick={() => appWindow.close()}
            class={`${winBtn} hover:bg-error/80 hover:text-white active:bg-error`}
            aria-label="关闭窗口"
            title="关闭"
          >
            <Icon name="x-mark" class="w-3.5 h-3.5" />
          </button>
        </Show>
      </div>
    </div>
  )
}
