import { createSignal, Show, For, untrack } from 'solid-js'
import type { JSX } from 'solid-js'
import type { SidebarFilter } from '../types'
import {
    FileIcon, VideoIcon, AudioIcon, DocumentIcon, ImageIcon,
    ArchiveIcon, AttachmentIcon, PinIcon, PinOffIcon,
    HistoryIcon,
} from './icons'

interface SidebarProps {
    filter: SidebarFilter
    onFilterChange: (filter: SidebarFilter) => void
    taskCounts: {
        all: number
        downloading: number
        completed: number
        paused: number
        failed: number
    }
    onOpenSniffer: () => void
    onOpenHistory: () => void
}

const SIDEBAR_STORAGE_KEY = 'tachyon-sidebar-state'
const MIN_WIDTH = 60
const MAX_WIDTH = 300
const DEFAULT_WIDTH = 200
const EDGE_ZONE_WIDTH = 20
type IconComponent = (props: { class?: string }) => JSX.Element

const statusItems: { key: SidebarFilter; label: string; icon: IconComponent }[] = [
    { key: 'all', label: '\u5168\u90E8\u6587\u4EF6', icon: FileIcon },
    { key: 'downloading', label: '\u4E0B\u8F7D\u4E2D', icon: FileIcon },
    { key: 'completed', label: '\u5DF2\u5B8C\u6210', icon: FileIcon },
    { key: 'paused', label: '\u5DF2\u6682\u505C', icon: FileIcon },
    { key: 'failed', label: '\u5931\u8D25', icon: FileIcon },
]

const typeItems: { key: string; label: string; icon: IconComponent }[] = [
    { key: 'video', label: '\u89C6\u9891', icon: VideoIcon },
    { key: 'audio', label: '\u97F3\u9891', icon: AudioIcon },
    { key: 'document', label: '\u6587\u6863', icon: DocumentIcon },
    { key: 'image', label: '\u56FE\u7247', icon: ImageIcon },
    { key: 'archive', label: '\u538B\u7F29\u5305', icon: ArchiveIcon },
    { key: 'other', label: '\u5176\u4ED6', icon: AttachmentIcon },
]

function loadSidebarState() {
    try {
        const raw = localStorage.getItem(SIDEBAR_STORAGE_KEY)
        if (raw) return JSON.parse(raw) as { width: number; pinned: boolean }
    } catch { /* ignore */ }
    return { width: DEFAULT_WIDTH, pinned: false }
}

function saveSidebarState(w: number, pinned: boolean) {
    try {
        localStorage.setItem(SIDEBAR_STORAGE_KEY, JSON.stringify({ width: w, pinned }))
    } catch { /* ignore */ }
}

export default function Sidebar(props: SidebarProps) {
    const saved = loadSidebarState()
    const [width, setWidth] = createSignal(saved.width)
    const [isPinned, setIsPinned] = createSignal(saved.pinned)
    const [isHovering, setIsHovering] = createSignal(false)
    const [isDragging, setIsDragging] = createSignal(false)
    let hideTimer: number | null = null

    const isExpanded = () => isPinned() || isHovering()
    const displayWidth = () => isExpanded() ? Math.max(width(), DEFAULT_WIDTH) : MIN_WIDTH

    const showSidebar = () => {
        if (hideTimer) { clearTimeout(hideTimer); hideTimer = null }
        setIsHovering(true)
    }

    const hideSidebar = () => {
        if (isPinned() || isDragging()) return
        hideTimer = window.setTimeout(() => setIsHovering(false), 300)
    }

    const handleDragStart = (e: MouseEvent) => {
        e.preventDefault()
        setIsDragging(true)
        const startX = e.clientX
        const startWidth = width()

        const handleMove = (ev: MouseEvent) => {
            const newWidth = Math.max(MIN_WIDTH, Math.min(MAX_WIDTH, startWidth + ev.clientX - startX))
            setWidth(newWidth)
        }

        const handleUp = () => {
            setIsDragging(false)
            saveSidebarState(width(), isPinned())
            document.removeEventListener('mousemove', handleMove)
            document.removeEventListener('mouseup', handleUp)
        }

        document.addEventListener('mousemove', handleMove)
        document.addEventListener('mouseup', handleUp)
    }

    const togglePin = () => {
        const next = !isPinned()
        setIsPinned(next)
        if (next && width() < DEFAULT_WIDTH) setWidth(DEFAULT_WIDTH)
        saveSidebarState(width(), next)
    }

    const NavItem = (p: { icon: IconComponent; label: string; count: number; active: boolean; onClick: () => void }) => {
        const Icon = untrack(() => p.icon)
        const showText = () => isExpanded()
        return (
            <div
                class={`flex items-center justify-between cursor-pointer select-none ${p.active ? '' : 'hover-light'}`}
                style={{
                    height: '36px',
                    padding: showText() ? '0 12px' : '0',
                    'border-radius': '8px',
                    background: p.active ? 'rgba(0,212,170,0.1)' : 'transparent',
                    'border-left': p.active ? '2px solid #00D4AA' : '2px solid transparent',
                    color: p.active ? '#00D4AA' : '#A0A0B0',
                    transition: 'all 200ms ease',
                    'justify-content': showText() ? 'space-between' : 'center',
                }}
                onClick={() => p.onClick()}
            >
                <div class="flex items-center min-w-0" style={{ gap: showText() ? '12px' : '0' }}>
                    <div style={{ width: '20px', height: '20px', display: 'flex', 'align-items': 'center', 'justify-content': 'center', 'flex-shrink': 0 }}>
                        <Icon />
                    </div>
                    <Show when={showText()}>
                        <span class="truncate" style={{ 'font-size': '14px' }}>{p.label}</span>
                    </Show>
                </div>
                <Show when={showText()}>
                    <span style={{ 'font-size': '12px', color: '#6B7280', 'flex-shrink': 0 }}>{p.count}</span>
                </Show>
            </div>
        )
    }

    return (
        <>
            {/* Edge trigger zone - only when collapsed and not pinned */}
            <Show when={!isPinned() && !isHovering()}>
                <div
                    class="fixed left-0 top-0 bottom-0 z-[5]"
                    style={{ width: `${EDGE_ZONE_WIDTH}px` }}
                    onMouseEnter={showSidebar}
                    onMouseLeave={hideSidebar}
                />
            </Show>

            {/* Sidebar panel */}
            <div
                class="relative flex-shrink-0 h-full overflow-hidden"
                style={{
                    width: `${displayWidth()}px`,
                    transition: isDragging() ? 'none' : 'width 250ms cubic-bezier(0.32, 0.72, 0, 1)',
                    'will-change': 'width',
                }}
                onMouseEnter={showSidebar}
                onMouseLeave={hideSidebar}
            >
                <div
                    class="h-full flex flex-col"
                    style={{
                        width: `${Math.max(displayWidth(), DEFAULT_WIDTH)}px`,
                        background: '#12121A',
                        'border-right': '1px solid rgba(255,255,255,0.05)',
                        'pointer-events': isExpanded() ? 'auto' : 'none',
                        opacity: isExpanded() ? 1 : 0,
                        transition: 'opacity 200ms ease',
                        position: 'absolute',
                        left: 0,
                        top: 0,
                        bottom: 0,
                    }}
                >
                    {/* Pin header */}
                    <div
                        class="flex items-center justify-between flex-shrink-0"
                        style={{
                            height: '40px',
                            padding: isExpanded() ? '0 12px' : '0 4px',
                            'border-bottom': '1px solid rgba(255,255,255,0.05)',
                        }}
                    >
                        <Show when={isExpanded()}>
                            <span style={{ 'font-size': '11px', 'font-weight': 600, color: '#6B7280', 'letter-spacing': '0.5px' }}>
                                {'\u5BFC\u822A'}
                            </span>
                        </Show>
                        <button
                            class="icon-btn-sm"
                            style={{
                                color: isPinned() ? '#00D4AA' : '#6B7280',
                                margin: !isExpanded() ? '0 auto' : '0',
                            }}
                            onClick={togglePin}
                            title={isPinned() ? '\u53D6\u6D88\u56FA\u5B9A' : '\u56FA\u5B9A\u4FA7\u8FB9\u680F'}
                        >
                            {isPinned() ? <PinIcon /> : <PinOffIcon />}
                        </button>
                    </div>

                    {/* Status section */}
                    <div class="flex flex-col gap-1" style={{ padding: '8px 6px' }}>
                        <Show when={isExpanded()}>
                            <div
                                style={{
                                    'font-size': '11px',
                                    'font-weight': 600,
                                    color: '#6B7280',
                                    'text-transform': 'uppercase',
                                    'letter-spacing': '0.5px',
                                    padding: '0 8px',
                                    'margin-bottom': '4px',
                                }}
                            >
                                {'\u72B6\u6001'}
                            </div>
                        </Show>
                        <For each={statusItems}>
                            {(item) => (
                                <NavItem
                                    icon={item.icon}
                                    label={item.label}
                                    count={props.taskCounts[item.key]}
                                    active={props.filter === item.key}
                                    onClick={() => props.onFilterChange(item.key)}
                                />
                            )}
                        </For>
                    </div>

                    <div style={{ height: '1px', background: 'rgba(255,255,255,0.05)', margin: '4px 10px' }} />

                    {/* Type section */}
                    <div class="flex flex-col gap-1" style={{ padding: '8px 6px' }}>
                        <Show when={isExpanded()}>
                            <div
                                style={{
                                    'font-size': '11px',
                                    'font-weight': 600,
                                    color: '#6B7280',
                                    'text-transform': 'uppercase',
                                    'letter-spacing': '0.5px',
                                    padding: '0 8px',
                                    'margin-bottom': '4px',
                                }}
                            >
                                {'\u5206\u7C7B'}
                            </div>
                        </Show>
                        <For each={typeItems}>
                            {(item) => (
                                <NavItem
                                    icon={item.icon}
                                    label={item.label}
                                    count={0}
                                    active={false}
                                    onClick={() => { }}
                                />
                            )}
                        </For>
                    </div>

                    <div style={{ height: '1px', background: 'rgba(255,255,255,0.05)', margin: '4px 10px' }} />

                    {/* Lab section */}
                    <div class="flex flex-col gap-1" style={{ padding: '8px 6px' }}>
                        <Show when={isExpanded()}>
                            <div
                                style={{
                                    'font-size': '11px',
                                    'font-weight': 600,
                                    color: '#6B7280',
                                    'text-transform': 'uppercase',
                                    'letter-spacing': '0.5px',
                                    padding: '0 8px',
                                    'margin-bottom': '4px',
                                }}
                            >
                                {'\u5B9E\u9A8C\u5BA4'}
                            </div>
                        </Show>
                        <NavItem
                            icon={HistoryIcon}
                            label={'\u5386\u53F2'}
                            count={0}
                            active={false}
                            onClick={props.onOpenHistory}
                        />
                    </div>

                    <div class="flex-1" />

                    {/* Resize handle */}
                    <div
                        class="resize-handle absolute right-0 top-0 bottom-0 cursor-col-resize z-10"
                        style={{
                            width: '4px',
                            background: isDragging() ? 'rgba(0,212,170,0.4)' : 'transparent',
                            transition: 'background 150ms ease',
                        }}
                        onMouseDown={handleDragStart}
                    />
                </div>
            </div>
        </>
    )
}
