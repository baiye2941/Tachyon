import { createMemo, onMount } from 'solid-js'
import type { TaskInfo } from '../types'
import { formatSpeed } from '../utils/format'

interface SpeedChartProps {
  task: TaskInfo
}

const MAX_POINTS = 120 // 2 minutes at 1 sample/sec

export default function SpeedChart(props: SpeedChartProps) {
  let svgRef: SVGSVGElement | undefined
  let dataPoints: number[] = []

  // Initialize with some simulated historical data for demo
  onMount(() => {
    const base = props.task.speed
    for (let i = 0; i < 60; i++) {
      const variance = (Math.random() - 0.5) * base * 0.4
      dataPoints.push(Math.max(0, base + variance))
    }
  })

  const pathD = createMemo(() => {
    const data = dataPoints.length > 0 ? dataPoints : [props.task.speed]
    const maxVal = Math.max(...data, 1)
    const width = 320
    const height = 120
    const padding = 4

    const points = data.map((val, i) => {
      const x = (i / (MAX_POINTS - 1)) * width
      const y = height - padding - (val / maxVal) * (height - padding * 2)
      return [x, y] as const
    })

    if (points.length < 2) return { line: '', area: '' }

    // Build smooth line with simple bezier curves
    let line = `M ${points[0][0]} ${points[0][1]}`
    for (let i = 1; i < points.length; i++) {
      const prev = points[i - 1]
      const curr = points[i]
      const cpx1 = prev[0] + (curr[0] - prev[0]) * 0.5
      const cpx2 = prev[0] + (curr[0] - prev[0]) * 0.5
      line += ` C ${cpx1} ${prev[1]}, ${cpx2} ${curr[1]}, ${curr[0]} ${curr[1]}`
    }

    const area = `${line} L ${width} ${height} L 0 ${height} Z`
    return { line, area }
  })

  const stats = createMemo(() => {
    const data = dataPoints.length > 0 ? dataPoints : [props.task.speed]
    const peak = Math.max(...data)
    const avg = data.reduce((a, b) => a + b, 0) / data.length
    return { peak, avg }
  })

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
          color: '#6B7280',
          'text-transform': 'uppercase',
          'letter-spacing': '0.5px',
          'margin-bottom': '12px',
        }}
      >
        速度趋势
      </div>

      <svg
        ref={svgRef}
        width="100%"
        height="120"
        viewBox="0 0 320 120"
        preserveAspectRatio="none"
        style={{ overflow: 'visible' }}
      >
        <defs>
          <linearGradient id="area-gradient" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stop-color="rgba(0, 212, 170, 0.2)" />
            <stop offset="100%" stop-color="rgba(0, 212, 170, 0)" />
          </linearGradient>
          <linearGradient id="line-gradient" x1="0" y1="0" x2="1" y2="0">
            <stop offset="0%" stop-color="#00D4AA" />
            <stop offset="100%" stop-color="#00B4D8" />
          </linearGradient>
        </defs>

        <path
          d={pathD().area}
          fill="url(#area-gradient)"
          stroke="none"
        />
        <path
          d={pathD().line}
          fill="none"
          stroke="url(#line-gradient)"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>

      <div class="flex items-center justify-between" style={{ 'margin-top': '12px' }}>
        <span style={{ 'font-size': '14px', color: '#A0A0B0', 'font-family': "'Geist Mono', monospace" }}>
          峰值: <span style={{ color: '#00D4AA' }}>{formatSpeed(stats().peak)}</span>
        </span>
        <span style={{ 'font-size': '14px', color: '#A0A0B0', 'font-family': "'Geist Mono', monospace" }}>
          平均: {formatSpeed(stats().avg)}
        </span>
      </div>
    </div>
  )
}
