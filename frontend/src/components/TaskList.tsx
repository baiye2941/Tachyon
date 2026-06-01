import { For, Show } from 'solid-js'
import { $tasks, $activeTasks, $completedTasks, $selectedId, setTasks, setSelectedId } from '../stores/downloads'
import { api } from '../api/invoke'
import { addToast } from '../stores/toast'
import DownloadCard from './DownloadCard'

async function refreshTasks() {
  try {
    const list = await api.getTaskList()
    setTasks(list)
  } catch (e) {
    addToast('刷新任务列表失败: ' + String(e), 'error')
  }
}

async function handlePause(id: string) {
  try {
    await api.pauseTask(id)
  } catch (e) {
    addToast('暂停任务失败: ' + String(e), 'error')
  }
}

async function handleResume(id: string) {
  try {
    await api.resumeTask(id)
  } catch (e) {
    addToast('恢复任务失败: ' + String(e), 'error')
  }
}

async function handleCancel(id: string) {
  try {
    await api.cancelTask(id)
  } catch (e) {
    addToast('取消任务失败: ' + String(e), 'error')
  }
}

async function handleDelete(id: string) {
  try {
    await api.deleteTask(id)
    const selected = $selectedId.get()
    if (selected === id) setSelectedId(null)
    await refreshTasks()
  } catch (e) {
    addToast('删除任务失败: ' + String(e), 'error')
  }
}

export default function TaskList() {
  return (
    <Show
      when={$tasks.get().length > 0}
      fallback={
        <div class="flex flex-col items-center justify-center gap-3 py-10 text-text-tertiary">
          <svg class="w-10 h-10 opacity-40" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1">
            <path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4M7 10l5 5 5-5M12 15V3" />
          </svg>
          <div class="text-[13px] text-text-secondary">暂无下载任务</div>
          <div class="text-[11px] text-text-tertiary">在顶部输入链接开始下载</div>
        </div>
      }
    >
      <div>
        <Show when={$activeTasks.get().length > 0}>
          <div class="text-[11px] font-semibold text-text-tertiary uppercase tracking-wider mb-2">活跃</div>
          <For each={$activeTasks.get()}>
            {(task) => (
              <DownloadCard
                task={task}
                selected={$selectedId.get() === task.id}
                onSelect={(id) => setSelectedId(id)}
                onPause={handlePause}
                onResume={handleResume}
                onCancel={handleCancel}
                onDelete={handleDelete}
              />
            )}
          </For>
        </Show>

        <Show when={$completedTasks.get().length > 0}>
          <div class="text-[11px] font-semibold text-text-tertiary uppercase tracking-wider mb-2 mt-5">已完成</div>
          <For each={$completedTasks.get()}>
            {(task) => (
              <DownloadCard
                task={task}
                selected={$selectedId.get() === task.id}
                onSelect={(id) => setSelectedId(id)}
                onPause={handlePause}
                onResume={handleResume}
                onCancel={handleCancel}
                onDelete={handleDelete}
              />
            )}
          </For>
        </Show>
      </div>
    </Show>
  )
}