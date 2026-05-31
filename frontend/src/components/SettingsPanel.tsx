import { For, Show, createSignal, onMount } from 'solid-js'
import { api } from '../api/invoke'
import { $config, $configLoading } from '../stores/settings'
import Toggle from './Toggle'
import type { AppConfig } from '../types'

const ALGORITHMS = ['blake3', 'sha256'] as const
const MIN_SIZE_OPTIONS = [
  { value: 0, label: '无限制' },
  { value: 1048576, label: '1 MB' },
  { value: 5242880, label: '5 MB' },
  { value: 10485760, label: '10 MB' },
  { value: 52428800, label: '50 MB' },
] as const

export default function SettingsPanel() {
  const loading = () => $configLoading.get()

  const [dir, setDir] = createSignal('')
  const [maxTasks, setMaxTasks] = createSignal(3)
  const [maxFragments, setMaxFragments] = createSignal(8)
  const [maxConnections, setMaxConnections] = createSignal(4)
  const [quicEnabled, setQuicEnabled] = createSignal(false)
  const [http2Enabled, setHttp2Enabled] = createSignal(true)
  const [verifyEnabled, setVerifyEnabled] = createSignal(true)
  const [algorithm, setAlgorithm] = createSignal('blake3')
  const [minSize, setMinSize] = createSignal(0)
  const [saved, setSaved] = createSignal(false)

  onMount(async () => {
    try {
      const cfg = await api.getConfig()
      $config.set(cfg)
      setDir(cfg.download.downloadDir)
      setMaxTasks(cfg.maxConcurrentTasks)
      setMaxFragments(cfg.download.maxConcurrentFragments)
      setMaxConnections(cfg.connection.maxConnectionsPerHost)
      setQuicEnabled(cfg.connection.enableQuic)
      setVerifyEnabled(cfg.download.verifyChecksum)
    } catch {
      // Tauri 未就绪时使用默认值
    } finally {
      $configLoading.set(false)
    }
  })

  async function handleSave() {
    const cfg: Partial<AppConfig> = {
      maxConcurrentTasks: maxTasks(),
      download: {
        downloadDir: dir(),
        maxConcurrentFragments: maxFragments(),
        verifyChecksum: verifyEnabled(),
        maxRetries: 3,
        requestTimeoutSecs: 30,
        userAgent: '',
      },
      connection: {
        maxConnectionsPerHost: maxConnections(),
        enableQuic: quicEnabled(),
        maxGlobalConnections: 256,
        keepAliveTimeoutSecs: 30,
        connectTimeoutSecs: 10,
        enableHttp2: true,
      },
    }
    try {
      await api.updateConfig(cfg)
      setSaved(true)
      setTimeout(() => setSaved(false), 1500)
    } catch {
      // 静默忽略
    }
  }

  return (
    <Show
      when={!loading()}
      fallback={<div class="empty-state"><div class="empty-text">加载配置中...</div></div>}
    >
      <div>
        <div class="setting-group">
          <div class="setting-group-title">下载</div>
          <div class="setting-row">
            <span class="setting-label">下载目录</span>
            <input
              class="setting-input"
              type="text"
              value={dir()}
              onInput={(e) => setDir(e.currentTarget.value)}
            />
          </div>
          <div class="setting-row">
            <span class="setting-label">最大并发任务</span>
            <input
              class="setting-input"
              type="number"
              min="1"
              max="20"
              value={maxTasks()}
              onInput={(e) => setMaxTasks(Number(e.currentTarget.value) || 1)}
              style={{ width: '80px', 'text-align': 'center' }}
            />
          </div>
          <div class="setting-row">
            <span class="setting-label">最大分片数</span>
            <input
              class="setting-input"
              type="number"
              min="1"
              max="128"
              value={maxFragments()}
              onInput={(e) => setMaxFragments(Number(e.currentTarget.value) || 1)}
              style={{ width: '80px', 'text-align': 'center' }}
            />
          </div>
          <div class="setting-row">
            <span class="setting-label">每主机最大连接数</span>
            <input
              class="setting-input"
              type="number"
              min="1"
              max="32"
              value={maxConnections()}
              onInput={(e) => setMaxConnections(Number(e.currentTarget.value) || 1)}
              style={{ width: '80px', 'text-align': 'center' }}
            />
          </div>
          <div class="setting-row">
            <span class="setting-label">最小文件大小</span>
            <select
              class="setting-input"
              value={minSize()}
              onChange={(e) => setMinSize(Number(e.currentTarget.value))}
              style={{ width: '120px' }}
            >
              <For each={MIN_SIZE_OPTIONS}>
                {(opt) => <option value={opt.value}>{opt.label}</option>}
              </For>
            </select>
          </div>
        </div>

        <div class="setting-group">
          <div class="setting-group-title">协议</div>
          <div class="setting-row">
            <span class="setting-label">QUIC 多路径</span>
            <Toggle
              initial={quicEnabled()}
              ariaLabel="QUIC 开关"
              onChange={setQuicEnabled}
            />
          </div>
          <div class="setting-row">
            <span class="setting-label">HTTP/2</span>
            <Toggle
              initial={http2Enabled()}
              ariaLabel="HTTP/2 开关"
              onChange={setHttp2Enabled}
            />
          </div>
        </div>

        <div class="setting-group">
          <div class="setting-group-title">校验</div>
          <div class="setting-row">
            <span class="setting-label">启用校验</span>
            <Toggle
              initial={verifyEnabled()}
              ariaLabel="校验开关"
              onChange={setVerifyEnabled}
            />
          </div>
          <div class="setting-row">
            <span class="setting-label">校验算法</span>
            <select
              class="setting-input"
              value={algorithm()}
              onChange={(e) => setAlgorithm(e.currentTarget.value)}
              style={{ width: '120px' }}
            >
              <For each={ALGORITHMS}>
                {(alg) => <option value={alg}>{alg === 'blake3' ? 'Blake3' : 'SHA-256'}</option>}
              </For>
            </select>
          </div>
        </div>

        <div style={{ display: 'flex', 'align-items': 'center', gap: '12px', 'margin-top': '8px' }}>
          <button class="btn btn-primary" onClick={handleSave}>
            保存配置
          </button>
          <Show when={saved()}>
            <span style={{ color: 'var(--ok)', 'font-size': '12px', 'font-weight': 600 }}>
              已保存
            </span>
          </Show>
        </div>
      </div>
    </Show>
  )
}
