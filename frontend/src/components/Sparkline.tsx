import { createMemo, Show } from 'solid-js'

interface SparklineProps {
  data: number[]
  width?: number
  height?: number
}

export default function Sparkline(props: SparklineProps) {
  const width = () => props.width || 80
  const height = () => props.height || 16

  const pathD = createMemo(() => {
    const data = props.data.length > 0 ? props.data : [0]
    const maxVal = Math.max(...data, 1)
    const w = width()
    const h = height()

    const points = data.map((val, i) => {
      const x = (i / (data.length - 1)) * w
      const y = h - (val / maxVal) * h
      return [x, y] as const
    })

    if (points.length < 2) return { line: '', area: '' }

    const first = points[0]!
    let line = `M ${first[0]} ${first[1]}`
    for (let i = 1; i < points.length; i++) {
      const pt = points[i]!
      line += ` L ${pt[0]} ${pt[1]}`
    }

    const area = `${line} L ${w} ${h} L 0 ${h} Z`
    return { line, area }
  })

  // 历史峰值点:无数据/全 0 时不渲染(与 pathD 同一套坐标换算)
  const peakPoint = createMemo(() => {
    const data = props.data
    if (data.length < 2) return null
    const peak = Math.max(...data)
    if (peak <= 0) return null
    const maxVal = Math.max(peak, 1)
    // 多峰值并列时 indexOf 取最左(最早)峰值,直观对应「首次达到峰值」
    const peakIndex = data.indexOf(peak)
    return {
      x: (peakIndex / (data.length - 1)) * width(),
      y: height() - (peak / maxVal) * height(),
    }
  })

  return (
    <svg
      width={width()}
      height={height()}
      viewBox={`0 0 ${width()} ${height()}`}
      preserveAspectRatio="none"
      style={{ overflow: 'visible', opacity: props.data.length > 1 ? 1 : 0, transition: 'opacity 200ms ease' }}
    >
      <path
        d={pathD().area}
        fill="var(--color-speed-soft)"
        stroke="none"
      />
      <path
        d={pathD().line}
        fill="none"
        stroke="var(--color-speed-active)"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
      <Show when={peakPoint()}>
        {(pt) => (
          <circle class="sparkline-peak" cx={pt().x} cy={pt().y} r={2} />
        )}
      </Show>
    </svg>
  )
}
