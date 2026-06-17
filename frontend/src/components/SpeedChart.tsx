import { createMemo, onMount, onCleanup, createSignal } from 'solid-js'
import type { TaskInfo } from '../types'
import { formatSpeed } from '../utils/format'
import { getHistory, getPeakSpeed, getAverageSpeed } from '../stores/speedHistory'
import { tr } from '../i18n'

interface SpeedChartProps {
  task: TaskInfo
}

const MAX_POINTS = 60

export default function SpeedChart(props: SpeedChartProps) {
  // 1Hz UI 采样:数据源已是 500ms 推送,UI 1Hz 足够,降低 50% path 重建
  const [tick, setTick] = createSignal(0)
  let timerId: number | undefined

  onMount(() => {
    const loop = () => {
      setTick(t => t + 1)
      timerId = window.setTimeout(loop, 1000)
    }
    timerId = window.setTimeout(loop, 1000)
  })

  onCleanup(() => {
    if (timerId !== undefined) clearTimeout(timerId)
  })

  // 真实速度历史:来自 stores/speedHistory 的环形缓冲区(Float64Array, O(1) 写入)
  const data = createMemo(() => {
    void tick() // 依赖 tick 触发重算
    return getHistory()
  })

  const pathD = createMemo(() => {
    const history = data()
    const sample = history.length > 0 ? history : [props.task.speed]
    // 降采样:最多 MAX_POINTS 个点,避免 path 字符串过长
    const points = sample.length > MAX_POINTS
      ? sample.slice(-MAX_POINTS)
      : sample
    const maxVal = Math.max(...points, 1)
    const width = 320
    const height = 120
    const padding = 4

    const coords = points.map((val, i) => {
      const x = (i / (MAX_POINTS - 1)) * width
      const y = height - padding - (val / maxVal) * (height - padding * 2)
      return [x, y] as const
    })

    if (coords.length < 2) return { line: '', area: '' }

    // 平滑曲线(简单贝塞尔)
    const first = coords[0]!
    let line = `M ${first[0]} ${first[1]}`
    for (let i = 1; i < coords.length; i++) {
      const prev = coords[i - 1]!
      const curr = coords[i]!
      const cpx1 = prev[0] + (curr[0] - prev[0]) * 0.5
      const cpx2 = prev[0] + (curr[0] - prev[0]) * 0.5
      line += ` C ${cpx1} ${prev[1]}, ${cpx2} ${curr[1]}, ${curr[0]} ${curr[1]}`
    }

    const area = `${line} L ${width} ${height} L 0 ${height} Z`
    return { line, area }
  })

  const stats = createMemo(() => ({
    peak: getPeakSpeed(),
    avg: getAverageSpeed(),
  }))

  const hasData = createMemo(() => data().length > 0)

  return (
    <div
      class="glass"
      style={{
        padding: '16px',
        'border-radius': '12px',
      }}
    >
      <div
        style={{
          'font-size': '12px',
          'font-weight': 600,
          color: 'var(--color-text-tertiary)',
          'text-transform': 'uppercase',
          'letter-spacing': '0.5px',
          'margin-bottom': '12px',
        }}
      >
        {tr('speedChart.title')}
      </div>

      <svg
        width="100%"
        height="120"
        viewBox="0 0 320 120"
        preserveAspectRatio="none"
        style={{ overflow: 'visible' }}
      >
        <defs>
          <linearGradient id="speed-area-gradient" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stop-color="var(--color-speed-soft)" />
            <stop offset="100%" stop-color="transparent" />
          </linearGradient>
          <linearGradient id="speed-line-gradient" x1="0" y1="0" x2="1" y2="0">
            <stop offset="0%" stop-color="var(--color-accent-primary)" />
            <stop offset="100%" stop-color="var(--color-speed-active)" />
          </linearGradient>
        </defs>

        {hasData() ? (
          <>
            <path
              d={pathD().area}
              fill="url(#speed-area-gradient)"
              stroke="none"
            />
            <path
              d={pathD().line}
              fill="none"
              stroke="url(#speed-line-gradient)"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </>
        ) : (
          <text
            x="160"
            y="64"
            text-anchor="middle"
            fill="var(--color-text-tertiary)"
            font-size="12"
            font-family="var(--font-mono)"
          >
            {tr('speedChart.waiting')}
          </text>
        )}
      </svg>

      <div class="flex items-center justify-between" style={{ 'margin-top': '12px' }}>
        <span
          class="mono"
          style={{
            'font-size': '14px',
            color: 'var(--color-text-secondary)',
          }}
        >
          {tr('speedChart.peak')}
          <span style={{ color: 'var(--color-speed-active)' }}>{formatSpeed(stats().peak)}</span>
        </span>
        <span
          class="mono"
          style={{
            'font-size': '14px',
            color: 'var(--color-text-secondary)',
          }}
        >
          {tr('speedChart.average')}
          {formatSpeed(stats().avg)}
        </span>
      </div>
    </div>
  )
}
