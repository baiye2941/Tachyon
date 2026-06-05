import type { JSX } from 'solid-js'
import { Show, Switch, Match } from 'solid-js'
import Sidebar from './Sidebar'
import Topbar from './Topbar'
import HFZone from './HFZone'
import SpeedDashboard from './SpeedDashboard'
import TitleBar from './TitleBar'
import type { ViewName } from '../types'

interface LayoutProps {
  currentView: ViewName
  onViewChange: (v: ViewName) => void
  children: JSX.Element
  detail?: JSX.Element
  onOpenSearch?: () => void
  onPauseAll?: () => void
  onResumeAll?: () => void
}

export default function Layout(props: LayoutProps) {
  const isDownloadsView = () => props.currentView === 'downloads'

  return (
    <div class="min-h-[100dvh] h-[100dvh] bg-canvas text-text-primary flex flex-col">
      <TitleBar
        onPauseAll={props.onPauseAll}
        onOpenSearch={props.onOpenSearch}
      />
      <div class="flex-1 flex min-h-0">
        <Sidebar
          currentView={props.currentView}
          onViewChange={props.onViewChange}
        />

        <main class="relative min-w-0 flex-1 flex flex-col overflow-hidden">
          <Show when={isDownloadsView()}>
            <Topbar />
            <SpeedDashboard />
          </Show>

          <div class="flex-1 min-h-0 overflow-auto p-2 page-transition">
            <Switch>
              <Match when={isDownloadsView()}>
                <div class="flex min-h-0 h-full">
                  <div class="flex-1 min-w-0">
                    {props.children}
                  </div>
                  <Show when={props.detail}>
                    <aside class="w-[280px] shrink-0 border-l border-border pl-2">
                      {props.detail}
                    </aside>
                  </Show>
                </div>
              </Match>
              <Match when={props.currentView === 'sniffer'}>
                <HFZone />
              </Match>
              <Match when={true}>
                <div class="min-w-0">
                  {props.children}
                </div>
              </Match>
            </Switch>
          </div>
        </main>
      </div>
    </div>
  )
}
