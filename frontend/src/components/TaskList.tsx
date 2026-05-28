import { For, Show } from 'solid-js'
import { useStore } from '@nanostores/solid'
import { $tasks, $activeTasks, $completedTasks, $selectedId } from '../stores/downloads'
import { api } from '../api/invoke'
import DownloadCard from './DownloadCard'

async function refreshTasks() {
  try {
    const list = await api.getTaskList()
    $tasks.set(list)
  } catch {
    // Tauri 未就绪时静默忽略
  }
}

async function handlePause(id: string) {
  await api.pauseTask(id)
  await refreshTasks()
}

async function handleResume(id: string) {
  await api.resumeTask(id)
  await refreshTasks()
}

async function handleCancel(id: string) {
  await api.cancelTask(id)
  await refreshTasks()
}

async function handleDelete(id: string) {
  await api.deleteTask(id)
  const selected = $selectedId.get()
  if (selected === id) $selectedId.set(null)
  await refreshTasks()
}

export default function TaskList() {
  const tasks = useStore($tasks)
  const activeTasks = useStore($activeTasks)
  const completedTasks = useStore($completedTasks)
  const selectedId = useStore($selectedId)

  return (
    <Show
      when={tasks().length > 0}
      fallback={
        <div class="empty-state">
          <svg class="empty-icon" viewBox="0 0 24 24">
            <path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4M7 10l5 5 5-5M12 15V3" />
          </svg>
          <div class="empty-text">暂无下载任务</div>
          <div class="empty-hint">在顶部输入链接开始下载</div>
        </div>
      }
    >
      <div>
        <Show when={activeTasks().length > 0}>
          <div class="section-title">活跃</div>
          <For each={activeTasks()}>
            {(task) => (
              <DownloadCard
                task={task}
                selected={selectedId() === task.id}
                onSelect={(id) => $selectedId.set(id)}
                onPause={handlePause}
                onResume={handleResume}
                onCancel={handleCancel}
                onDelete={handleDelete}
              />
            )}
          </For>
        </Show>

        <Show when={completedTasks().length > 0}>
          <div class="section-title" style={{ 'margin-top': activeTasks().length > 0 ? '20px' : '0' }}>
            已完成
          </div>
          <For each={completedTasks()}>
            {(task) => (
              <DownloadCard
                task={task}
                selected={selectedId() === task.id}
                onSelect={(id) => $selectedId.set(id)}
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
