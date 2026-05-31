import { For } from 'solid-js'
import type { ViewName } from '../types'
import { $totalSpeed } from '../stores/downloads'
import { formatSpeed } from '../utils/format'

const NAV_ITEMS: { view: ViewName; label: string; iconPath: string }[] = [
  { view: 'downloads', label: '下载列表', iconPath: 'M4 16l4.586-4.586a2 2 0 012.828 0L16 16m-2-2l1.586-1.586a2 2 0 012.828 0L20 14m-6-6h.01M6 20h12a2 2 0 002-2V6a2 2 0 00-2-2H6a2 2 0 00-2 2v12a2 2 0 002 2z' },
  { view: 'sniffer', label: '资源嗅探', iconPath: 'M9 20l-5.447-2.724A1 1 0 013 16.38V5.618a1 1 0 011.768-.384l2.694 2.986a1 1 0 001.632-.09l.733-1.104A1 1 0 0110 6.5V4a1 1 0 012 0v.5a1 1 0 001 1h1.5a1 1 0 011 1v4.35a4.028 4.028 0 01-1.106 2.874l-.637.631a4 4 0 01-5.322.276' },
  { view: 'settings', label: '设置', iconPath: 'M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.066 2.573c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.573 1.066c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.066-2.573c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.066z' },
]

export default function Sidebar(props: {
  currentView: ViewName
  onViewChange: (view: ViewName) => void
  hovered: boolean
  onHoverChange: (hovered: boolean) => void
}) {
  return (
    <div
      class="fixed top-0 left-0 bottom-0 z-30 overflow-hidden bg-surface backdrop-blur-md border-r border-white/[0.06] transition-[width] duration-300 ease-[cubic-bezier(0.34,1.56,0.64,1)"
      style={{ width: props.hovered ? '240px' : '48px' }}
      onMouseEnter={() => props.onHoverChange(true)}
      onMouseLeave={() => props.onHoverChange(false)}
    >
      <div
        class="h-full flex flex-col"
        style={{
          transform: props.hovered ? 'translate3d(0, 0, 0)' : 'translate3d(-192px, 0, 0)',
          transition: 'transform 0.3s cubic-bezier(0.34, 1.56, 0.64, 1)',
        }}
      >
        <div class="px-4 py-3 font-bold text-accent">AI Model Downloader</div>
        <nav class="flex-1 px-2 py-2 space-y-1">
          <For each={NAV_ITEMS}>
            {(item) => (
              <button
                class={`w-full flex items-center gap-3 px-3 py-2 rounded text-[13px] transition-colors duration-150 ${
                  props.currentView === item.view
                    ? 'text-accent bg-accent/12'
                    : 'text-text-secondary hover:bg-white/3'
                }`}
                onClick={() => props.onViewChange(item.view)}
              >
                <svg viewBox="0 0 24 24" class="w-5 h-5 shrink-0" fill="none" stroke="currentColor" stroke-width="1.5">
                  <path d={item.iconPath} />
                </svg>
                {props.hovered ? item.label : null}
              </button>
            )}
          </For>
        </nav>
        <div class="px-4 py-3 border-t border-white/6 text-xs text-text-tertiary">
          <div class="flex justify-between">
            <span>速度</span>
            <span class="font-mono">{formatSpeed($totalSpeed.get())}</span>
          </div>
        </div>
      </div>
    </div>
  )
}
