import { createMemo, For, Show } from 'solid-js'
import * as speedHistory from '../stores/speedHistory'
import { formatSpeed } from '../utils/format'

function generateWaveformPath(data: readonly number[], width: number, height: number): string {
  if (data.length === 0) {
    return `M0,${height} L${width},${height}`
  }

  const max = Math.max(...data, 1)
  const stepX = width / (data.length - 1 || 1)

  let d = `M0,${height - (data[0] / max) * height}`
  for (let i = 1; i < data.length; i++) {
    const x = i * stepX
    const y = height - (data[i] / max) * height
    d += ` L${x},${y}`
  }

  return d
}

function generateWaveformArea(data: readonly number[], width: number, height: number): string {
  if (data.length === 0) {
    return `M0,${height} L${width},${height} L${width},0 L0,0 Z`
  }

  const linePath = generateWaveformPath(data, width, height)
  return `${linePath} L${width},${height} L0,${height} Z`
}

export default function SpeedDashboard() {
  const currentSpeed = createMemo(() => speedHistory.getCurrentSpeed())
  const activeTasks = createMemo(() => speedHistory.getActiveTasks())
  const avgSpeed = createMemo(() => speedHistory.getAverageSpeed())
  const peakSpeed = createMemo(() => speedHistory.getPeakSpeed())
  const history = createMemo(() => speedHistory.getHistory())

  const waveformPath = createMemo(() => {
    return generateWaveformPath(history(), 140, 24)
  })

  const waveformArea = createMemo(() => {
    return generateWaveformArea(history(), 140, 24)
  })

  const stats = () => [
    { label: '总速度', value: formatSpeed(currentSpeed()), testId: 'stat-total-speed' },
    { label: '活跃', value: String(activeTasks()), testId: 'stat-active-tasks' },
    { label: '均速', value: formatSpeed(avgSpeed()), testId: 'stat-avg-speed' },
    { label: '峰值', value: formatSpeed(peakSpeed()), testId: 'stat-peak-speed' },
  ]

  return (
    <div
      data-testid="speed-dashboard"
      class="flex items-center gap-3 px-3 py-1 border-b border-border"
    >
      <For each={stats()}>
        {(stat, index) => (
          <>
            <div class="flex items-baseline gap-1.5">
              <span class="w-1 h-1 rounded-full bg-accent shrink-0" />
              <span class="text-[10px] text-text-tertiary">{stat.label}</span>
              <span
                data-testid={stat.testId}
                class="text-[12px] font-mono font-semibold text-text-primary"
              >
                {stat.value}
              </span>
            </div>
            <Show when={index() < stats().length - 1}>
              <div class="w-px h-3 bg-border-strong" />
            </Show>
          </>
        )}
      </For>

      <svg
        data-testid="speed-waveform"
        viewBox="0 0 140 24"
        class="h-[24px] w-[140px] shrink-0 ml-auto"
        preserveAspectRatio="none"
      >
        <defs>
          <linearGradient id="waveform-gradient" x1="0%" y1="0%" x2="0%" y2="100%">
            <stop offset="0%" stop-color="currentColor" stop-opacity="0.15" />
            <stop offset="100%" stop-color="currentColor" stop-opacity="0" />
          </linearGradient>
        </defs>
        {/* 背景网格线 */}
        <line x1="0" y1="6" x2="140" y2="6" stroke="rgba(255,255,255,0.04)" stroke-width="0.5" stroke-dasharray="2 2" />
        <line x1="0" y1="12" x2="140" y2="12" stroke="rgba(255,255,255,0.04)" stroke-width="0.5" stroke-dasharray="2 2" />
        <line x1="0" y1="18" x2="140" y2="18" stroke="rgba(255,255,255,0.04)" stroke-width="0.5" stroke-dasharray="2 2" />
        <path
          d={waveformArea()}
          fill="url(#waveform-gradient)"
          class="text-accent"
        />
        <path
          d={waveformPath()}
          fill="none"
          stroke="currentColor"
          stroke-width="1"
          class="text-accent"
          vector-effect="non-scaling-stroke"
        />
      </svg>
    </div>
  )
}
