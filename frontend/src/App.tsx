import { createSignal, onMount, createEffect } from 'solid-js'
import type { ViewName } from './types'
import { api } from './api/invoke'
import { $tasks, refreshTaskList } from './stores/downloads'
import { useProgressListener } from './hooks/useTauriEvent'
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
    <Layout
      currentView={currentView()}
      onViewChange={setCurrentView}
      detail={currentView() === 'downloads' ? <DetailPanel /> : undefined}
    >
      {currentView() === 'downloads' && <TaskList />}
      {currentView() === 'settings' && <SettingsPanel />}
    </Layout>
  )
}
