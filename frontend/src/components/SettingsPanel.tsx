import { createSignal, createEffect, For, Show, onMount, untrack } from 'solid-js'
import { api } from '../api/invoke'
import { $config, $configLoading } from '../stores/settings'
import { addToast } from '../stores/toast'
import type { AppConfig } from '../types'
import { CloseIcon } from './icons'

type SettingsTab = 'general' | 'download' | 'connection' | 'scheduler' | 'about'

interface SettingsPanelProps {
  visible: boolean
  onClose: () => void
}

export default function SettingsPanel(props: SettingsPanelProps) {
  const [activeTab, setActiveTab] = createSignal<SettingsTab>('general')
  const initialVisible = untrack(() => props.visible)
  const [shouldRender, setShouldRender] = createSignal(initialVisible)
  const [visible, setVisible] = createSignal(initialVisible)

  let closeTimer: number | null = null

  const cancelCloseTimer = () => {
    if (closeTimer !== null) {
      clearTimeout(closeTimer)
      closeTimer = null
    }
  }

  createEffect(() => {
    if (props.visible) {
      cancelCloseTimer()
      if (!shouldRender()) {
        setShouldRender(true)
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            setVisible(true)
          })
        })
      } else {
        setVisible(true)
      }
    } else if (shouldRender() && visible()) {
      setVisible(false)
      cancelCloseTimer()
      closeTimer = window.setTimeout(() => {
        setShouldRender(false)
        closeTimer = null
      }, 250)
    }
  })

  const tabs: { id: SettingsTab; label: string }[] = [
    { id: 'general', label: '通用' },
    { id: 'download', label: '下载' },
    { id: 'connection', label: '连接' },
    { id: 'scheduler', label: '调度' },
    { id: 'about', label: '关于' },
  ]

  // 真实 AppConfig 字段
  const [downloadDir, setDownloadDir] = createSignal('')
  const [maxConcurrentTasks, setMaxConcurrentTasks] = createSignal(3)
  const [maxConcurrentFragments, setMaxConcurrentFragments] = createSignal(8)
  const [maxRetries, setMaxRetries] = createSignal(3)
  const [verifyChecksum, setVerifyChecksum] = createSignal(true)
  const [userAgent, setUserAgent] = createSignal('')
  const [maxConnectionsPerHost, setMaxConnectionsPerHost] = createSignal(4)
  const [enableHttp2, setEnableHttp2] = createSignal(true)
  const [enableQuic, setEnableQuic] = createSignal(false)
  const [connectTimeoutSecs, setConnectTimeoutSecs] = createSignal(30)
  const [minFragmentSize, setMinFragmentSize] = createSignal(1048576)
  const [maxFragmentSize, setMaxFragmentSize] = createSignal(67108864)
  const [ewmaAlpha, setEwmaAlpha] = createSignal(0.3)
  const [saving, setSaving] = createSignal(false)

  const applyConfig = (cfg: AppConfig) => {
    setMaxConcurrentTasks(cfg.maxConcurrentTasks)
    setDownloadDir(cfg.download.downloadDir)
    setMaxConcurrentFragments(cfg.download.maxConcurrentFragments)
    setMaxRetries(cfg.download.maxRetries)
    setVerifyChecksum(cfg.download.verifyChecksum)
    setUserAgent(cfg.download.userAgent)
    setMaxConnectionsPerHost(cfg.connection.maxConnectionsPerHost)
    setEnableHttp2(cfg.connection.enableHttp2)
    setEnableQuic(cfg.connection.enableQuic)
    setConnectTimeoutSecs(cfg.connection.connectTimeoutSecs)
    setMinFragmentSize(cfg.scheduler.minFragmentSize)
    setMaxFragmentSize(cfg.scheduler.maxFragmentSize)
    setEwmaAlpha(cfg.scheduler.ewmaAlpha)
  }

  onMount(async () => {
    $configLoading.set(true)
    try {
      const cfg = await api.getConfig()
      $config.set(cfg)
      applyConfig(cfg)
    } catch (e) {
      addToast('加载配置失败: ' + String(e), 'error')
    } finally {
      $configLoading.set(false)
    }
  })

  const buildConfig = (): AppConfig | null => {
    const base = $config.get()
    if (!base) return null

    return {
      maxConcurrentTasks: maxConcurrentTasks(),
      download: {
        ...base.download,
        downloadDir: downloadDir(),
        maxConcurrentFragments: maxConcurrentFragments(),
        maxRetries: maxRetries(),
        verifyChecksum: verifyChecksum(),
        userAgent: userAgent(),
      },
      connection: {
        ...base.connection,
        maxConnectionsPerHost: maxConnectionsPerHost(),
        enableHttp2: enableHttp2(),
        enableQuic: enableQuic(),
        connectTimeoutSecs: connectTimeoutSecs(),
      },
      scheduler: {
        ...base.scheduler,
        minFragmentSize: minFragmentSize(),
        maxFragmentSize: maxFragmentSize(),
        ewmaAlpha: ewmaAlpha(),
      },
    }
  }

  const handleSave = async () => {
    const cfg = buildConfig()
    if (!cfg) {
      addToast('配置尚未加载', 'error')
      return
    }

    setSaving(true)
    try {
      await api.updateConfig(cfg)
      $config.set(cfg)
      addToast('配置已保存', 'success')
    } catch (e) {
      addToast('保存配置失败: ' + String(e), 'error')
    } finally {
      setSaving(false)
    }
  }

  const handleChooseDownloadDir = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog')
      const selected = await open({ directory: true, multiple: false })
      if (typeof selected === 'string') {
        setDownloadDir(selected)
      }
    } catch (e) {
      addToast('无法打开目录选择器: ' + String(e), 'error')
    }
  }

  return (
    <Show when={shouldRender()}>
      {/* Overlay */}
      <div
        class="panel-overlay"
        style={{
          opacity: visible() ? 1 : 0,
          transition: 'opacity 250ms ease',
        }}
        onClick={() => props.onClose()}
      />

      {/* Panel */}
      <div
        class="fixed z-[210]"
        style={{
          top: '50%',
          left: '50%',
          transform: `translate(-50%, -50%) scale(${visible() ? 1 : 0.95})`,
          opacity: visible() ? 1 : 0,
          transition: 'transform 250ms cubic-bezier(0.32, 0.72, 0, 1), opacity 250ms ease',
          width: '640px',
          height: '520px',
          background: 'rgba(18, 18, 26, 0.98)',
          'backdrop-filter': 'blur(20px) saturate(1.5)',
          'border-radius': '16px',
          border: '1px solid rgba(255, 255, 255, 0.08)',
          'box-shadow': '0 16px 48px rgba(0, 0, 0, 0.5)',
          display: 'flex',
          overflow: 'hidden',
        }}
      >
        {/* Sidebar */}
        <div style={{
          width: '160px',
          background: 'rgba(255, 255, 255, 0.02)',
          'border-right': '1px solid rgba(255, 255, 255, 0.05)',
          padding: '16px 8px',
          display: 'flex',
          'flex-direction': 'column',
          gap: '2px',
        }}>
          <For each={tabs}>
            {(tab) => (
              <button
                class="text-left"
                style={{
                  padding: '8px 12px',
                  'border-radius': '6px',
                  'font-size': '13px',
                  background: activeTab() === tab.id ? 'rgba(0, 212, 170, 0.1)' : 'transparent',
                  color: activeTab() === tab.id ? '#00D4AA' : '#A0A0B0',
                  border: 'none',
                  cursor: 'pointer',
                  transition: 'all 150ms ease',
                  'font-weight': activeTab() === tab.id ? 600 : 400,
                }}
                onClick={() => setActiveTab(tab.id)}
              >
                {tab.label}
              </button>
            )}
          </For>
        </div>

        {/* Content */}
        <div class="flex-1 flex flex-col" style={{ overflow: 'hidden' }}>
          {/* Header */}
          <div class="panel-header">
            <span style={{ 'font-size': '15px', 'font-weight': 600, color: '#F0F0F5' }}>
              {tabs.find(t => t.id === activeTab())?.label}设置
            </span>
            <button
              class="icon-btn-sm hover-light"
              onClick={() => props.onClose()}
            >
              <CloseIcon />
            </button>
          </div>

          {/* Scrollable content */}
          <div class="flex-1 overflow-y-auto" style={{ padding: '20px' }}>
            <Show
              when={!$configLoading.get()}
              fallback={<div style={{ color: '#A0A0B0', 'font-size': '14px' }}>加载配置中...</div>}
            >
              <Show when={activeTab() === 'general'}>
                <div class="flex flex-col gap-5">
                  <div>
                    <div style={{ 'font-size': '13px', color: '#A0A0B0', 'margin-bottom': '8px' }}>默认下载路径</div>
                    <div class="flex items-center gap-2">
                      <input
                        type="text"
                        class="input flex-1"
                        value={downloadDir()}
                        onInput={e => setDownloadDir(e.currentTarget.value)}
                        style={{ 'font-size': '13px' }}
                      />
                      <button
                        class="btn btn-secondary"
                        style={{ 'font-size': '12px', padding: '6px 12px' }}
                        onClick={handleChooseDownloadDir}
                      >
                        浏览
                      </button>
                    </div>
                  </div>
                </div>
              </Show>

              <Show when={activeTab() === 'download'}>
                <div class="flex flex-col gap-5">
                  <SliderItem
                    label="最大并发任务数"
                    value={maxConcurrentTasks()}
                    min={1}
                    max={16}
                    onChange={setMaxConcurrentTasks}
                    displayValue={`${maxConcurrentTasks()}`}
                  />
                  <SliderItem
                    label="最大并发分片数"
                    value={maxConcurrentFragments()}
                    min={1}
                    max={32}
                    onChange={setMaxConcurrentFragments}
                    displayValue={`${maxConcurrentFragments()}`}
                  />
                  <SliderItem
                    label="最大重试次数"
                    value={maxRetries()}
                    min={0}
                    max={10}
                    onChange={setMaxRetries}
                    displayValue={`${maxRetries()} 次`}
                  />
                  <ToggleItem label="校验文件完整性" value={verifyChecksum()} onChange={setVerifyChecksum} />
                  <div>
                    <div style={{ 'font-size': '13px', color: '#A0A0B0', 'margin-bottom': '8px' }}>User-Agent</div>
                    <input
                      type="text"
                      class="input"
                      value={userAgent()}
                      onInput={e => setUserAgent(e.currentTarget.value)}
                      placeholder="默认 User-Agent"
                      style={{ width: '100%', 'font-size': '13px' }}
                    />
                  </div>
                </div>
              </Show>

              <Show when={activeTab() === 'connection'}>
                <div class="flex flex-col gap-5">
                  <SliderItem
                    label="每个主机最大连接数"
                    value={maxConnectionsPerHost()}
                    min={1}
                    max={16}
                    onChange={setMaxConnectionsPerHost}
                    displayValue={`${maxConnectionsPerHost()}`}
                  />
                  <SliderItem
                    label="连接超时"
                    value={connectTimeoutSecs()}
                    min={5}
                    max={120}
                    onChange={setConnectTimeoutSecs}
                    displayValue={`${connectTimeoutSecs()} 秒`}
                  />
                  <ToggleItem label="启用 HTTP/2" value={enableHttp2()} onChange={setEnableHttp2} />
                  <ToggleItem label="启用 QUIC" value={enableQuic()} onChange={setEnableQuic} />
                </div>
              </Show>

              <Show when={activeTab() === 'scheduler'}>
                <div class="flex flex-col gap-5">
                  <SliderItem
                    label="最小分片大小"
                    value={minFragmentSize()}
                    min={262144}
                    max={10485760}
                    onChange={setMinFragmentSize}
                    displayValue={`${(minFragmentSize() / 1048576).toFixed(1)} MB`}
                  />
                  <SliderItem
                    label="最大分片大小"
                    value={maxFragmentSize()}
                    min={10485760}
                    max={134217728}
                    onChange={setMaxFragmentSize}
                    displayValue={`${(maxFragmentSize() / 1048576).toFixed(0)} MB`}
                  />
                  <SliderItem
                    label="EWMA 平滑系数"
                    value={ewmaAlpha()}
                    min={0.1}
                    max={0.9}
                    onChange={setEwmaAlpha}
                    displayValue={ewmaAlpha().toFixed(2)}
                  />
                </div>
              </Show>

              <Show when={activeTab() === 'about'}>
                <div class="flex flex-col items-center gap-3" style={{ padding: '40px 20px' }}>
                  <div style={{
                    width: '48px',
                    height: '48px',
                    background: 'linear-gradient(135deg, #00D4AA 0%, #00B4D8 100%)',
                    'border-radius': '12px',
                    display: 'flex',
                    'align-items': 'center',
                    'justify-content': 'center',
                    color: '#0A0A0F',
                    'font-size': '24px',
                    'font-weight': 700,
                  }}>
                    T
                  </div>
                  <div style={{ 'font-size': '18px', 'font-weight': 600, color: '#F0F0F5' }}>Tachyon</div>
                  <div style={{ 'font-size': '13px', color: '#6B7280' }}>版本 0.1.0 · Rust + Tauri</div>
                  <div style={{ 'font-size': '12px', color: '#4A4A5A', 'margin-top': '8px' }}>
                    高性能多线程下载加速器
                  </div>
                </div>
              </Show>
            </Show>
          </div>

          <div
            class="flex items-center justify-end gap-2"
            style={{ padding: '12px 20px', 'border-top': '1px solid rgba(255, 255, 255, 0.05)' }}
          >
            <button
              class="btn btn-secondary"
              style={{ 'font-size': '13px', padding: '7px 14px' }}
              onClick={() => props.onClose()}
            >
              取消
            </button>
            <button
              class="btn-primary"
              style={{ 'font-size': '13px', padding: '7px 14px' }}
              disabled={$configLoading.get() || saving()}
              onClick={handleSave}
            >
              {saving() ? '保存中...' : '保存配置'}
            </button>
          </div>
        </div>
      </div>
    </Show>
  )
}

function ToggleItem(props: { label: string; value: boolean; onChange: (v: boolean) => void }) {
  return (
    <div class="flex items-center justify-between">
      <span style={{ 'font-size': '13px', color: '#F0F0F5' }}>{props.label}</span>
      <button
        class="relative"
        style={{
          width: '40px',
          height: '22px',
          'border-radius': '11px',
          background: props.value ? '#00D4AA' : '#2A2A3A',
          border: 'none',
          cursor: 'pointer',
          transition: 'background 200ms ease',
        }}
        onClick={() => props.onChange(!props.value)}
      >
        <div
          style={{
            position: 'absolute',
            width: '18px',
            height: '18px',
            'border-radius': '50%',
            background: 'white',
            top: '2px',
            left: '2px',
            transform: props.value ? 'translateX(18px)' : 'translateX(0)',
            transition: 'transform 200ms cubic-bezier(0.32, 0.72, 0, 1)',
          }}
        />
      </button>
    </div>
  )
}

function SliderItem(props: {
  label: string
  value: number
  min: number
  max: number
  onChange: (v: number) => void
  displayValue: string
}) {
  return (
    <div>
      <div class="flex items-center justify-between" style={{ 'margin-bottom': '8px' }}>
        <span style={{ 'font-size': '13px', color: '#A0A0B0' }}>{props.label}</span>
        <span class="mono" style={{ 'font-size': '13px', color: '#F0F0F5' }}>
          {props.displayValue}
        </span>
      </div>
      <input
        type="range"
        aria-label={props.label}
        min={props.min}
        max={props.max}
        value={props.value}
        onInput={e => props.onChange(parseInt(e.currentTarget.value))}
        style={{ width: '100%' }}
      />
    </div>
  )
}
