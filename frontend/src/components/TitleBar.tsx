import { createSignal, onCleanup, onMount } from 'solid-js'
import { LogoIcon, MinimizeIcon, MaximizeIcon, RestoreIcon, CloseIcon } from './icons'

type AppWindow = {
  minimize: () => Promise<void>
  toggleMaximize: () => Promise<void>
  close: () => Promise<void>
  isMaximized: () => Promise<boolean>
  onResized: (handler: () => void | Promise<void>) => Promise<() => void>
}

interface TitleBarProps {
  onOpenSettings: () => void
}

export default function TitleBar(_props: TitleBarProps) {
  const [isMaximized, setIsMaximized] = createSignal(false)
  let appWindow: AppWindow | undefined
  let unlistenResize: (() => void) | undefined

  const syncMaximized = async () => {
    if (!appWindow) return

    try {
      setIsMaximized(await appWindow.isMaximized())
    } catch {
      // Tauri API 在浏览器环境中不可用，静默忽略
    }
  }

  onMount(async () => {
    try {
      const { getCurrentWebviewWindow } = await import('@tauri-apps/api/webviewWindow')
      appWindow = getCurrentWebviewWindow()

      await syncMaximized()
      unlistenResize = await appWindow.onResized(syncMaximized)
    } catch {
      // Tauri API 在浏览器环境中不可用，静默忽略
    }
  })

  onCleanup(() => {
    unlistenResize?.()
  })

  const handleMinimize = async () => {
    try {
      await appWindow?.minimize()
    } catch {
      // Tauri API 在浏览器环境中不可用，静默忽略
    }
  }

  const handleMaximize = async () => {
    try {
      await appWindow?.toggleMaximize()
      await syncMaximized()
    } catch {
      // Tauri API 在浏览器环境中不可用，静默忽略
    }
  }

  const handleClose = async () => {
    try {
      await appWindow?.close()
    } catch {
      // Tauri API 在浏览器环境中不可用，静默忽略
    }
  }

  return (
    <div
      class="flex items-center justify-between select-none relative z-50"
      style={{
        height: '36px',
        background: '#0A0A0F',
        'border-bottom': '1px solid rgba(255,255,255,0.05)',
      }}
      data-tauri-drag-region
    >
      {/* Brand */}
      <div
        class="flex items-center gap-2"
        style={{ padding: '0 12px', height: '100%' }}
      >
        <div
          class="flex items-center justify-center"
          style={{
            width: '18px',
            height: '18px',
            color: '#00D4AA',
            animation: 'logo-shimmer 3s ease-in-out infinite',
          }}
        >
          <LogoIcon />
        </div>
        <span
          style={{
            'font-family': "'Geist', sans-serif",
            'font-size': '13px',
            'font-weight': 500,
            color: '#F0F0F5',
            'letter-spacing': '0.5px',
          }}
        >
          Tachyon
        </span>
      </div>

      {/* Drag region */}
      <div class="flex-1 h-full" data-tauri-drag-region />

      {/* Window controls */}
      <div class="flex items-center">
        <button
          class="win-btn"
          onClick={handleMinimize}
          aria-label="最小化窗口"
          title="最小化"
        >
          <MinimizeIcon />
        </button>
        <button
          class="win-btn"
          onClick={handleMaximize}
          aria-label={isMaximized() ? '恢复窗口' : '最大化窗口'}
          title={isMaximized() ? '恢复' : '最大化'}
        >
          {isMaximized() ? <RestoreIcon /> : <MaximizeIcon />}
        </button>
        <button
          class="win-btn win-btn-close"
          onClick={handleClose}
          aria-label="关闭窗口"
          title="关闭"
        >
          <CloseIcon />
        </button>
      </div>

      {/* Bottom glow line */}
      <div
        class="absolute bottom-0 left-0 right-0 pointer-events-none"
        style={{
          height: '1px',
          background: 'linear-gradient(90deg, transparent 0%, rgba(0,212,170,0.1) 20%, rgba(0,180,216,0.1) 80%, transparent 100%)',
          opacity: 0.5,
        }}
      />
    </div>
  )
}
