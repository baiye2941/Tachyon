import { createSignal } from 'solid-js'
import { api } from '../api/invoke'
import { $tasks, refreshTaskList } from '../stores/downloads'
import { addToast } from '../stores/toast'

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

  async function pauseAll() {
    const active = $tasks.get().filter(t => t.status === 'downloading' || t.status === 'pending')
    await Promise.allSettled(active.map(t => api.pauseTask(t.id).catch(() => {})))
    refreshTaskList()
  }

  return (
    <div class="flex flex-col gap-2.5 px-5 py-3 border-b border-white/6">
      <div class="flex-1 flex gap-2">
        <input
          type="text"
          value={url()}
          onInput={(e) => setUrl(e.currentTarget.value)}
          onKeyDown={(e) => { if (e.key === 'Enter') startDownload() }}
          placeholder="粘贴下载链接,支持 HTTP/HTTPS"
          aria-label="下载链接输入"
          class="flex-1 px-3 py-2 bg-surface border border-white/6 rounded text-[13px] font-mono text-text-primary placeholder:text-text-tertiary placeholder:font-sans outline-none focus:border-accent transition-colors duration-150"
        />
        <button
          class="px-4 py-2 bg-accent text-canvas text-[12px] font-semibold rounded hover:opacity-85 active:scale-[0.98] transition-all duration-100"
          onClick={startDownload}
          aria-label="开始下载"
        >
          开始下载
        </button>
      </div>
      <div class="flex items-center gap-2">
        <button
          class="text-[11px] text-text-tertiary hover:text-text-secondary transition-colors"
          onClick={() => setShowMirror(!showMirror())}
        >
          {showMirror() ? '隐藏镜像' : '使用镜像站'}
        </button>
        {showMirror() && (
          <input
            type="text"
            value={mirrorUrl()}
            onInput={(e) => setMirrorUrl(e.currentTarget.value)}
            placeholder="镜像站 URL,例如 https://hf-mirror.com/..."
            class="flex-1 px-3 py-1.5 bg-surface border border-white/6 rounded text-[12px] font-mono text-text-primary placeholder:text-text-tertiary outline-none focus:border-accent transition-colors duration-150"
          />
        )}
      </div>
      <button
        class="px-3 py-2 text-[12px] font-semibold text-text-secondary border border-white/6 rounded hover:text-text-primary hover:border-white/12 active:scale-[0.98] transition-all duration-100 self-start"
        onClick={pauseAll}
        aria-label="暂停所有下载任务"
      >
        全部暂停
      </button>
    </div>
  )
}
