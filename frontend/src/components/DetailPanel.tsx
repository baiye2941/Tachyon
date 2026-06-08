import { createMemo, createSignal, createEffect, Show, onCleanup, untrack } from 'solid-js'
import type { TaskInfo } from '../types'
import { formatSize, formatSpeed, getFileType, getStatusLabel, getStatusColor, formatETA, formatDate } from '../utils/format'
import {
    CloseIcon, PauseIcon, PlayIcon,
    FileIcon,
    OpenFileIcon, MoreIcon, CopyIcon, FolderOpenIcon, RefreshIcon,
} from './icons'
import ChunkMatrix from './ChunkMatrix'
import SpeedChart from './SpeedChart'

interface DetailPanelProps {
    task: TaskInfo | null
    onClose: () => void
}

export default function DetailPanel(props: DetailPanelProps) {
    const [displayTask, setDisplayTask] = createSignal<TaskInfo | null>(null)
    const [shouldRender, setShouldRender] = createSignal(false)
    const [visible, setVisible] = createSignal(false)
    const [menuOpen, setMenuOpen] = createSignal(false)
    const [copied, setCopied] = createSignal<string | null>(null)

    let closeTimer: number | null = null
    let copiedTimer: number | null = null
    let menuRef: HTMLDivElement | undefined

    const cancelCloseTimer = () => {
        if (closeTimer !== null) {
            clearTimeout(closeTimer)
            closeTimer = null
        }
    }

    createEffect(() => {
        const task = props.task
        if (task) {
            cancelCloseTimer()
            setDisplayTask(task)
            setMenuOpen(false)
            if (!shouldRender()) {
                setShouldRender(true)
                requestAnimationFrame(() => {
                    requestAnimationFrame(() => {
                        setVisible(true)
                    })
                })
            } else {
                setVisible(true)
            }
        } else if (shouldRender() && visible()) {
            setVisible(false)
            cancelCloseTimer()
            closeTimer = window.setTimeout(() => {
                setShouldRender(false)
                setDisplayTask(null)
                closeTimer = null
            }, 300)
        }
    })

    // Click outside to close menu
    createEffect(() => {
        if (!menuOpen()) return
        const handler = (e: MouseEvent) => {
            if (menuRef && !menuRef.contains(e.target as Node)) {
                setMenuOpen(false)
            }
        }
        document.addEventListener('mousedown', handler)
        onCleanup(() => document.removeEventListener('mousedown', handler))
    })

    const handleClose = () => {
        setVisible(false)
        cancelCloseTimer()
        closeTimer = window.setTimeout(() => {
            setShouldRender(false)
            setDisplayTask(null)
            closeTimer = null
            untrack(() => props.onClose())
        }, 300)
    }

    const task = () => displayTask()
    const fileInfo = createMemo(() => {
        const currentTask = task()
        return currentTask ? getFileType(currentTask.fileName) : { icon: FileIcon, color: '#6B7280' }
    })
    const isCompleted = () => task()?.status === 'completed'
    const isFailed = () => task()?.status === 'failed'
    const isDownloading = () => task()?.status === 'downloading'

    const eta = createMemo(() => {
        const t = task()
        if (!t || !isDownloading()) return '---'
        const remaining = (t.fileSize || 0) - t.downloaded
        return formatETA(t.speed, remaining)
    })

    const copyToClipboard = (text: string, label: string) => {
        navigator.clipboard.writeText(text)
        setCopied(label)
        if (copiedTimer !== null) clearTimeout(copiedTimer)
        copiedTimer = window.setTimeout(() => {
            setCopied(null)
            copiedTimer = null
        }, 1500)
    }

    return (
        <Show when={shouldRender()}>
            <div
                style={{
                    display: 'grid',
                    'grid-template-columns': visible() ? '400px' : '0px',
                    'grid-template-rows': '1fr',
                    transition: 'grid-template-columns 280ms cubic-bezier(0.32, 0.72, 0, 1)',
                    overflow: 'hidden',
                    'min-height': 0,
                    height: '100%',
                    opacity: visible() ? 1 : 0,
                    'pointer-events': visible() ? 'auto' : 'none',
                }}
            >
                <div
                    style={{
                        width: '400px',
                        'max-width': '100%',
                        background: '#12121A',
                        'border-left': '1px solid rgba(255,255,255,0.05)',
                        transition: 'opacity 220ms ease',
                    }}
                    class="flex flex-col h-full overflow-y-auto overflow-x-hidden"
                >
                    {/* Header - compact */}
                    <div class="panel-header">
                        <div class="flex items-center gap-2">
                            <Show when={isCompleted()}>
                                <button class="btn-secondary" style={{ padding: '6px 12px', 'font-size': '12px' }}>
                                    <OpenFileIcon />
                                    <span>{'\u6253\u5F00'}</span>
                                </button>
                            </Show>
                            <Show when={!isCompleted()}>
                                <button class="btn-primary hover-lift-sm" style={{ padding: '6px 12px', 'font-size': '12px' }}>
                                    {isDownloading() ? <><PauseIcon /><span>{'\u6682\u505C'}</span></> : <><PlayIcon /><span>{'\u6062\u590D'}</span></>}
                                </button>
                            </Show>
                        </div>
                        <div class="flex items-center gap-1">
                            <div style={{ position: 'relative' }}>
                                <button class="icon-btn-sm" onClick={() => setMenuOpen(v => !v)}>
                                    <MoreIcon />
                                </button>
                                <Show when={menuOpen()}>
                                    <div class="detail-menu" ref={menuRef}>
                                        <button class="detail-menu-item" onClick={() => { copyToClipboard(task()?.url || '', 'url'); setMenuOpen(false) }}>
                                            <CopyIcon />
                                            <span>{'\u590D\u5236\u94FE\u63A5'}</span>
                                        </button>
                                        <button class="detail-menu-item" onClick={() => setMenuOpen(false)}>
                                            <FolderOpenIcon />
                                            <span>{'\u6253\u5F00\u6587\u4EF6\u5939'}</span>
                                        </button>
                                        <button class="detail-menu-item" onClick={() => setMenuOpen(false)}>
                                            <RefreshIcon />
                                            <span>{'\u91CD\u65B0\u4E0B\u8F7D'}</span>
                                        </button>
                                    </div>
                                </Show>
                            </div>
                            <button class="icon-btn-sm" onClick={handleClose}>
                                <CloseIcon />
                            </button>
                        </div>
                    </div>

                    {/* File Info - compact inline layout */}
                    <div class="flex items-center gap-3" style={{ padding: '12px 20px', 'max-width': '100%' }}>
                        <div
                            class="flex items-center justify-center flex-shrink-0"
                            style={{
                                width: '36px',
                                height: '36px',
                                color: fileInfo().color,
                            }}
                        >
                            {(() => {
                                const Icon = fileInfo().icon
                                return <Icon />
                            })()}
                        </div>
                        <div class="min-w-0 flex-1">
                            <div
                                class="truncate"
                                style={{
                                    'font-size': '14px',
                                    'font-weight': 600,
                                    color: '#F0F0F5',
                                    'max-width': '100%',
                                }}
                            >
                                {task()?.fileName}
                            </div>
                            <div class="flex items-center gap-2" style={{ 'margin-top': '2px' }}>
                                <span
                                    class="flex-shrink-0"
                                    style={{
                                        'font-size': '10px',
                                        color: '#6B7280',
                                        padding: '1px 6px',
                                        'border-radius': '3px',
                                        background: 'rgba(255,255,255,0.04)',
                                    }}
                                >
                                    {task()?.url?.split(':')[0]?.toUpperCase() || ''}
                                </span>
                                <span style={{ 'font-size': '11px', color: getStatusColor(task()?.status || ''), 'font-weight': 600 }}>
                                    {getStatusLabel(task()?.status || '')}
                                </span>
                            </div>
                        </div>
                    </div>

                    {/* Progress Section - compact */}
                    <div class="flex flex-col items-center" style={{ padding: '0 20px 12px' }}>
                        <div
                            class="mono"
                            style={{
                                'font-size': '24px',
                                'font-weight': 700,
                                color: '#F0F0F5',
                                'line-height': '1.2',
                            }}
                        >
                            {((task()?.progress || 0) * 100).toFixed(1)}%
                        </div>

                        {/* Progress bar */}
                        <div
                            class="relative overflow-hidden w-full"
                            style={{
                                height: '4px',
                                'margin-top': '8px',
                                'border-radius': '9999px',
                                background: '#1A1A25',
                            }}
                        >
                            <div
                                class={`absolute left-0 top-0 bottom-0${isDownloading() ? ' progress-bar-active' : ''}`}
                                style={{
                                    width: `${(task()?.progress || 0) * 100}%`,
                                    'border-radius': '9999px',
                                    background: isFailed()
                                        ? '#EF4444'
                                        : isDownloading()
                                            ? undefined
                                            : 'linear-gradient(90deg, #00D4AA 0%, #00B4D8 100%)',
                                    transition: 'width 300ms ease-out',
                                }}
                            />
                        </div>

                        {/* Progress stats row */}
                        <div class="detail-progress-row">
                            <span class="mono" style={{ 'font-size': '11px', color: '#A0A0B0' }}>
                                {formatSize(task()?.downloaded || 0)}
                            </span>
                            <span class="mono" style={{ 'font-size': '11px', color: '#6B7280' }}>
                                {formatSize(task()?.fileSize || 0)}
                            </span>
                        </div>
                    </div>

                    {/* Error State */}
                    <Show when={isFailed()}>
                        <div style={{ padding: '0 20px 12px' }}>
                            <div class="detail-error-box">
                                <div class="detail-error-icon">
                                    <span style={{ color: '#EF4444', 'font-size': '12px', 'font-weight': 700 }}>!</span>
                                </div>
                                <div class="flex-1 min-w-0">
                                    <div style={{ 'font-size': '13px', color: '#EF4444', 'font-weight': 500 }}>
                                        {'\u4E0B\u8F7D\u5931\u8D25'}
                                    </div>
                                    <div class="truncate" style={{ 'font-size': '12px', color: '#A0A0B0', 'margin-top': '2px' }}>
                                        {'\u8FDE\u63A5\u8D85\u65F6\uFF0C\u8BF7\u68C0\u67E5\u7F51\u7EDC\u540E\u91CD\u8BD5'}
                                    </div>
                                </div>
                                <button
                                    class="btn-secondary"
                                    style={{ padding: '4px 10px', 'font-size': '12px', 'flex-shrink': 0 }}
                                >
                                    <RefreshIcon />
                                    <span>{'\u91CD\u8BD5'}</span>
                                </button>
                            </div>
                        </div>
                    </Show>

                    {/* Stats Grid - 3 columns, moved UP before charts */}
                    <div
                        style={{
                            padding: '0 20px 12px',
                            'border-top': '1px solid rgba(255,255,255,0.05)',
                        }}
                    >
                        <div class="detail-stat-grid" style={{ 'min-width': 0 }}>
                            <div class="detail-stat-cell">
                                <div class="detail-stat-label">{'\u5927\u5C0F'}</div>
                                <div class="detail-stat-value">{task()?.fileSize ? formatSize(task()!.fileSize!) : '\u672A\u77E5'}</div>
                            </div>
                            <div class="detail-stat-cell">
                                <div class="detail-stat-label">{'\u901F\u5EA6'}</div>
                                <div class={`detail-stat-value${isDownloading() ? ' detail-stat-value--highlight' : ''}`}>{formatSpeed(task()?.speed || 0)}</div>
                            </div>
                            <div class="detail-stat-cell">
                                <div class="detail-stat-label">{'\u5269\u4F59'}</div>
                                <div class={`detail-stat-value${isDownloading() ? ' detail-stat-value--highlight' : ''}`}>{eta()}</div>
                            </div>
                            <div class="detail-stat-cell">
                                <div class="detail-stat-label">{'\u5206\u7247'}</div>
                                <div class="detail-stat-value">{`${task()?.fragmentsDone || 0}/${task()?.fragmentsTotal || 0}`}</div>
                            </div>
                            <div class="detail-stat-cell">
                                <div class="detail-stat-label">{'\u5DF2\u4E0B\u8F7D'}</div>
                                <div class="detail-stat-value">{formatSize(task()?.downloaded || 0)}</div>
                            </div>
                            <div class="detail-stat-cell">
                                <div class="detail-stat-label">{'\u72B6\u6001'}</div>
                                <div class="detail-stat-value">{getStatusLabel(task()?.status || '')}</div>
                            </div>
                        </div>
                    </div>

                    {/* URL & Path - compact */}
                    <div style={{ padding: '0 20px 12px' }}>
                        <InfoRow
                            label={'\u4E0B\u8F7D\u94FE\u63A5'}
                            value={task()?.url || ''}
                            copyable
                            copied={copied() === 'url'}
                            onCopy={() => copyToClipboard(task()?.url || '', 'url')}
                        />
                        <InfoRow
                            label={'\u4FDD\u5B58\u8DEF\u5F84'}
                            value={'默认下载目录'}
                        />
                        <InfoRow
                            label={'\u521B\u5EFA\u65F6\u95F4'}
                            value={task()?.createdAt ? formatDate(task()!.createdAt) : '---'}
                        />
                    </div>

                    {/* Speed Chart - collapsible, after stats */}
                    <Show when={task()?.status === 'downloading' || task()?.status === 'paused'}>
                        <div style={{ padding: '0 20px 12px' }}>
                            <SpeedChart task={task()!} />
                        </div>
                    </Show>

                    {/* Chunk Matrix - collapsible, after chart */}
                    <Show when={(task()?.fragmentsTotal || 0) > 0}>
                        <div style={{ padding: '0 20px 12px' }}>
                            <ChunkMatrix
                                fragmentsTotal={task()!.fragmentsTotal}
                                fragmentsDone={task()!.fragmentsDone}
                                progress={task()!.progress}
                            />
                        </div>
                    </Show>

                    {/* Action Buttons - at bottom */}
                    <div class="flex flex-col" style={{ padding: '0 20px 20px', gap: '8px' }}>
                        <Show when={!isCompleted()}>
                            <button class="btn-primary hover-lift detail-action-btn" style={{ 'font-size': '14px' }}>
                                {isDownloading() ? '\u6682\u505C\u4E0B\u8F7D' : '\u6062\u590D\u4E0B\u8F7D'}
                            </button>
                        </Show>
                        <button class="detail-action-btn detail-action-delete">
                            {'\u5220\u9664\u4EFB\u52A1'}
                        </button>
                    </div>
                </div>
            </div>
        </Show>
    )
}

function InfoRow(props: { label: string; value: string; copyable?: boolean; copied?: boolean; onCopy?: () => void }) {
    return (
        <div class="detail-info-row">
            <div class="min-w-0 flex-1 overflow-hidden">
                <div class="detail-info-label">{props.label}</div>
                <div class="detail-info-value">{props.value}</div>
            </div>
            <Show when={props.copyable}>
                <button
                    class="icon-btn-sm"
                    style={{ 'flex-shrink': 0, width: '24px', height: '24px' }}
                    onClick={() => props.onCopy?.()}
                    title={props.copied ? '\u5DF2\u590D\u5236' : '\u590D\u5236'}
                >
                    <Show when={props.copied} fallback={<CopyIcon />}>
                        <span style={{ color: '#00D4AA', 'font-size': '12px', 'font-weight': 700 }}>&#10003;</span>
                    </Show>
                </button>
            </Show>
        </div>
    )
}
