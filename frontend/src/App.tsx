import { createSignal, onMount, createEffect, createMemo, For, ErrorBoundary, onCleanup } from 'solid-js'
import type { ViewName } from './types'
import { api } from './api/invoke'
import { $tasks, $totalSpeed, $activeCount, refreshTaskList } from './stores/downloads'
import { useProgressListener } from './hooks/useTauriEvent'
import { toasts } from './stores/toast'
import * as speedHistory from './stores/speedHistory'
import { historyRecords, getHistoryStats, clearHistory } from './stores/history'
import { $selectedIds, deselectAll } from './stores/selection'
import Layout from './components/Layout'
import TaskList from './components/TaskList'
import DetailPanel from './components/DetailPanel'
import SettingsPanel from './components/SettingsPanel'
import HistoryPanel from './components/HistoryPanel'
import StatsDashboard from './components/StatsDashboard'
import BatchToolbar from './components/BatchToolbar'
import CommandPalette from './components/CommandPalette'

function AppContent() {
  const [currentView, setCurrentView] = createSignal<ViewName>('downloads')
  const [searchOpen, setSearchOpen] = createSignal(false)

  useProgressListener()

  // Ctrl+K 全局搜索快捷键
  function handleGlobalKey(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key === 'k') {
      e.preventDefault()
      setSearchOpen(prev => !prev)
    }
  }

  onMount(() => {
    refreshTaskList()
    api.subscribeProgress().catch(() => {})
    window.addEventListener('keydown', handleGlobalKey)
  })
  onCleanup(() => window.removeEventListener('keydown', handleGlobalKey))

  // terminal-status effect 加 debounce 防连锁刷新
  let lastRefreshKey = ''
  let refreshTimer: ReturnType<typeof setTimeout> | undefined
  createEffect(() => {
    const tasks = $tasks.get()
    const terminalIds = tasks
      .filter(t => ['completed', 'failed', 'cancelled'].includes(t.status))
      .map(t => t.id)
      .sort()
      .join(',')
    if (terminalIds && terminalIds !== lastRefreshKey) {
      lastRefreshKey = terminalIds
      clearTimeout(refreshTimer)
      refreshTimer = setTimeout(() => refreshTaskList(), 300)
    }
  })
  onCleanup(() => clearTimeout(refreshTimer))

  // 直接使用 store 的 memo，不再重复遍历 tasks
  createEffect(() => {
    speedHistory.pushSpeed($totalSpeed.get())
    speedHistory.setActiveTasksCount($activeCount.get())
  })

  // 通用批量操作辅助函数
  async function batchAction(
    apiMethod: (id: string) => Promise<void>,
    options?: { deselectAfter?: boolean },
  ) {
    const ids = Array.from($selectedIds.get())
    if (ids.length === 0) return
    await Promise.allSettled(ids.map(id => apiMethod(id).catch(() => {})))
    if (options?.deselectAfter) deselectAll()
    refreshTaskList()
  }

  const batchPause = () => batchAction(api.pauseTask)
  const batchResume = () => batchAction(api.resumeTask)
  const batchDelete = () => batchAction(api.deleteTask, { deselectAfter: true })

  // memo 化 historyStats，避免每次渲染重复计算
  const stats = createMemo(() => getHistoryStats())

  return (
    <>
      <Layout
        currentView={currentView()}
        onViewChange={setCurrentView}
        onOpenSearch={() => setSearchOpen(true)}
        onPauseAll={batchPause}
        onResumeAll={batchResume}
        detail={currentView() === 'downloads' ? <DetailPanel /> : undefined}
      >
        {currentView() === 'downloads' && <TaskList />}
        {currentView() === 'settings' && <SettingsPanel />}
        {currentView() === 'history' && (
          <HistoryPanel
            records={historyRecords}
            onClear={clearHistory}
          />
        )}
        {currentView() === 'stats' && <StatsDashboard stats={stats()} />}
      </Layout>
      <BatchToolbar
        onPauseAll={batchPause}
        onResumeAll={batchResume}
        onDeleteAll={batchDelete}
      />
      <CommandPalette
        open={searchOpen()}
        onClose={() => setSearchOpen(false)}
        onViewChange={(view) => { setCurrentView(view); setSearchOpen(false) }}
      />
      <div class="fixed bottom-4 right-4 z-50 flex flex-col gap-2 max-w-sm">
        <For each={toasts()}>
          {(toast) => (
            <div
              class={`px-3 py-2 rounded-lg text-[12px] font-medium shadow-elevation-2 animate-slide-up ${
                toast.type === 'error'
                  ? 'bg-error/90 text-white'
                  : toast.type === 'success'
                    ? 'bg-success/90 text-white'
                    : 'bg-surface-elevated text-text-primary border border-border'
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

export default function App() {
  return (
    <ErrorBoundary
      fallback={(err) => (
        <div class="min-h-[100dvh] bg-canvas text-text-primary flex items-center justify-center p-8">
          <div class="glass-panel rounded-lg p-6 max-w-md">
            <div class="text-[16px] font-semibold text-error mb-2">应用发生错误</div>
            <div class="text-[13px] text-text-secondary font-mono break-all">{String(err)}</div>
          </div>
        </div>
      )}
    >
      <AppContent />
    </ErrorBoundary>
  )
}
