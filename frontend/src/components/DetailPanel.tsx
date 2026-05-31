import { Show } from 'solid-js'
import { $tasks, $selectedId } from '../stores/downloads'
import { formatSize, formatSpeed, statusText, statusClass } from '../utils/format'
import FragmentGrid from './FragmentGrid'

export default function DetailPanel() {
  const selectedTask = () => {
    const id = $selectedId.get()
    if (!id) return null
    return $tasks.get().find(t => t.id === id) ?? null
  }

  return (
    <Show when={selectedTask()} keyed>
      {(task) => (
        <div class="space-y-0">
          <div class="text-[13px] font-semibold text-text-primary mb-4 truncate">{task.fileName}</div>

          <div class="flex justify-between items-center py-1.5 border-b border-white/6 text-[12px]">
            <span class="text-text-tertiary">状态</span>
            <span class={`text-[11px] font-semibold px-2 py-0.5 rounded-full ${statusClass(task.status)}`}>{statusText(task.status)}</span>
          </div>
          <div class="flex justify-between items-center py-1.5 border-b border-white/6 text-[12px]">
            <span class="text-text-tertiary">大小</span>
            <span class="font-mono text-[11px]">{formatSize(task.fileSize)}</span>
          </div>
          <div class="flex justify-between items-center py-1.5 border-b border-white/6 text-[12px]">
            <span class="text-text-tertiary">已下载</span>
            <span class="font-mono text-[11px]">{formatSize(task.downloaded)}</span>
          </div>
          <div class="flex justify-between items-center py-1.5 border-b border-white/6 text-[12px]">
            <span class="text-text-tertiary">进度</span>
            <span class="font-mono text-[11px]">{task.progress.toFixed(1)}%</span>
          </div>
          <Show when={task.speed > 0}>
            <div class="flex justify-between items-center py-1.5 border-b border-white/6 text-[12px]">
              <span class="text-text-tertiary">速度</span>
              <span class="font-mono text-[11px] text-accent">{formatSpeed(task.speed)}</span>
            </div>
          </Show>
          <div class="flex justify-between items-center py-1.5 border-b border-white/6 text-[12px]">
            <span class="text-text-tertiary">分片</span>
            <span class="font-mono text-[11px]">{task.fragmentsDone} / {task.fragmentsTotal}</span>
          </div>
          <div class="flex justify-between items-center py-1.5 border-b border-white/6 text-[12px]">
            <span class="text-text-tertiary">协议</span>
            <span class="font-mono text-[11px]">{(() => { try { return new URL(task.url).protocol.replace(':', '').toUpperCase() } catch { return 'UNKNOWN' } })()}</span>
          </div>

          <Show when={task.fragmentsTotal > 0}>
            <div class="mt-3">
              <FragmentGrid total={task.fragmentsTotal} done={task.fragmentsDone} status={task.status} />
            </div>
          </Show>
        </div>
      )}
    </Show>
  )
}