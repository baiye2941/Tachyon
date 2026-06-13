import { createSignal, Show, For } from 'solid-js'
import { Icon } from '../utils/icons'
import type { ViewName } from '../types'

interface CommandItem {
  id: string
  label: string
  hint?: string
  group: 'navigation' | 'action'
  icon: string
  action: () => void
}

interface CommandPaletteProps {
  open: boolean
  onClose: () => void
  onViewChange: (view: ViewName) => void
  onNewDownload?: () => void
  onPauseAll?: () => void
  onResumeAll?: () => void
}

export default function CommandPalette(props: CommandPaletteProps) {
  let inputRef: HTMLInputElement | undefined
  let listRef: HTMLDivElement | undefined
  const [query, setQuery] = createSignal('')
  const [activeIndex, setActiveIndex] = createSignal(0)

  // 构建命令列表
  function getCommands(): CommandItem[] {
    return [
      // 导航
      { id: 'nav-downloads', label: '下载管理', hint: '查看所有下载任务', group: 'navigation', icon: 'list-bullet', action: () => { props.onViewChange('downloads'); props.onClose() } },
      { id: 'nav-sniffer', label: '资源嗅探', hint: '嗅探网页中的可下载资源', group: 'navigation', icon: 'magnifying-glass', action: () => { props.onViewChange('sniffer'); props.onClose() } },
      { id: 'nav-settings', label: '设置', hint: '应用配置与偏好', group: 'navigation', icon: 'cog-6-tooth', action: () => { props.onViewChange('settings'); props.onClose() } },
      { id: 'nav-history', label: '历史', hint: '下载历史记录', group: 'navigation', icon: 'clock', action: () => { props.onViewChange('history'); props.onClose() } },
      { id: 'nav-stats', label: '统计', hint: '下载速度与数据统计', group: 'navigation', icon: 'chart-bar', action: () => { props.onViewChange('stats'); props.onClose() } },
      // 操作
      { id: 'act-new', label: '新建下载', hint: '添加新的下载任务', group: 'action', icon: 'plus', action: () => { props.onNewDownload?.(); props.onClose() } },
      { id: 'act-pause-all', label: '全部暂停', hint: '暂停所有进行中的下载', group: 'action', icon: 'pause-circle', action: () => { props.onPauseAll?.(); props.onClose() } },
      { id: 'act-resume-all', label: '全部恢复', hint: '恢复所有已暂停的下载', group: 'action', icon: 'play', action: () => { props.onResumeAll?.(); props.onClose() } },
    ]
  }

  const filtered = () => {
    const q = query().toLowerCase().trim()
    const cmds = getCommands()
    if (!q) return cmds
    return cmds.filter(c =>
      c.label.toLowerCase().includes(q) || c.hint?.toLowerCase().includes(q)
    )
  }

  const groupedItems = () => {
    const items = filtered()
    const nav = items.filter(c => c.group === 'navigation')
    const act = items.filter(c => c.group === 'action')
    return { nav, act, all: items }
  }

  // 执行当前选中项
  function executeActive() {
    const items = filtered()
    const idx = activeIndex()
    if (idx >= 0 && idx < items.length) {
      items[idx]?.action()
    }
  }

  // 滚动选中项到可视区域
  function scrollActiveIntoView() {
    const el = listRef?.querySelector(`[data-cmd-index="${activeIndex()}"]`)
    el?.scrollIntoView({ block: 'nearest' })
  }

  // 全局键盘处理
  function handleKeyDown(e: KeyboardEvent) {
    if (!props.open) return

    switch (e.key) {
      case 'Escape':
        e.preventDefault()
        props.onClose()
        break
      case 'ArrowDown':
        e.preventDefault()
        setActiveIndex(i => {
          const total = filtered().length
          return total === 0 ? 0 : (i + 1) % total
        })
        scrollActiveIntoView()
        break
      case 'ArrowUp':
        e.preventDefault()
        setActiveIndex(i => {
          const total = filtered().length
          return total === 0 ? 0 : (i - 1 + total) % total
        })
        scrollActiveIntoView()
        break
      case 'Enter':
        e.preventDefault()
        executeActive()
        break
    }
  }

  function handleOverlayClick(e: MouseEvent) {
    if (e.target === e.currentTarget) {
      props.onClose()
    }
  }

  return (
    <Show when={props.open}>
      <div
        class="fixed inset-0 z-[100] flex items-start justify-center pt-[15vh]"
        style={{ background: 'rgba(0, 0, 0, 0.5)', 'backdrop-filter': 'blur(4px)' }}
        onClick={handleOverlayClick}
        onKeyDown={handleKeyDown}
        ref={() => {
          // 打开时聚焦输入框
          requestAnimationFrame(() => inputRef?.focus())
        }}
      >
        <div
          class="glass-panel rounded-lg w-full max-w-lg shadow-elevation-3 flex flex-col max-h-[60vh] animate-slide-up"
          onClick={(e) => e.stopPropagation()}
        >
          {/* 搜索输入框 */}
          <div class="flex items-center gap-2.5 px-4 py-3 border-b border-border">
            <Icon name="magnifying-glass" class="w-4 h-4 text-text-tertiary shrink-0" />
            <input
              ref={inputRef}
              type="text"
              class="flex-1 bg-transparent text-[14px] text-text-primary placeholder:text-text-tertiary focus-visible:outline-2 focus-visible:outline-accent-primary focus-visible:outline-offset-2"
              placeholder="搜索命令或导航..."
              value={query()}
              onInput={(e) => {
                setQuery(e.currentTarget.value)
                setActiveIndex(0)
              }}
              autofocus
            />
            <kbd class="text-[10px] text-text-tertiary bg-surface-elevated rounded px-1.5 py-0.5 font-mono border border-border shrink-0">
              Esc
            </kbd>
          </div>

          {/* 结果列表 */}
          <div ref={listRef} class="overflow-y-auto py-1.5" role="listbox" aria-label="命令列表">
            <Show when={filtered().length === 0}>
              <div class="px-4 py-6 text-center text-[13px] text-text-tertiary">
                未找到匹配的命令
              </div>
            </Show>

            {/* 导航分组 */}
            <Show when={groupedItems().nav.length > 0}>
              <div class="px-3 py-1.5 text-[10px] font-semibold text-text-tertiary uppercase tracking-wider select-none">
                导航
              </div>
              <For each={groupedItems().nav}>
                {(item) => {
                  const idx = () => groupedItems().all.indexOf(item)
                  return (
                    <button
                      data-cmd-index={idx()}
                      class={`w-full flex items-center gap-3 px-3 py-2 text-left text-[13px] transition-colors duration-100 ${
                        activeIndex() === idx()
                          ? 'bg-accent/10 text-text-primary'
                          : 'text-text-secondary hover:bg-surface-hover'
                      }`}
                      onClick={() => item.action()}
                      onMouseEnter={() => setActiveIndex(idx())}
                      role="option"
                      aria-selected={activeIndex() === idx()}
                    >
                      <Icon name={item.icon} class={`w-4 h-4 shrink-0 ${
                        activeIndex() === idx() ? 'text-accent' : 'text-text-tertiary'
                      }`} />
                      <div class="flex-1 min-w-0">
                        <span class="block truncate">{item.label}</span>
                        <Show when={item.hint}>
                          <span class="block text-[11px] text-text-tertiary truncate">{item.hint}</span>
                        </Show>
                      </div>
                    </button>
                  )
                }}
              </For>
            </Show>

            {/* 操作分组 */}
            <Show when={groupedItems().act.length > 0}>
              <div class="px-3 py-1.5 mt-1 text-[10px] font-semibold text-text-tertiary uppercase tracking-wider select-none">
                操作
              </div>
              <For each={groupedItems().act}>
                {(item) => {
                  const idx = () => groupedItems().all.indexOf(item)
                  return (
                    <button
                      data-cmd-index={idx()}
                      class={`w-full flex items-center gap-3 px-3 py-2 text-left text-[13px] transition-colors duration-100 ${
                        activeIndex() === idx()
                          ? 'bg-accent/10 text-text-primary'
                          : 'text-text-secondary hover:bg-surface-hover'
                      }`}
                      onClick={() => item.action()}
                      onMouseEnter={() => setActiveIndex(idx())}
                      role="option"
                      aria-selected={activeIndex() === idx()}
                    >
                      <Icon name={item.icon} class={`w-4 h-4 shrink-0 ${
                        activeIndex() === idx() ? 'text-accent' : 'text-text-tertiary'
                      }`} />
                      <div class="flex-1 min-w-0">
                        <span class="block truncate">{item.label}</span>
                        <Show when={item.hint}>
                          <span class="block text-[11px] text-text-tertiary truncate">{item.hint}</span>
                        </Show>
                      </div>
                    </button>
                  )
                }}
              </For>
            </Show>
          </div>

          {/* 底部提示 */}
          <div class="flex items-center gap-4 px-4 py-2 border-t border-border text-[10px] text-text-tertiary">
            <span class="flex items-center gap-1">
              <kbd class="bg-surface-elevated rounded px-1 py-0.5 font-mono border border-border">↑</kbd>
              <kbd class="bg-surface-elevated rounded px-1 py-0.5 font-mono border border-border">↓</kbd>
              导航
            </span>
            <span class="flex items-center gap-1">
              <kbd class="bg-surface-elevated rounded px-1 py-0.5 font-mono border border-border">Enter</kbd>
              执行
            </span>
            <span class="flex items-center gap-1">
              <kbd class="bg-surface-elevated rounded px-1 py-0.5 font-mono border border-border">Esc</kbd>
              关闭
            </span>
          </div>
        </div>
      </div>
    </Show>
  )
}
