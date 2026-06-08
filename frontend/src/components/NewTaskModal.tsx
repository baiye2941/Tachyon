import { createSignal } from 'solid-js'
import { CloseIcon, FolderOpenIcon, PlusIcon } from './icons'
import { api } from '../api/invoke'
import { addToast } from '../stores/toast'

interface NewTaskModalProps {
  onClose: () => void
}

export default function NewTaskModal(props: NewTaskModalProps) {
  const [url, setUrl] = createSignal('')
  const [savePath, setSavePath] = createSignal('')
  const [autoStart, setAutoStart] = createSignal(true)
  const [isDragOver, setIsDragOver] = createSignal(false)

  const handleBrowse = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog')
      const selected = await open({ directory: true, multiple: false })
      if (selected) {
        setSavePath(selected as string)
      }
    } catch (err) {
      console.warn('文件夹选择不可用（浏览器或 SSR 环境）:', err)
    }
  }

  const handleSubmit = async () => {
    const urlValue = url().trim()
    if (!urlValue) {
      addToast('请输入下载链接', 'error')
      return
    }

    try {
      await api.createTask(urlValue)
      addToast('任务已创建', 'success')
      setUrl('')
      setSavePath('')
      setAutoStart(true)
      props.onClose()
    } catch (err) {
      console.error('创建任务失败:', err)
      addToast(`创建任务失败: ${err}`, 'error')
    }
  }

  return (
    <div
      class="fixed inset-0 z-[200] flex items-center justify-center"
      style={{
        background: 'rgba(0, 0, 0, 0.6)',
        'backdrop-filter': 'blur(4px)',
      }}
      onClick={() => props.onClose()}
    >
      <div
        class="glass-strong"
        style={{
          width: '480px',
          'border-radius': '16px',
          padding: '24px',
          'box-shadow': '0 16px 48px rgba(0, 0, 0, 0.6), 0 0 20px rgba(0, 212, 170, 0.15)',
          animation: 'toast-in 300ms cubic-bezier(0.32, 0.72, 0, 1)',
        }}
        onClick={e => e.stopPropagation()}
      >
        {/* Header */}
        <div class="flex items-center justify-between" style={{ 'margin-bottom': '20px' }}>
          <span
            style={{
              'font-size': '18px',
              'font-weight': 600,
              color: '#F0F0F5',
            }}
          >
            添加下载任务
          </span>
          <button
            class="icon-btn-sm"
            style={{
              width: '28px',
              height: '28px',
              color: '#6B7280',
              background: 'none',
              border: 'none',
              cursor: 'pointer',
            }}
            onClick={() => props.onClose()}
          >
            <CloseIcon />
          </button>
        </div>

        {/* URL Input */}
        <div style={{ 'margin-bottom': '16px' }}>
          <label
            style={{
              display: 'block',
              'font-size': '12px',
              'font-weight': 500,
              color: '#A0A0B0',
              'margin-bottom': '6px',
            }}
          >
            下载链接
          </label>
          <input
            type="text"
            placeholder="粘贴或拖拽下载链接到此处"
            value={url()}
            onInput={e => setUrl(e.currentTarget.value)}
            class={`input${isDragOver() ? ' input-drag-over' : ''}`}
            style={{
              width: '100%',
              padding: '10px 12px',
              'font-size': '14px',
            }}
            onDragOver={e => { e.preventDefault(); setIsDragOver(true) }}
            onDragLeave={() => setIsDragOver(false)}
            onDrop={e => {
              e.preventDefault()
              setIsDragOver(false)
              const text = e.dataTransfer?.getData('text') || e.dataTransfer?.getData('text/uri-list') || ''
              if (text) setUrl(text.trim())
            }}
          />
        </div>

        {/* Save Path */}
        <div style={{ 'margin-bottom': '16px' }}>
          <label
            style={{
              display: 'block',
              'font-size': '12px',
              'font-weight': 500,
              color: '#A0A0B0',
              'margin-bottom': '6px',
            }}
          >
            保存到
          </label>
          <div class="flex items-center gap-2">
            <input
              type="text"
              placeholder="默认下载目录"
              value={savePath()}
              onInput={e => setSavePath(e.currentTarget.value)}
              class="input"
              style={{
                flex: 1,
                padding: '10px 12px',
                'font-size': '14px',
              }}
            />
            <button
              class="btn-secondary flex items-center gap-1 flex-shrink-0"
              style={{
                padding: '10px 12px',
                'border-radius': '8px',
                'font-size': '13px',
              }}
              onClick={handleBrowse}
            >
              <FolderOpenIcon />
              <span>浏览</span>
            </button>
          </div>
        </div>

        {/* Auto Start */}
        <div
          class="flex items-center gap-2 cursor-pointer"
          style={{ 'margin-bottom': '24px' }}
          onClick={() => setAutoStart(v => !v)}
        >
          <div
            style={{
              width: '18px',
              height: '18px',
              'border-radius': '4px',
              border: autoStart() ? 'none' : '1px solid rgba(255,255,255,0.3)',
              background: autoStart() ? '#00D4AA' : 'transparent',
              display: 'flex',
              'align-items': 'center',
              'justify-content': 'center',
              transition: 'all 150ms ease',
            }}
          >
            {autoStart() && (
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#0A0A0F" stroke-width="3" stroke-linecap="round" stroke-linejoin="round">
                <polyline points="20 6 9 17 4 12" />
              </svg>
            )}
          </div>
          <span style={{ 'font-size': '14px', color: '#A0A0B0' }}>
            自动开始下载
          </span>
        </div>

        {/* Actions */}
        <div class="flex items-center justify-end gap-3">
          <button
            class="hover-light"
            style={{
              padding: '8px 16px',
              'border-radius': '8px',
              'font-size': '14px',
              color: '#A0A0B0',
              background: 'none',
              border: 'none',
              cursor: 'pointer',
            }}
            onClick={() => props.onClose()}
          >
            取消
          </button>
          <button
            class="btn-primary hover-lift"
            style={{
              padding: '8px 20px',
              'border-radius': '8px',
              'font-size': '14px',
              'font-weight': 600,
              color: '#0A0A0F',
              background: 'linear-gradient(135deg, #00D4AA 0%, #00B4D8 100%)',
              border: 'none',
              cursor: 'pointer',
              display: 'flex',
              'align-items': 'center',
              gap: '6px',
            }}
            onClick={handleSubmit}
          >
            <PlusIcon />
            <span>开始下载</span>
          </button>
        </div>
      </div>
    </div>
  )
}
