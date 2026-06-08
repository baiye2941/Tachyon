import { createSignal, createMemo, createEffect, Show, onMount, onCleanup, ErrorBoundary } from 'solid-js'
import type { SidebarFilter, FileTypeFilter, ListDensity, TaskInfo, SnifferResource, ViewName } from './types'
import { api } from './api/invoke'
import { $tasks, $totalSpeed, $activeCount, refreshTaskList } from './stores/downloads'
import { useProgressListener } from './hooks/useTauriEvent'
import * as speedHistory from './stores/speedHistory'
import { $selectedIds, deselectAll, selectAll, toggleSelection } from './stores/selection'
import TitleBar from './components/TitleBar'
import Sidebar from './components/Sidebar'
import Toolbar from './components/Toolbar'
import TaskList from './components/TaskList'
import DetailPanel from './components/DetailPanel'
import StatusBar from './components/StatusBar'
import ToastContainer from './components/ToastContainer'
import NewTaskModal from './components/NewTaskModal'
import ContextMenu from './components/ContextMenu'
import SnifferPanel from './components/SnifferPanel'
import HistoryPanel from './components/HistoryPanel'
import SettingsPanel from './components/SettingsPanel'
import CommandPalette from './components/CommandPalette'
import BatchToolbar from './components/BatchToolbar'

function AppContent() {
    const [selectedTaskId, setSelectedTaskId] = createSignal<string | null>(null)
    const [sidebarFilter, setSidebarFilter] = createSignal<SidebarFilter>('all')
    const [fileTypeFilter] = createSignal<FileTypeFilter>('all')
    const [listDensity, setListDensity] = createSignal<ListDensity>('comfortable')
    const [isMultiSelectMode, setIsMultiSelectMode] = createSignal(false)
    const [showNewTaskModal, setShowNewTaskModal] = createSignal(false)
    const [searchQuery, setSearchQuery] = createSignal('')
    const [searchOpen, setSearchOpen] = createSignal(false)

    // Panel visibility
    const [snifferVisible, setSnifferVisible] = createSignal(false)
    const [historyVisible, setHistoryVisible] = createSignal(false)
    const [settingsVisible, setSettingsVisible] = createSignal(false)

    // Context menu
    const [contextMenu, setContextMenu] = createSignal<{
        visible: boolean
        x: number
        y: number
        task: TaskInfo | null
    }>({ visible: false, x: 0, y: 0, task: null })

    // Drag & drop
    const [isDragOver, setIsDragOver] = createSignal(false)

    // Sniffer resources
    const [snifferResources, setSnifferResources] = createSignal<SnifferResource[]>([])

    // 真实数据订阅
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
        // 加载 sniffer 资源
        api.getSnifferResources().then(setSnifferResources).catch(() => {})
        window.addEventListener('keydown', handleGlobalKey)
    })

    onCleanup(() => window.removeEventListener('keydown', handleGlobalKey))

    // speedHistory effect
    createEffect(() => {
        speedHistory.pushSpeed($totalSpeed.get())
        speedHistory.setActiveTasksCount($activeCount.get())
    })

    // Parse search query for filter tags and text query
    const searchFilters = createMemo(() => {
        const query = searchQuery().trim()
        const filters: { type: string; value: string; raw: string }[] = []
        let textQuery = query

        // Match filter syntax: type:value or status:value or size:>value etc.
        const filterRegex = /(\w+):([^\s]+)/g
        let match
        while ((match = filterRegex.exec(query)) !== null) {
            const [, type, value] = match
            if (['status', 'type', 'size', 'speed', 'name'].includes(type)) {
                filters.push({ type, value, raw: match[0] })
                textQuery = textQuery.replace(match[0], '').trim()
            }
        }

        return { filters, textQuery }
    })

    const filteredTasks = createMemo(() => {
        let result = $tasks.get()

        // Sidebar filter
        const sf = sidebarFilter()
        if (sf !== 'all') {
            result = result.filter(t => {
                if (sf === 'downloading') return t.status === 'downloading' || t.status === 'connecting'
                if (sf === 'completed') return t.status === 'completed'
                if (sf === 'paused') return t.status === 'paused'
                if (sf === 'failed') return t.status === 'failed'
                return true
            })
        }

        // File type filter
        const tf = fileTypeFilter()
        if (tf !== 'all') {
            result = result.filter(t => {
                const ext = t.fileName.split('.').pop()?.toLowerCase() || ''
                const typeMap: Record<string, FileTypeFilter> = {
                    mp4: 'video', mkv: 'video', avi: 'video', mov: 'video', webm: 'video',
                    mp3: 'audio', wav: 'audio', flac: 'audio', aac: 'audio', ogg: 'audio',
                    pdf: 'document', doc: 'document', docx: 'document', txt: 'document', xls: 'document', xlsx: 'document',
                    jpg: 'image', jpeg: 'image', png: 'image', gif: 'image', webp: 'image', svg: 'image',
                    zip: 'archive', rar: 'archive', '7z': 'archive', tar: 'archive', gz: 'archive',
                    exe: 'executable', msi: 'executable', dmg: 'executable', sh: 'executable',
                }
                return typeMap[ext] === tf
            })
        }

        // Advanced search filters
        const { filters, textQuery } = searchFilters()
        for (const filter of filters) {
            result = result.filter(t => {
                if (filter.type === 'status') {
                    return t.status.toLowerCase() === filter.value.toLowerCase()
                }
                if (filter.type === 'type') {
                    const ext = t.fileName.split('.').pop()?.toLowerCase() || ''
                    const typeMap: Record<string, string> = {
                        mp4: 'video', mkv: 'video', avi: 'video', mov: 'video', webm: 'video',
                        mp3: 'audio', wav: 'audio', flac: 'audio', aac: 'audio', ogg: 'audio',
                        pdf: 'document', doc: 'document', docx: 'document', txt: 'document',
                        jpg: 'image', jpeg: 'image', png: 'image', gif: 'image', webp: 'image', svg: 'image',
                        zip: 'archive', rar: 'archive', '7z': 'archive', tar: 'archive', gz: 'archive',
                        exe: 'executable', msi: 'executable', dmg: 'executable', sh: 'executable',
                    }
                    return typeMap[ext] === filter.value.toLowerCase()
                }
                if (filter.type === 'size') {
                    if (!t.fileSize) return false
                    const val = filter.value.toLowerCase()
                    if (val.startsWith('>')) {
                        const num = parseSize(val.slice(1))
                        return t.fileSize > num
                    }
                    if (val.startsWith('<')) {
                        const num = parseSize(val.slice(1))
                        return t.fileSize < num
                    }
                    if (val.includes('..')) {
                        const [min, max] = val.split('..').map(parseSize)
                        return t.fileSize >= min && t.fileSize <= max
                    }
                    return t.fileSize === parseSize(val)
                }
                if (filter.type === 'speed') {
                    const val = filter.value.toLowerCase()
                    if (val.startsWith('>')) {
                        const num = parseSize(val.slice(1))
                        return t.speed > num
                    }
                    return true
                }
                if (filter.type === 'name') {
                    return t.fileName.toLowerCase().includes(filter.value.toLowerCase())
                }
                return true
            })
        }

        // Text search
        if (textQuery) {
            result = result.filter(t => t.fileName.toLowerCase().includes(textQuery.toLowerCase()))
        }

        return result
    })

    function parseSize(val: string): number {
        const v = val.trim().toLowerCase()
        const num = parseFloat(v)
        if (v.includes('gb')) return num * 1024 * 1024 * 1024
        if (v.includes('mb')) return num * 1024 * 1024
        if (v.includes('kb')) return num * 1024
        return num
    }

    const selectedTask = createMemo(() => {
        const id = selectedTaskId()
        return id ? $tasks.get().find(t => t.id === id) || null : null
    })

    const handleDetailClose = () => {
        setSelectedTaskId(null)
    }

    const handleTaskClick = (taskId: string) => {
        if (isMultiSelectMode()) {
            toggleSelection(taskId)
        } else {
            setSelectedTaskId(prev => prev === taskId ? null : taskId)
        }
    }

    const handleTaskContextMenu = (e: MouseEvent, taskId: string) => {
        e.preventDefault()
        const task = $tasks.get().find(t => t.id === taskId) || null
        setContextMenu({ visible: true, x: e.clientX, y: e.clientY, task })
    }

    const handleSelectAll = () => {
        const allIds = filteredTasks().map(t => t.id)
        const current = $selectedIds.get()
        if (current.size === allIds.length) {
            deselectAll()
        } else {
            selectAll(allIds)
        }
    }

    const handleBatchAction = (action: (taskId: string) => Promise<unknown>) => {
        const ids = Array.from($selectedIds.get())
        Promise.allSettled(ids.map(action)).then(() => {
            deselectAll()
            refreshTaskList()
        }).catch((e) => console.error('batch action', e))
    }

    const handlePauseSelected = () => handleBatchAction(api.pauseTask)
    const handleResumeSelected = () => handleBatchAction(api.resumeTask)
    const handleDeleteSelected = () => handleBatchAction(api.deleteTask)

    const handleViewChange = (view: ViewName) => {
        setSearchOpen(false)
        setSnifferVisible(view === 'sniffer')
        setHistoryVisible(view === 'history')
        setSettingsVisible(view === 'settings')
        if (view === 'downloads' || view === 'stats') {
            setSnifferVisible(false)
            setHistoryVisible(false)
            setSettingsVisible(false)
        }
    }

    const handleAddFromSniffer = (resource: SnifferResource) => {
        api.createTask(resource.url).then(() => {
            refreshTaskList()
        }).catch((e) => console.error('create task from sniffer', e))
    }

    const handleRedownload = (task: TaskInfo) => {
        api.createTask(task.url).then(() => {
            refreshTaskList()
        }).catch((e) => console.error('redownload', e))
    }

    const handleDeleteRecord = (taskId: string) => {
        api.deleteTask(taskId).then(() => {
            refreshTaskList()
        }).catch((e) => console.error('delete record', e))
    }

    // Drag & drop handlers
    const handleDragOver = (e: DragEvent) => {
        e.preventDefault()
        e.stopPropagation()
        setIsDragOver(true)
    }

    const handleDragLeave = (e: DragEvent) => {
        e.preventDefault()
        e.stopPropagation()
        const rect = (e.currentTarget as HTMLElement).getBoundingClientRect()
        const x = e.clientX
        const y = e.clientY
        if (x <= rect.left || x >= rect.right || y <= rect.top || y >= rect.bottom) {
            setIsDragOver(false)
        }
    }

    const handleDrop = (e: DragEvent) => {
        e.preventDefault()
        e.stopPropagation()
        setIsDragOver(false)

        const text = e.dataTransfer?.getData('text/plain')
        if (text) {
            const urls = text.split('\n').filter(u => u.trim())
            urls.forEach(url => {
                api.createTask(url.trim()).catch((e) => console.error('create task from drop', e))
            })
            setTimeout(() => refreshTaskList(), 300)
        }
    }

    const pausedCount = createMemo(() => $tasks.get().filter(t => t.status === 'paused').length)

    return (
        <div
            class="w-screen h-screen flex flex-col overflow-hidden"
            style={{ background: 'var(--color-bg-primary)' }}
            onDragOver={handleDragOver}
            onDragLeave={handleDragLeave}
            onDrop={handleDrop}
        >
            {/* Drag overlay */}
            <Show when={isDragOver()}>
                <div
                    class="fixed inset-0 z-[300] flex items-center justify-center"
                    style={{
                        background: 'rgba(0, 0, 0, 0.6)',
                        'backdrop-filter': 'blur(4px)',
                        animation: 'fadeIn 150ms ease forwards',
                    }}
                >
                    <div
                        class="flex flex-col items-center gap-4"
                        style={{
                            padding: '48px 64px',
                            'border-radius': '16px',
                            border: '2px dashed #00D4AA',
                            background: 'rgba(0, 212, 170, 0.05)',
                        }}
                    >
                        <svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="#00D4AA" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
                            <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                            <polyline points="17 8 12 3 7 8" />
                            <line x1="12" y1="3" x2="12" y2="15" />
                        </svg>
                        <span style={{ 'font-size': '16px', color: '#00D4AA', 'font-weight': 500 }}>
                            拖放链接到此处开始下载
                        </span>
                    </div>
                </div>
            </Show>

            <TitleBar
                onOpenSettings={() => setSettingsVisible(true)}
            />

            <div class="flex flex-1 overflow-hidden">
                <Sidebar
                    filter={sidebarFilter()}
                    onFilterChange={setSidebarFilter}
                    taskCounts={{
                        all: $tasks.get().length,
                        downloading: $tasks.get().filter(t => t.status === 'downloading').length,
                        completed: $tasks.get().filter(t => t.status === 'completed').length,
                        paused: $tasks.get().filter(t => t.status === 'paused').length,
                        failed: $tasks.get().filter(t => t.status === 'failed').length,
                    }}
                    onOpenSniffer={() => setSnifferVisible(true)}
                    onOpenHistory={() => setHistoryVisible(true)}
                />

                <div class="flex-1 flex flex-col min-w-0">
                    <Toolbar
                        searchQuery={searchQuery()}
                        onSearchChange={setSearchQuery}
                        filters={searchFilters().filters}
                        onRemoveFilter={(raw) => setSearchQuery(q => q.replace(raw, '').trim())}
                        isMultiSelectMode={isMultiSelectMode()}
                        onToggleMultiSelect={() => {
                            setIsMultiSelectMode(v => !v)
                            deselectAll()
                        }}
                        selectedCount={$selectedIds.get().size}
                        onSelectAll={handleSelectAll}
                        onPauseSelected={handlePauseSelected}
                        onResumeSelected={handleResumeSelected}
                        onDeleteSelected={handleDeleteSelected}
                        onExitMultiSelect={() => {
                            setIsMultiSelectMode(false)
                            deselectAll()
                        }}
                        listDensity={listDensity()}
                        onToggleDensity={() => setListDensity(d => d === 'comfortable' ? 'compact' : 'comfortable')}
                        onNewTask={() => setShowNewTaskModal(true)}
                        onOpenSettings={() => setSettingsVisible(true)}
                    />

                    <div class="flex flex-1 overflow-hidden">
                        <TaskList
                            tasks={filteredTasks()}
                            selectedTaskId={selectedTaskId()}
                            onTaskClick={handleTaskClick}
                            onTaskContextMenu={handleTaskContextMenu}
                            isMultiSelectMode={isMultiSelectMode()}
                            selectedTaskIds={$selectedIds.get()}
                            density={listDensity()}
                            searchQuery={searchQuery()}
                        />

                        <DetailPanel
                            task={selectedTask()}
                            onClose={handleDetailClose}
                        />
                    </div>
                </div>
            </div>

            <StatusBar
                isIdle={$activeCount.get() === 0}
                totalSpeed={$totalSpeed.get()}
                activeCount={$activeCount.get()}
                pausedCount={pausedCount()}
                totalCount={$tasks.get().length}
            />

            <ToastContainer />

            <Show when={showNewTaskModal()}>
                <NewTaskModal onClose={() => setShowNewTaskModal(false)} />
            </Show>

            {/* Context Menu */}
            <ContextMenu
                x={contextMenu().x}
                y={contextMenu().y}
                visible={contextMenu().visible}
                task={contextMenu().task}
                onClose={() => setContextMenu(prev => ({ ...prev, visible: false }))}
                onPause={(taskId) => {
                    api.pauseTask(taskId).then(() => refreshTaskList()).catch(e => console.error(e))
                }}
                onResume={(taskId) => {
                    api.resumeTask(taskId).then(() => refreshTaskList()).catch(e => console.error(e))
                }}
                onOpenFolder={() => { }}
                onCopyLink={(taskId) => {
                    const task = $tasks.get().find(t => t.id === taskId)
                    if (task) navigator.clipboard.writeText(task.url)
                }}
                onRedownload={(taskId) => {
                    const task = $tasks.get().find(t => t.id === taskId)
                    if (task) handleRedownload(task)
                }}
                onDelete={(taskId) => {
                    api.deleteTask(taskId).then(() => refreshTaskList()).catch(e => console.error(e))
                    if (selectedTaskId() === taskId) setSelectedTaskId(null)
                }}
                onDeleteWithFile={(taskId) => {
                    api.deleteTask(taskId).then(() => refreshTaskList()).catch(e => console.error(e))
                    if (selectedTaskId() === taskId) setSelectedTaskId(null)
                }}
            />

            {/* Panels */}
            <SnifferPanel
                visible={snifferVisible()}
                resources={snifferResources()}
                onClose={() => setSnifferVisible(false)}
                onAddDownload={handleAddFromSniffer}
            />

            <HistoryPanel
                visible={historyVisible()}
                tasks={$tasks.get()}
                onClose={() => setHistoryVisible(false)}
                onOpenFolder={() => { }}
                onRedownload={handleRedownload}
                onDeleteRecord={handleDeleteRecord}
            />

            <SettingsPanel
                visible={settingsVisible()}
                onClose={() => setSettingsVisible(false)}
            />

            <CommandPalette
                open={searchOpen()}
                onClose={() => setSearchOpen(false)}
                onViewChange={handleViewChange}
                onNewDownload={() => setShowNewTaskModal(true)}
                onPauseAll={handlePauseSelected}
                onResumeAll={handleResumeSelected}
            />

            <BatchToolbar
                onPauseAll={handlePauseSelected}
                onResumeAll={handleResumeSelected}
                onDeleteAll={handleDeleteSelected}
            />
        </div>
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
