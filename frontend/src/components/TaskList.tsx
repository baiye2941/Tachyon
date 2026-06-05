import { For, Show } from 'solid-js'
import { $filteredTasks, $selectedId, setSelectedId } from '../stores/downloads'
import { toggleSelection, isSelected } from '../stores/selection'
import { api } from '../api/invoke'
import { addToast } from '../stores/toast'
import DownloadCard from './DownloadCard'
import { Icon } from '../utils/icons'

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
  } catch (e) {
    addToast('删除任务失败: ' + String(e), 'error')
  }
}

export default function TaskList() {
  return (
    <Show
      when={$filteredTasks.get().length > 0}
      fallback={
        <div class="flex flex-col items-center justify-center gap-2 py-16 text-text-tertiary">
          <Icon name="arrow-down-tray" class="w-12 h-12 text-text-tertiary mx-auto" />
          <div class="text-[14px] font-medium text-text-secondary">暂无下载任务</div>
          <div class="text-[12px] text-text-tertiary mt-1">粘贴 URL 开始下载，或拖拽文件到窗口</div>
        </div>
      }
    >
      <div class="flex flex-col" role="list" aria-label="下载任务列表">
        <For each={$filteredTasks.get()}>
          {(task) => (
            <DownloadCard
              task={task}
              selected={isSelected(task.id)}
              onSelect={(id) => setSelectedId(id)}
              onToggleSelect={(id) => toggleSelection(id)}
              onPause={handlePause}
              onResume={handleResume}
              onCancel={handleCancel}
              onDelete={handleDelete}
            />
          )}
        </For>
      </div>
    </Show>
  )
}
