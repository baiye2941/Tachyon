import { For, Show, createMemo, createSignal, onCleanup } from 'solid-js'
import { THREAD_COLORS } from '../utils/format'

interface ChunkMatrixProps {
    fragmentsTotal: number
    fragmentsDone: number
    progress: number
}

export default function ChunkMatrix(props: ChunkMatrixProps) {
    const chunksPerRow = 25
    const [tooltipIndex, setTooltipIndex] = createSignal<number | null>(null)
    const [cursorPos, setCursorPos] = createSignal({ x: 0, y: 0 })
    let tooltipTimer: number | null = null
    let gridRef: HTMLDivElement | undefined

    const showTooltip = (index: number) => {
        if (tooltipTimer !== null) clearTimeout(tooltipTimer)
        tooltipTimer = window.setTimeout(() => {
            setTooltipIndex(index)
            tooltipTimer = null
        }, 150)
    }

    const hideTooltip = () => {
        if (tooltipTimer !== null) {
            clearTimeout(tooltipTimer)
            tooltipTimer = null
        }
        setTooltipIndex(null)
    }

    const handleMouseMove = (e: MouseEvent) => {
        if (!gridRef) return
        const rect = gridRef.getBoundingClientRect()
        setCursorPos({ x: e.clientX - rect.left, y: e.clientY - rect.top })
    }

    onCleanup(() => {
        if (tooltipTimer !== null) clearTimeout(tooltipTimer)
    })

    const chunks = createMemo(() => {
        const total = props.fragmentsTotal
        const done = props.fragmentsDone
        const progress = props.progress
        return Array.from({ length: total }, (_, i) => {
            const isDone = i < done
            const isDownloading = i === done && progress < 1
            const threadId = i % THREAD_COLORS.length
            return {
                index: i,
                isDone,
                isDownloading,
                threadId,
                color: THREAD_COLORS[threadId],
            }
        })
    })

    const statusLabel = (chunk: { isDone: boolean; isDownloading: boolean }) => {
        if (chunk.isDone) return '\u5DF2\u5B8C\u6210'
        if (chunk.isDownloading) return '\u4E0B\u8F7D\u4E2D'
        return '\u7B49\u5F85\u4E2D'
    }

    const tooltipInfo = createMemo(() => {
        const idx = tooltipIndex()
        if (idx === null) return null
        const chunkList = chunks()
        const chunk = chunkList[idx]
        if (!chunk) return null
        const pos = cursorPos()
        return {
            idx,
            chunk,
            total: chunkList.length,
            left: Math.min(pos.x + 12, (gridRef?.clientWidth || 360) - 160),
            top: pos.y - 60,
        }
    })

    return (
        <div>
            <div class="section-label" style={{ 'margin-bottom': '12px' }}>
                {'\u4E0B\u8F7D\u5206\u5E03'}
            </div>

            <div
                class="glass"
                style={{
                    padding: '16px',
                    'border-radius': '12px',
                    position: 'relative',
                }}
                ref={gridRef}
                onMouseMove={handleMouseMove}
            >
                <div
                    class="flex flex-wrap"
                    style={{ gap: '3px' }}
                >
                    <For each={chunks()}>
                        {(chunk) => (
                            <div
                                class="chunk-cell"
                                style={{
                                    width: '14px',
                                    height: '14px',
                                    'border-radius': '3px',
                                    background: chunk.isDone
                                        ? chunk.color
                                        : chunk.isDownloading
                                            ? chunk.color
                                            : '#1A1A25',
                                    'box-shadow': chunk.isDone
                                        ? `0 0 4px ${chunk.color}66`
                                        : 'none',
                                    animation: chunk.isDownloading
                                        ? `chunk-appear 200ms cubic-bezier(0.34, 1.56, 0.64, 1) forwards, chunk-pulse 1.5s ease-in-out infinite`
                                        : `chunk-appear 200ms cubic-bezier(0.34, 1.56, 0.64, 1) forwards`,
                                    'animation-delay': chunk.isDownloading
                                        ? `${chunk.index * 5}ms, ${(chunk.index % chunksPerRow) * 0.05 + Math.floor(chunk.index / chunksPerRow) * 0.1}s`
                                        : `${chunk.index * 5}ms`,
                                    opacity: 0,
                                }}
                                onMouseEnter={() => showTooltip(chunk.index)}
                                onMouseLeave={() => hideTooltip()}
                            />
                        )}
                    </For>
                </div>

                {/* Tooltip */}
                <Show when={tooltipInfo()} keyed>
                    {(tooltip) => (
                        <div
                            class="chunk-tooltip"
                            style={{
                                left: `${tooltip.left}px`,
                                top: `${tooltip.top}px`,
                            }}
                        >
                            <div
                                class="chunk-tooltip-dot"
                                style={{ background: tooltip.chunk.isDone || tooltip.chunk.isDownloading ? tooltip.chunk.color : '#1A1A25' }}
                            />
                            <div class="chunk-tooltip-body">
                                <div class="chunk-tooltip-title">
                                    {'\u5206\u7247'} #{tooltip.idx + 1}
                                    <span class="chunk-tooltip-subtitle">/ {tooltip.total}</span>
                                </div>
                                <div class="chunk-tooltip-meta">
                                    <span style={{ color: tooltip.chunk.isDone ? '#00D4AA' : tooltip.chunk.isDownloading ? '#00B4D8' : '#6B7280' }}>
                                        {statusLabel(tooltip.chunk)}
                                    </span>
                                    <span class="chunk-tooltip-sep">{'\u00B7'}</span>
                                    <span style={{ color: tooltip.chunk.color }}>{'T'}{tooltip.chunk.threadId + 1}</span>
                                </div>
                            </div>
                        </div>
                    )}
                </Show>

                {/* Legend */}
                <div class="flex items-center gap-4" style={{ 'margin-top': '12px' }}>
                    <LegendItem color="#00D4AA" label={'\u5DF2\u5B8C\u6210'} />
                    <LegendItem color="#1A1A25" label={'\u672A\u5F00\u59CB'} />
                    <LegendItem color="#00D4AA" label={'\u4E0B\u8F7D\u4E2D'} pulse />
                    <LegendItem color="#EF4444" label={'\u9519\u8BEF'} />
                </div>
            </div>
        </div>
    )
}

function LegendItem(props: { color: string; label: string; pulse?: boolean }) {
    return (
        <div class="flex items-center gap-1.5">
            <div
                style={{
                    width: '8px',
                    height: '8px',
                    'border-radius': '2px',
                    background: props.color,
                    animation: props.pulse ? 'chunk-pulse 1.5s ease-in-out infinite' : 'none',
                }}
            />
            <span style={{ 'font-size': '11px', color: '#6B7280' }}>{props.label}</span>
        </div>
    )
}
