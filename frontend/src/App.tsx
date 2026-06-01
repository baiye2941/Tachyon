import { createSignal, onMount, createEffect, For } from 'solid-js'
import type { ViewName } from './types'
import { api } from './api/invoke'
import { $tasks, refreshTaskList } from './stores/downloads'
import { useProgressListener } from './hooks/useTauriEvent'
import { toasts } from './stores/toast'
import Layout from './components/Layout'
import TaskList from './components/TaskList'
import DetailPanel from './components/DetailPanel'
import SettingsPanel from './components/SettingsPanel'

export default function App() {
  const [currentView, setCurrentView] = createSignal<ViewName>('downloads')

  useProgressListener()

  onMount(() => {
    refreshTaskList()
    api.subscribeProgress().catch(() => {})
  })

  let lastRefreshKey = ''
  createEffect(() => {
    const tasks = $tasks.get()
    const terminalIds = tasks
      .filter(t => ['completed', 'failed', 'cancelled'].includes(t.status))
      .map(t => t.id)
      .sort()
      .join(',')
    if (terminalIds && terminalIds !== lastRefreshKey) {
      lastRefreshKey = terminalIds
      refreshTaskList()
    }
  })

  return (
    <>
      <Layout
        currentView={currentView()}
        onViewChange={setCurrentView}
        detail={currentView() === 'downloads' ? <DetailPanel /> : undefined}
      >
        {currentView() === 'downloads' && <TaskList />}
        {currentView() === 'settings' && <SettingsPanel />}
      </Layout>
      <div class="fixed bottom-4 right-4 z-50 flex flex-col gap-2 max-w-sm">
        <For each={toasts}>
          {(toast) => (
            <div
              class={`px-4 py-2.5 rounded-lg text-[13px] font-medium shadow-lg backdrop-blur-sm animate-slide-up ${
                toast.type === 'error'
                  ? 'bg-error/90 text-white'
                  : toast.type === 'success'
                    ? 'bg-success/90 text-white'
                    : 'bg-surface/90 text-text-primary border border-white/10'
              }`}
            >
              {toast.message}
            </div>
          )}
        </For>
      </div>
    </>
  )
}
