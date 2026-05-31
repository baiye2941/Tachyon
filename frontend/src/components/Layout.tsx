import type { JSX } from 'solid-js'
import { Show, createSignal } from 'solid-js'
import Sidebar from './Sidebar'
import Topbar from './Topbar'
import HFZone from './HFZone'
import type { ViewName } from '../types'

interface LayoutProps {
  currentView: ViewName
  onViewChange: (v: ViewName) => void
  children: JSX.Element
  detail?: JSX.Element
}

export default function Layout(props: LayoutProps) {
  const [sidebarHovered, setSidebarHovered] = createSignal(false)
  const sidebarWidth = () => sidebarHovered() ? 240 : 48

  return (
    <div class="min-h-[100dvh] bg-canvas text-text-primary">
      <Sidebar
        currentView={props.currentView}
        onViewChange={props.onViewChange}
        hovered={sidebarHovered()}
        onHoverChange={setSidebarHovered}
      />
      <div
        class="transition-[margin-left] duration-300 ease-[cubic-bezier(0.34,1.56,0.64,1)]"
        style={{ 'margin-left': `${sidebarWidth()}px` }}
      >
        <Show when={props.currentView === 'downloads'}>
          <Topbar />
        </Show>
        <div class="p-4 overflow-auto" style={{ 'max-height': '100dvh' }}>
          <Show when={props.currentView === 'downloads'} fallback={
            <Show when={props.currentView === 'sniffer'} fallback={
              props.children
            }>
              <HFZone />
            </Show>
          }>
            <div class="flex gap-4">
              <div class="flex-1 min-w-0">
                {props.children}
              </div>
              <Show when={props.detail}>
                <div class="w-[260px] shrink-0 border-l border-white/6 pl-4">
                  {props.detail}
                </div>
              </Show>
            </div>
          </Show>
        </div>
      </div>
    </div>
  )
}
