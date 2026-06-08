import { createMemo } from 'solid-js'
import { ArrowDownIcon, ChevronDownIcon } from './icons'
import Sparkline from './Sparkline'
import { formatSpeed } from '../utils/format'

interface StatusBarProps {
  isIdle: boolean
  totalSpeed: number
  activeCount: number
  pausedCount: number
  totalCount: number
}

function generateSpeedHistory(currentSpeed: number): number[] {
  if (currentSpeed === 0) return []
  const points: number[] = []
  for (let i = 0; i < 30; i++) {
    const variance = (Math.random() - 0.5) * currentSpeed * 0.5
    points.push(Math.max(0, currentSpeed + variance))
  }
  return points
}

export default function StatusBar(props: StatusBarProps) {
  const speedHistory = createMemo(() => generateSpeedHistory(props.totalSpeed))

  return (
    <div
      class="flex items-center justify-between flex-shrink-0"
      style={{
        height: '28px',
        background: '#12121A',
        'border-top': '1px solid rgba(255,255,255,0.05)',
        padding: '0 12px',
        'font-size': '12px',
      }}
    >
      {/* Left */}
      <div class="flex items-center gap-2">
        <div
          style={{
            width: '8px',
            height: '8px',
            'border-radius': '50%',
            background: props.isIdle ? '#6B7280' : '#00D4AA',
          }}
          class={props.isIdle ? '' : "status-indicator-active"}
        />
        <span style={{ color: '#A0A0B0' }}>
          {props.isIdle ? '空闲' : '下载中'}
        </span>
        <span
          style={{
            color: props.isIdle ? '#A0A0B0' : '#00D4AA',
            'font-family': "'Geist Mono', monospace",
            display: 'flex',
            'align-items': 'center',
            gap: '4px',
            transition: 'color 300ms ease',
          }}
        >
          <ArrowDownIcon />
          {formatSpeed(props.totalSpeed)}
        </span>
        <Sparkline data={speedHistory()} width={80} height={16} />

        <span style={{ color: '#6B7280' }}>
          {props.activeCount} 活跃 · {props.pausedCount} 暂停 · {props.totalCount} 总计
        </span>
      </div>

      {/* Right */}
      <div class="flex items-center gap-3">
        <button
          class="flex items-center gap-1 hover-light"
          style={{ color: '#6B7280' }}
        >
          <span>无限制</span>
          <ChevronDownIcon />
        </button>
        <button
          class="hover-light"
          style={{ color: '#6B7280' }}
        >
          反馈
        </button>
      </div>
    </div>
  )
}
