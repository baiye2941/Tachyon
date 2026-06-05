import { Show, type JSX } from 'solid-js'
import { $selectedTask } from '../stores/downloads'
import { formatSize, formatSpeed, statusText, statusClass } from '../utils/format'
import FragmentGrid from './FragmentGrid'
import StateMachine from './StateMachine'

export default function DetailPanel() {
  const task = () => $selectedTask.get()

  return (
    <Show when={task()} keyed>
      {(task) => (
        <div class="flex flex-col h-full">
          <div class="text-[12px] font-semibold truncate px-3 py-2 border-b border-border text-text-primary">
            {task.fileName}
          </div>

          <div class="px-3 py-2 grid grid-cols-2 gap-1.5">
            <InfoRow label="状态">
              <span class={`text-[10px] font-semibold px-1.5 py-0.5 rounded-full ${statusClass(task.status)} ${task.status === 'downloading' ? 'animate-pulse-subtle' : ''}`}>
                {statusText(task.status)}
              </span>
            </InfoRow>
            <InfoRow label="大小" value={formatSize(task.fileSize)} />
            <InfoRow label="已下载" value={formatSize(task.downloaded)} />
            <InfoRow label="进度" value={`${task.progress.toFixed(1)}%`} />
            <Show when={task.speed > 0}>
              <InfoRow label="速度" value={formatSpeed(task.speed)} valueClass="text-accent" />
            </Show>
            <InfoRow label="分片" value={`${task.fragmentsDone} / ${task.fragmentsTotal}`} />
            <InfoRow
              label="协议"
              value={(() => {
                try {
                  return new URL(task.url).protocol.replace(':', '').toUpperCase()
                } catch {
                  return 'UNKNOWN'
                }
              })()}
            />
          </div>

          <div class="px-3 pt-2 border-t border-border">
            <div class="text-[10px] font-semibold text-text-tertiary uppercase tracking-wider mb-1.5">状态流转</div>
            <div class="glass-panel rounded-lg p-3">
              <StateMachine currentStatus={task.status} />
            </div>
          </div>

          <Show when={task.fragmentsTotal > 0}>
            <div class="px-3 pt-2 border-t border-border">
              <div class="text-[10px] font-semibold text-text-tertiary uppercase tracking-wider mb-1.5">分片分布</div>
              <div class="glass-panel rounded-lg p-3">
                <FragmentGrid total={task.fragmentsTotal} done={task.fragmentsDone} status={task.status} />
              </div>
            </div>
          </Show>
        </div>
      )}
    </Show>
  )
}

function InfoRow(props: {
  label: string
  value?: string
  children?: JSX.Element
  valueClass?: string
}) {
  return (
    <div class="glass-panel rounded p-1.5 flex flex-col gap-0.5">
      <span class="text-[11px] text-text-tertiary">{props.label}</span>
      {props.children ?? (
        <span class={`text-[12px] font-mono text-text-primary ${props.valueClass ?? ''}`}>{props.value}</span>
      )}
    </div>
  )
}
