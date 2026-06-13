import { createMemo } from 'solid-js'

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
        fill="rgba(0, 212, 170, 0.1)"
        stroke="none"
      />
      <path
        d={pathD().line}
        fill="none"
        stroke="#00D4AA"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  )
}
