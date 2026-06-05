import { createSignal, Show } from 'solid-js'
import { api } from '../api/invoke'
import { refreshTaskList } from '../stores/downloads'
import { addToast } from '../stores/toast'
import { btnPrimary, inputBase } from '../utils/styles'

export default function Topbar() {
  const [url, setUrl] = createSignal('')
  const [mirrorUrl, setMirrorUrl] = createSignal('')
  const [showMirror, setShowMirror] = createSignal(false)

  async function startDownload() {
    const u = url().trim()
    if (!u) return
    try {
      const mirrors = showMirror() ? [mirrorUrl().trim()].filter(Boolean) : []
      await api.createTask(u, mirrors.length > 0 ? mirrors : undefined)
      setUrl('')
      setMirrorUrl('')
      refreshTaskList()
    } catch (e) {
      addToast(String(e), 'error')
    }
  }

  return (
    <div class="bg-surface border-b border-border px-3 py-2">
      <div class="flex items-center gap-2">
        <input
          type="text"
          value={url()}
          onInput={(e) => setUrl(e.currentTarget.value)}
          onKeyDown={(e) => { if (e.key === 'Enter') startDownload() }}
          placeholder="粘贴下载链接,支持 HTTP/HTTPS"
          aria-label="下载链接输入"
          class={`flex-1 font-mono ${inputBase}`}
        />
        <button
          class="text-[11px] text-text-tertiary hover:text-text-secondary transition-colors duration-150 shrink-0"
          onClick={() => setShowMirror(!showMirror())}
          aria-label="切换镜像输入"
        >
          镜像{showMirror() ? '▲' : '▼'}
        </button>
        <button
          class={btnPrimary}
          onClick={startDownload}
          aria-label="开始下载"
        >
          开始下载
        </button>
      </div>
      <Show when={showMirror()}>
        <input
          type="text"
          value={mirrorUrl()}
          onInput={(e) => setMirrorUrl(e.currentTarget.value)}
          placeholder="镜像站 URL,例如 https://hf-mirror.com/..."
          class={`mt-1.5 w-full font-mono ${inputBase}`}
        />
      </Show>
    </div>
  )
}
