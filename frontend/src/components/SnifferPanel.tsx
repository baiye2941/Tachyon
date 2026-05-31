import { For, Show, onMount } from 'solid-js'
import { $snifferResources, $snifferActive } from '../stores/sniffer'
import { $tasks, setTasks } from '../stores/downloads'
import { api } from '../api/invoke'
import { formatSize } from '../utils/format'
import Toggle from './Toggle'
import type { SnifferResourceType } from '../types'

const TYPE_LABEL: Record<SnifferResourceType, string> = {
  video: 'VIDEO',
  audio: 'AUDIO',
  document: 'DOC',
  archive: 'ARC',
  executable: 'EXE',
  image: 'IMG',
  other: 'OTHER',
}

const TYPE_COLORS: Record<SnifferResourceType, string> = {
  video: 'text-error',
  audio: 'text-success',
  document: 'text-accent',
  archive: 'text-warning',
  executable: 'text-aurora',
  image: 'text-accent',
  other: 'text-text-tertiary',
}

async function fetchResources() {
  try {
    const list = await api.getSnifferResources()
    $snifferResources.set(list)
  } catch (e) {
    console.error('获取嗅探资源失败:', e)
  }
}

async function handleDownload(url: string) {
  try {
    await api.createTask(url)
    const list = await api.getTaskList()
    setTasks(list)
  } catch (e) {
    console.error('下载嗅探资源失败:', e)
  }
}

export default function SnifferPanel() {
  onMount(() => {
    fetchResources()
  })

  return (
    <div class="space-y-4">
      <div class="flex items-center gap-3">
        <h2 class="text-[15px] font-semibold text-text-primary">资源嗅探</h2>
        <span class={`text-[11px] font-medium ${$snifferActive.get() ? 'text-success' : 'text-text-tertiary'}`}>
          {$snifferActive.get() ? '监听中' : '已停止'}
        </span>
        <Toggle
          checked={$snifferActive.get()}
          ariaLabel="嗅探开关"
          onChange={(val) => $snifferActive.set(val)}
        />
        <button
          class="ml-auto px-3 py-1.5 text-[12px] font-semibold text-text-secondary border border-white/6 rounded hover:text-text-primary hover:border-white/12 transition-colors duration-150"
          onClick={fetchResources}
        >
          刷新
        </button>
      </div>

      <Show
        when={$snifferResources.get().length > 0}
        fallback={
          <div class="flex flex-col items-center justify-center gap-3 py-10 text-text-tertiary">
            <svg class="w-10 h-10 opacity-40" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1">
              <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
              <circle cx="12" cy="12" r="3" />
            </svg>
            <div class="text-[13px] text-text-secondary">暂未嗅探到资源</div>
            <div class="text-[11px] text-text-tertiary">浏览网页时自动捕获可下载资源</div>
          </div>
        }
      >
        <div class="space-y-0">
          <For each={$snifferResources.get()}>
            {(res) => (
              <div class="flex items-center gap-3 py-2.5 border-b border-white/6 hover:border-white/12 transition-colors duration-150">
                <span class={`text-[10px] font-mono font-semibold uppercase min-w-[48px] text-center px-1.5 py-0.5 rounded ${TYPE_COLORS[res.type]}`}>
                  {TYPE_LABEL[res.type]}
                </span>
                <div class="flex-1 min-w-0">
                  <div class="text-[13px] font-medium text-text-primary truncate">{res.name}</div>
                  <div class="text-[11px] text-text-tertiary font-mono truncate">{res.url}</div>
                </div>
                <span class="text-[11px] font-mono text-text-secondary min-w-[64px] text-right shrink-0">
                  {formatSize(res.size)}
                </span>
                <button
                  class="px-2.5 py-1 text-[11px] font-semibold bg-accent text-canvas rounded hover:opacity-85 active:scale-[0.98] transition-all duration-100"
                  onClick={() => handleDownload(res.url)}
                >
                  下载
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  )
}