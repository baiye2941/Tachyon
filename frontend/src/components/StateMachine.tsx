import { For, createMemo } from 'solid-js'
import type { DownloadStatus } from '../types'
import {
  STATE_NODES,
  STATE_EDGES,
  getVisitedStates,
  getNodeStyle,
  getLabelStyle,
} from '../utils/stateMachine'

interface StateMachineProps {
  currentStatus: DownloadStatus
}

export default function StateMachine(props: StateMachineProps) {
  const visitedStates = createMemo(() => getVisitedStates(props.currentStatus))

  const isVisited = (status: DownloadStatus) => visitedStates().includes(status)

  return (
    <div class="w-full" data-testid="state-machine">
      <svg viewBox="0 0 440 80" class="w-full h-auto">
        <defs>
          <marker
            id="arrowhead"
            markerWidth="10"
            markerHeight="7"
            refX="9"
            refY="3.5"
            orient="auto"
          >
            <polygon points="0 0, 10 3.5, 0 7" fill="rgba(255,255,255,0.3)" />
          </marker>
          <marker
            id="arrowhead-active"
            markerWidth="10"
            markerHeight="7"
            refX="9"
            refY="3.5"
            orient="auto"
          >
            <polygon points="0 0, 10 3.5, 0 7" fill="#00d2ff" />
          </marker>
        </defs>

        {/* Edges */}
        <For each={STATE_EDGES}>
          {(edge) => {
            const fromNode = STATE_NODES.find((n) => n.id === edge.from)
            const toNode = STATE_NODES.find((n) => n.id === edge.to)
            if (!fromNode || !toNode) return null

            const isActive = isVisited(edge.from) && isVisited(edge.to)

            return (
              <line
                x1={fromNode.x}
                y1={fromNode.y}
                x2={toNode.x - 24}
                y2={toNode.y}
                stroke={isActive ? '#00d2ff' : 'rgba(255,255,255,0.15)'}
                stroke-width="1.5"
                stroke-dasharray={isActive ? '0' : '4 3'}
                marker-end={isActive ? 'url(#arrowhead-active)' : 'url(#arrowhead)'}
                data-testid={`edge-${edge.from}-${edge.to}`}
              />
            )
          }}
        </For>

        {/* Nodes */}
        <For each={STATE_NODES}>
          {(node) => {
            const style = getNodeStyle(node.id, props.currentStatus)
            const labelStyle = getLabelStyle(node.id, props.currentStatus)

            return (
              <g data-testid={`node-${node.id}`}>
                {/* Node circle */}
                <circle
                  cx={node.x}
                  cy={node.y}
                  r={style.radius}
                  fill={style.fill}
                  stroke={style.stroke}
                  stroke-width={style.strokeWidth}
                  class={style.hasPulse ? 'animate-pulse' : ''}
                />
                {/* Label */}
                <text
                  x={node.x}
                  y={node.y + 28}
                  text-anchor="middle"
                  fill={labelStyle.fill}
                  font-size="10"
                  font-family="var(--font-sans)"
                >
                  {node.label}
                </text>
              </g>
            )
          }}
        </For>
      </svg>
    </div>
  )
}
