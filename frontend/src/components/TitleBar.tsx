import { createSignal, onCleanup, onMount, Show, For } from 'solid-js'
import { LightningIcon, MinimizeIcon, MaximizeIcon, RestoreIcon, CloseIcon, MenuIcon } from './icons'
import { tr, type MessageKey } from '../i18n'
import { $ui, openSettingsTab } from '../stores/ui'

type AppWindow = {
  minimize: () => Promise<void>
  toggleMaximize: () => Promise<void>
  close: () => Promise<void>
  isMaximized: () => Promise<boolean>
  onResized: (handler: () => void | Promise<void>) => Promise<() => void>
}

interface MenuItem {
  id: string
  labelKey: MessageKey
  action: () => void
  separatorAfter?: boolean
}

export default function TitleBar() {
  const [isMaximized, setIsMaximized] = createSignal(false)
  const [menuOpen, setMenuOpen] = createSignal(false)
  let appWindow: AppWindow | undefined
  let unlistenResize: (() => void) | undefined
  let menuRef: HTMLDivElement | undefined
  let triggerRef: HTMLButtonElement | undefined

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

  // 应用菜单项(spec 7.2):设置 / 快捷键 / 关于 / 退出
  const menuItems: MenuItem[] = [
    { id: 'settings', labelKey: 'titleBar.menu.settings', action: () => $ui.openSettings(), separatorAfter: true },
    { id: 'shortcuts', labelKey: 'titleBar.menu.shortcuts', action: () => $ui.openShortcutHelp(), separatorAfter: true },
    { id: 'about', labelKey: 'titleBar.menu.about', action: () => openSettingsTab('about'), separatorAfter: true },
    { id: 'quit', labelKey: 'titleBar.menu.quit', action: () => void handleClose() },
  ]

  // 点击外部/Esc 关闭菜单
  const handleDocPointerDown = (e: PointerEvent) => {
    if (!menuOpen()) return
    const target = e.target as Node | null
    if (menuRef && target && menuRef.contains(target)) return
    if (triggerRef && target && triggerRef.contains(target)) return
    setMenuOpen(false)
  }
  const handleDocKeyDown = (e: KeyboardEvent) => {
    if (menuOpen() && e.key === 'Escape') setMenuOpen(false)
  }
  onMount(() => {
    document.addEventListener('pointerdown', handleDocPointerDown)
    document.addEventListener('keydown', handleDocKeyDown)
  })
  onCleanup(() => {
    document.removeEventListener('pointerdown', handleDocPointerDown)
    document.removeEventListener('keydown', handleDocKeyDown)
  })

  const runMenuItem = (item: MenuItem) => {
    setMenuOpen(false)
    item.action()
  }

  return (
    <div
      class="flex items-center justify-between select-none relative z-50"
      style={{
        height: '36px',
        background: 'var(--color-bg-primary)',
        'border-bottom': '1px solid var(--color-border-subtle)',
      }}
      data-tauri-drag-region
    >
      {/* Brand + 应用菜单 */}
      <div class="flex items-center" style={{ height: '100%' }}>
        <div
          class="flex items-center gap-2"
          style={{ padding: "0 12px", height: "100%" }}
        >
          {/* brand 材质方块:深底 + 电青闪电(对齐参考稿 sidebar brand)。
              深底 surface-2 + 闪电 fill accent,暗色下高对比可辨。 */}
          <div
            class="flex items-center justify-center"
            style={{
              width: "22px",
              height: "22px",
              "border-radius": "6px",
              background: "var(--color-bg-raised)",
              color: "var(--color-accent-primary)",
              "box-shadow": "var(--shadow-inset-bevel)",
            }}
          >
            <LightningIcon />
          </div>
          <span
            style={{
              "font-family": "'Geist', sans-serif",
              "font-size": "13px",
              "font-weight": 600,
              color: "var(--color-text-title)",
              "letter-spacing": "-0.01em",
            }}
          >
            Tachyon
          </span>
        </div>

        {/* 应用菜单按钮(≡) + 下拉(spec 7.2) */}
        <div style={{ position: 'relative', height: '100%' }}>
          <button
            ref={triggerRef}
            class="win-btn"
            style={{ height: '100%' }}
            aria-label={tr('titleBar.menu')}
            aria-haspopup="menu"
            aria-expanded={menuOpen()}
            title={tr('titleBar.menu')}
            onClick={() => setMenuOpen((v) => !v)}
          >
            <MenuIcon />
          </button>
          <Show when={menuOpen()}>
            <div
              ref={menuRef}
              role="menu"
              aria-orientation="vertical"
              class="detail-menu"
              style={{
                left: '0',
                top: '100%',
                'min-width': '180px',
                'margin-top': '0',
              }}
            >
              <For each={menuItems}>
                {(item) => (
                  <button
                    role="menuitem"
                    class="detail-menu-item"
                    style={{ 'font-size': '13px' }}
                    onClick={() => runMenuItem(item)}
                  >
                    {tr(item.labelKey)}
                  </button>
                )}
              </For>
            </div>
          </Show>
        </div>
      </div>

      {/* Drag region */}
      <div class="flex-1 h-full" data-tauri-drag-region />

      {/* Window controls */}
      <div class="flex items-center">
        <button
          class="win-btn"
          onClick={handleMinimize}
          aria-label={tr("titleBar.aria.minimize")}
          title={tr("titleBar.minimize")}
        >
          <MinimizeIcon />
        </button>
        <button
          class="win-btn"
          onClick={handleMaximize}
          aria-label={isMaximized() ? tr("titleBar.aria.restore") : tr("titleBar.aria.maximize")}
          title={isMaximized() ? tr("titleBar.restore") : tr("titleBar.maximize")}
        >
          {isMaximized() ? <RestoreIcon /> : <MaximizeIcon />}
        </button>
        <button
          class="win-btn win-btn-close"
          onClick={handleClose}
          aria-label={tr("titleBar.aria.close")}
          title={tr("titleBar.close")}
        >
          <CloseIcon />
        </button>
      </div>

      {/* 去 AI 味:原 violet→cyan 渐变 glow 线删除,subtle border 已足够分隔 */}
    </div>
  )
}
