/**
 * Overlay 模式侧边栏
 *
 * 默认完全隐藏，鼠标移至左边缘 8px 触发区展开。
 * 展开时为 fixed 覆盖层（不推挤内容），Pin 后始终显示并推挤。
 */
import { Show, For, createSignal, createEffect } from 'solid-js'
import type { ViewName, DownloadFilter } from '../types'
import {
  $currentFilter,
  $filterCounts,
  $totalSpeed,
  setCurrentFilter,
} from '../stores/downloads'
import { historyRecords } from '../stores/history'
import { formatSpeed } from '../utils/format'
import { Icon } from '../utils/icons'
import { btnIcon, labelCaption } from '../utils/styles'

/* ---- 配置 ---- */
const STORAGE_KEY = 'aimd.sidebar.pinned'
const SIDEBAR_WIDTH = 220
const CLOSE_DELAY = 300

/* ---- 导航数据 ---- */
const FILTER_ITEMS: { filter: DownloadFilter; label: string; icon: string }[] =
  [
    { filter: 'all', label: '全部', icon: 'list-bullet' },
    { filter: 'downloading', label: '下载中', icon: 'arrow-down-tray' },
    { filter: 'completed', label: '已完成', icon: 'check-circle' },
    { filter: 'incomplete', label: '未完成', icon: 'clock' },
  ]

const TOOL_ITEMS: { view: ViewName; label: string; icon: string }[] = [
  { view: 'sniffer', label: '资源嗅探', icon: 'magnifying-glass' },
  { view: 'settings', label: '设置', icon: 'cog-6-tooth' },
]

const DATA_ITEMS: { view: ViewName; label: string; icon: string }[] = [
  { view: 'history', label: '历史', icon: 'clock' },
  { view: 'stats', label: '统计', icon: 'chart-bar' },
]

/* ---- 组件 ---- */
export default function Sidebar(props: {
  currentView: ViewName
  onViewChange: (view: ViewName) => void
}) {
  /* -- 状态 -- */
  const [hovered, setHovered] = createSignal(false)
  const initialPinned = (() => {
    try {
      return localStorage.getItem(STORAGE_KEY) === 'true'
    } catch {
      return false
    }
  })()
  const [pinned, setPinned] = createSignal(initialPinned)

  let closeTimer: ReturnType<typeof setTimeout> | undefined

  const sidebarVisible = () => pinned() || hovered()

  createEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, String(pinned()))
    } catch {
      /* ignore */
    }
  })

  /* -- 计时器 -- */
  function cancelClose() {
    if (closeTimer !== undefined) {
      clearTimeout(closeTimer)
      closeTimer = undefined
    }
  }

  function startClose() {
    cancelClose()
    closeTimer = setTimeout(() => {
      setHovered(false)
      closeTimer = undefined
    }, CLOSE_DELAY)
  }

  function handleMouseEnter() {
    cancelClose()
    setHovered(true)
  }

  function handleMouseLeave() {
    if (!pinned()) startClose()
  }

  function handleBackdropClick() {
    if (!pinned()) {
      setHovered(false)
      cancelClose()
    }
  }

  function togglePin(e: Event) {
    e.stopPropagation()
    setPinned((prev) => !prev)
  }

  /* -- 导航操作 -- */
  function handleFilterClick(filter: DownloadFilter) {
    setCurrentFilter(filter)
    props.onViewChange('downloads')
    if (!pinned()) {
      setHovered(false)
      cancelClose()
    }
  }

  function handleNavClick(view: ViewName) {
    props.onViewChange(view)
    if (!pinned()) {
      setHovered(false)
      cancelClose()
    }
  }

  const counts = () => $filterCounts.get()
  const historyCount = () => historyRecords.length

  /* -- 渲染 -- */
  return (
    <>
      {/* 8px 隐形触发区 */}
      <div
        class="fixed left-0 w-2 z-30"
        style={{ top: '36px', height: 'calc(100dvh - 36px)' }}
        onMouseEnter={handleMouseEnter}
        onMouseLeave={handleMouseLeave}
      />

      {/* Pin 模式占位（推挤内容区） */}
      <Show when={pinned()}>
        <div style={{ width: `${SIDEBAR_WIDTH}px`, 'flex-shrink': '0' }} />
      </Show>

      {/* 遮罩层（仅 overlay 模式） */}
      <Show when={sidebarVisible() && !pinned()}>
        <div
          class="fixed inset-0 bg-black/50 z-40"
          style={{ top: '36px' }}
          onClick={handleBackdropClick}
        />
      </Show>

      {/* 侧边栏面板 */}
      <div
        class="fixed flex flex-col bg-surface backdrop-blur-xl border-r border-border overflow-hidden z-40"
        style={{
          top: '36px',
          left: '0',
          width: sidebarVisible() ? `${SIDEBAR_WIDTH}px` : '0',
          height: 'calc(100dvh - 36px)',
          'transition-property': 'width',
          'transition-duration': '250ms',
          'transition-timing-function': 'cubic-bezier(0.16, 1, 0.3, 1)',
        }}
        onMouseEnter={handleMouseEnter}
        onMouseLeave={handleMouseLeave}
      >
        {/* 标题栏 */}
        <div class="flex items-center justify-between px-3 h-9 border-b border-border shrink-0">
          <span class="text-[12px] font-bold text-accent tracking-tight whitespace-nowrap overflow-hidden">
            Tachyon
          </span>
          <button
            class={`${btnIcon} w-5 h-5 shrink-0`}
            onClick={togglePin}
            aria-label={pinned() ? '取消固定侧边栏' : '固定侧边栏'}
            title={pinned() ? '取消固定' : '固定侧边栏'}
          >
            <Icon
              name={pinned() ? 'pause-circle' : 'plus'}
              class="w-3.5 h-3.5"
            />
          </button>
        </div>

        {/* 导航区域 */}
        <div class="flex-1 overflow-y-auto py-1">
          {/* 下载过滤 */}
          <div class={`px-3 py-1 ${labelCaption}`}>下载</div>
          <For each={FILTER_ITEMS}>
            {(item) => (
              <button
                class={`relative w-full flex items-center gap-2.5 px-3 py-1.5 text-[12px] transition-all duration-150 ${
                  props.currentView === 'downloads' &&
                  $currentFilter.get() === item.filter
                    ? 'text-accent bg-accent-muted'
                    : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary hover:translate-x-[1px]'
                }`}
                onClick={() => handleFilterClick(item.filter)}
                aria-pressed={
                  props.currentView === 'downloads' &&
                  $currentFilter.get() === item.filter
                }
              >
                <Show
                  when={
                    props.currentView === 'downloads' &&
                    $currentFilter.get() === item.filter
                  }
                >
                  <div class="absolute left-0 top-1/2 -translate-y-1/2 w-[2px] h-4 bg-accent rounded-full" />
                </Show>
                <Icon name={item.icon} class="w-4 h-4 shrink-0" />
                <span class="flex-1 text-left whitespace-nowrap">
                  {item.label}
                </span>
                <Show when={counts()[item.filter] !== undefined}>
                  <span class="text-[10px] font-mono text-text-tertiary">
                    {counts()[item.filter]}
                  </span>
                </Show>
              </button>
            )}
          </For>

          <div class="mx-2 my-1.5 h-px bg-border" />

          {/* 工具 */}
          <div class={`px-3 py-1 ${labelCaption}`}>工具</div>
          <For each={TOOL_ITEMS}>
            {(item) => (
              <button
                class={`relative w-full flex items-center gap-2.5 px-3 py-1.5 text-[12px] transition-all duration-150 ${
                  props.currentView === item.view
                    ? 'text-accent bg-accent-muted'
                    : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary hover:translate-x-[1px]'
                }`}
                onClick={() => handleNavClick(item.view)}
                aria-pressed={props.currentView === item.view}
              >
                <Show when={props.currentView === item.view}>
                  <div class="absolute left-0 top-1/2 -translate-y-1/2 w-[2px] h-4 bg-accent rounded-full" />
                </Show>
                <Icon name={item.icon} class="w-4 h-4 shrink-0" />
                <span class="flex-1 text-left whitespace-nowrap">
                  {item.label}
                </span>
              </button>
            )}
          </For>

          <div class="mx-2 my-1.5 h-px bg-border" />

          {/* 数据 */}
          <div class={`px-3 py-1 ${labelCaption}`}>数据</div>
          <For each={DATA_ITEMS}>
            {(item) => (
              <button
                class={`relative w-full flex items-center gap-2.5 px-3 py-1.5 text-[12px] transition-all duration-150 ${
                  props.currentView === item.view
                    ? 'text-accent bg-accent-muted'
                    : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary hover:translate-x-[1px]'
                }`}
                onClick={() => handleNavClick(item.view)}
                aria-pressed={props.currentView === item.view}
              >
                <Show when={props.currentView === item.view}>
                  <div class="absolute left-0 top-1/2 -translate-y-1/2 w-[2px] h-4 bg-accent rounded-full" />
                </Show>
                <Icon name={item.icon} class="w-4 h-4 shrink-0" />
                <span class="flex-1 text-left whitespace-nowrap">
                  {item.label}
                </span>
                <Show when={item.view === 'history' && historyCount() > 0}>
                  <span class="text-[10px] font-mono text-text-tertiary">
                    {historyCount()}
                  </span>
                </Show>
              </button>
            )}
          </For>
        </div>

        {/* 底部总速度 */}
        <div class="px-3 py-2 border-t border-border shrink-0">
          <div class="flex items-center justify-between text-[10px]">
            <span class="text-text-tertiary">总速度</span>
            <span class="font-mono text-accent">
              {formatSpeed($totalSpeed.get())}
            </span>
          </div>
        </div>
      </div>
    </>
  )
}
